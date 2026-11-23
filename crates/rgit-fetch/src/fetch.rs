// SPDX-License-Identifier: MIT
//! `git fetch` from one remote: the whole sequence on top of the pieces in
//! the other modules.

use bstr::{BString, ByteSlice};
use rgit_hash::ObjectId;
use rgit_protocol::v2::RemoteRef;
use rgit_refs::{RefStore, RefUpdate};
use rgit_remote::{BranchConfig, Direction, Refspec, RefspecSet, Remote, TagOpt};

use crate::display::{self, display_url};
use crate::refmap::{self, Entry, Origin};
use crate::repo::LocalRepo;
use crate::session::{ConnectSettings, PackOptions, ReceivedPack, Session};
use crate::update::{self, RefLine, Status};
use crate::{Error, Result, graph, messages, shallow};

/// What to fetch and how (the command-line options of `git fetch`).
#[derive(Debug, Clone, Default)]
pub struct FetchOptions {
    /// Command-line refspecs; empty uses `remote.<name>.fetch`.
    pub refspecs: Vec<String>,
    /// `--tags` / `--no-tags`; `None` uses `remote.<name>.tagOpt`.
    pub tags: Option<TagOpt>,
    /// `--prune` / `--no-prune`; `None` uses `remote.<name>.prune`, then
    /// `fetch.prune`.
    pub prune: Option<bool>,
    /// `--force`: allow non-fast-forward updates for every ref.
    pub force: bool,
    /// `--dry-run`: change no ref and no `FETCH_HEAD`.
    pub dry_run: bool,
    /// `--update-head-ok`.
    pub update_head_ok: bool,
    /// Write `FETCH_HEAD` (`--no-write-fetch-head` clears it).
    pub write_fetch_head: bool,
    /// `--append`: add to `FETCH_HEAD` instead of replacing it.
    pub append: bool,
    /// Reflog action (`fetch <args>` or `$GIT_REFLOG_ACTION`).
    pub reflog_action: String,
    /// Ask the server not to send progress.
    pub no_progress: bool,
    /// `-o` / `--server-option`.
    pub server_options: Vec<String>,
    /// `-k` / `--keep`: always keep the received pack.
    pub keep: bool,
    /// `--refmap`: replaces `remote.<name>.fetch` for the opportunistic
    /// updates of command-line refspecs (`Some(vec![])` for `--refmap=`).
    pub refmap: Option<Vec<String>>,
    /// `--prune-tags` / `--no-prune-tags`; `None` uses
    /// `remote.<name>.pruneTags`, then `fetch.pruneTags`.
    pub prune_tags: Option<bool>,
    /// Branches checked out (or being rebased or bisected) in a worktree,
    /// with the worktree's path: fetching into them is refused unless
    /// `update_head_ok`.
    pub checked_out: Vec<(String, std::path::PathBuf)>,
    /// `--refetch`: tell the server about no objects we have.
    pub refetch: bool,
    /// `--atomic`: update the refs in one transaction, all or none.
    pub atomic: bool,
    /// `--negotiation-restrict` / `--negotiation-tip` (and
    /// `--negotiation-include`): the commits the negotiation starts from;
    /// `None` uses every local ref.
    pub negotiation_tips: Option<Vec<ObjectId>>,
    /// `--depth` (`deepen <n>`), `--deepen` (with
    /// [`deepen_relative`](Self::deepen_relative)) or `--unshallow`
    /// (`deepen 2147483647`).
    pub deepen: Option<u32>,
    /// `--deepen`: the depth counts from the current shallow boundary.
    pub deepen_relative: bool,
    /// `--shallow-since` as a timestamp.
    pub deepen_since: Option<i64>,
    /// `--shallow-exclude` revisions.
    pub deepen_not: Vec<String>,
    /// `--update-shallow`: accept refs whose history the server cut.
    pub update_shallow: bool,
}

impl FetchOptions {
    fn deepens(&self) -> bool {
        self.deepen.is_some() || self.deepen_since.is_some() || !self.deepen_not.is_empty()
    }
}

/// `fetch.unpackLimit`, else `transfer.unpackLimit`, else 100; 0 or less
/// means every pack is unpacked.
fn unpack_limit(config: &rgit_config::ConfigSet) -> u32 {
    let n = config
        .int("fetch.unpackLimit")
        .ok()
        .flatten()
        .or_else(|| config.int("transfer.unpackLimit").ok().flatten())
        .unwrap_or(100);
    u32::try_from(n).ok().filter(|&n| n > 0).unwrap_or(u32::MAX)
}

/// The result of [`fetch`].
#[derive(Debug, Clone, Default)]
pub struct FetchResult {
    /// The URL as shown (`From <url>`).
    pub url: String,
    /// One line per ref, in display order (pruned refs first).
    pub lines: Vec<RefLine>,
    /// Messages to print with `error: ` before the summary.
    pub errors: Vec<String>,
    /// Messages to print with `warning: ` before the errors.
    pub warnings: Vec<String>,
    /// The final ref map (what `FETCH_HEAD` lists).
    pub entries: Vec<Entry>,
    /// The `followRemoteHEAD` warning (printed on standard output).
    pub head_warning: Option<HeadWarning>,
}

impl FetchResult {
    /// Exit code of `git fetch`: 1 if any ref was rejected or failed.
    pub fn exit_code(&self) -> i32 {
        if self.lines.iter().any(|l| l.status.is_error()) {
            1
        } else {
            0
        }
    }
}

fn parse_spec(s: &str) -> Result<Refspec> {
    Refspec::parse(s.as_bytes(), Direction::Fetch).map_err(|e| Error::fatal(e.to_string()))
}

/// `remote.<name>.followRemoteHEAD` (or `fetch.followRemoteHEAD`).
fn follow_remote_head(repo: &LocalRepo, name: &[u8]) -> String {
    let key = format!("remote.{}.followremotehead", name.to_str_lossy());
    let v = repo
        .config
        .string(&key)
        .ok()
        .flatten()
        .or_else(|| repo.config.string("fetch.followremotehead").ok().flatten());
    v.map(|v| v.to_string().to_ascii_lowercase())
        .unwrap_or_else(|| "create".into())
}

/// Fetch from `remote` into `repo`: see the crate documentation.
pub fn fetch(
    repo: &LocalRepo,
    remote: &Remote,
    opts: &FetchOptions,
    connect: &ConnectSettings<'_>,
) -> Result<FetchResult> {
    let Some(url) = remote.fetch_url() else {
        return Err(Error::fatal(messages::NO_URL));
    };
    let url = url.to_str_lossy().into_owned();
    let shown_url = display_url(&url);
    let cmdline = !opts.refspecs.is_empty();
    let tag_opt = opts.tags.unwrap_or(remote.tag_opt);

    let mut specs = if cmdline {
        let mut s = RefspecSet::default();
        let mut it = opts.refspecs.iter();
        while let Some(r) = it.next() {
            if r == "tag" {
                let Some(t) = it.next() else {
                    return Err(Error::fatal(messages::NEED_TAG_NAME));
                };
                s.items
                    .push(parse_spec(&format!("refs/tags/{t}:refs/tags/{t}"))?);
            } else {
                s.items.push(parse_spec(r)?);
            }
        }
        s
    } else {
        remote.fetch.clone()
    };
    let prune = opts.prune.or(remote.prune).unwrap_or_else(|| {
        repo.config
            .bool("fetch.prune")
            .ok()
            .flatten()
            .unwrap_or(false)
    });
    // `--prune-tags` adds the tag refspec, for fetching and pruning, when
    // no refspec is given on the command line.
    let prune_tags = prune
        && !cmdline
        && opts.prune_tags.or(remote.prune_tags).unwrap_or_else(|| {
            repo.config
                .bool("fetch.prunetags")
                .ok()
                .flatten()
                .unwrap_or(false)
        });
    let tag_spec = || parse_spec("refs/tags/*:refs/tags/*");
    if prune_tags {
        specs.items.push(tag_spec()?);
    }
    // What `--prune` compares with: without the tags of `--tags`.
    let prune_specs = specs.clone();
    if tag_opt == TagOpt::All && !prune_tags {
        specs.items.push(tag_spec()?);
    }

    // Merge candidates of the current branch (also when its remote is a
    // URL such as `.`).
    let mut merge_refs: Vec<BString> = Vec::new();
    if !cmdline {
        if let Some(branch) = repo.current_branch() {
            let short = &branch["refs/heads/".len()..];
            if let Ok(bc) = BranchConfig::from_config(&repo.config, short.as_bytes()) {
                if bc.remote.as_ref().map(|r| r.as_slice()) == Some(remote.name.as_slice()) {
                    merge_refs = bc.merge;
                }
            }
        }
    }
    // Nothing to fetch configured: the merge refs of the current branch,
    // else the remote's HEAD.
    if specs.positive().next().is_none() && merge_refs.is_empty() {
        specs.items.push(parse_spec("HEAD")?);
    }
    let head_mode = follow_remote_head(repo, &remote.name);
    let follow_head = !cmdline && remote.is_configured() && head_mode != "never";

    let mut extra: Vec<String> = merge_refs
        .iter()
        .map(|m| m.to_str_lossy().into_owned())
        .collect();
    if tag_opt != TagOpt::NoTags {
        extra.push("refs/tags/".into());
    }
    if follow_head {
        extra.push("HEAD".into());
    }
    let prefixes = refmap::ref_prefixes(&specs, &extra);

    // FETCH_HEAD is emptied before connecting (git leaves an empty file
    // behind when the fetch fails).
    let fetch_head = repo.git_dir.join("FETCH_HEAD");
    if opts.write_fetch_head && !opts.dry_run && !opts.append {
        std::fs::write(&fetch_head, "").map_err(|e| {
            Error::fatal(messages::cannot_open_fetch_head(
                &fetch_head,
                &rgit_transport_strerror(&e),
            ))
        })?;
    }

    let mut session = Session::connect(&url, connect, "fetch")?;
    let refs = session.ls_refs(&prefixes)?;

    let mut result = FetchResult {
        url: shown_url,
        ..FetchResult::default()
    };
    let mut entries = refmap::map_refspecs(&specs, &refs, repo.hash, &mut result.errors)?;
    if cmdline {
        for e in &mut entries {
            e.for_merge = true;
        }
    } else if !merge_refs.is_empty() {
        for e in &mut entries {
            e.for_merge = merge_refs.contains(&e.remote);
        }
        for m in &merge_refs {
            if entries.iter().any(|e| &e.remote == m) {
                continue;
            }
            if let Some(oid) = refmap::advertised(&refs, m).and_then(|r| r.oid) {
                entries.push(Entry {
                    remote: m.clone(),
                    oid,
                    local: None,
                    force: false,
                    origin: Origin::Merge,
                    for_merge: true,
                });
            }
        }
    } else if let (Some(first), Some(e)) = (specs.items.first(), entries.first_mut()) {
        if !first.pattern && !first.negative && first.src_matches(&e.remote) {
            e.for_merge = true;
        }
    }
    // Merge candidates come first (in the summary as in FETCH_HEAD).
    let (mut entries, rest): (Vec<Entry>, Vec<Entry>) =
        entries.into_iter().partition(|e| e.for_merge);
    entries.extend(rest);
    let mut entries = refmap::dedup(entries)?;

    // Never move a branch checked out in a worktree (or one a rebase or
    // bisect works on). Without the list from the caller: the current
    // branch of this work tree.
    if !opts.update_head_ok {
        let mut busy = opts.checked_out.clone();
        if busy.is_empty() && !repo.is_bare() {
            if let (Some(cur), Some(wt)) = (repo.current_branch(), &repo.work_tree) {
                busy.push((cur, wt.clone()));
            }
        }
        for e in &entries {
            let Some(local) = &e.local else { continue };
            if let Some((name, wt)) = busy.iter().find(|(b, _)| b.as_bytes() == local.as_slice()) {
                return Err(Error::fatal(messages::refusing_to_fetch_into(name, wt)));
            }
        }
    }

    // One `want` per ref whose object is missing, duplicates included
    // (as git sends them); deepening asks for every ref.
    let deepens = opts.deepens();
    let mut wants: Vec<ObjectId> = Vec::new();
    for e in &entries {
        if deepens || opts.refetch || !repo.has_object(&e.oid) {
            wants.push(e.oid);
        }
    }
    let follow_tags = tag_opt == TagOpt::Auto
        && entries
            .iter()
            .any(|e| e.origin == Origin::Refspec && e.local.is_some());
    // Order (as observed from git): refspec entries, tags pointing at
    // wanted or present objects, opportunistic remote-tracking updates,
    // then tags whose target only arrived with the pack.
    if follow_tags {
        let mut tags = Vec::new();
        add_followed_tags(repo, &refs, &entries, &mut tags, &mut wants, false);
        entries.extend(tags);
    }
    if cmdline {
        // `--refmap` replaces the configured refspecs here.
        let map = match &opts.refmap {
            Some(specs) => {
                let mut set = RefspecSet::default();
                for s in specs.iter().filter(|s| !s.is_empty()) {
                    set.items.push(parse_spec(s)?);
                }
                Some(set)
            }
            None => remote.is_configured().then(|| remote.fetch.clone()),
        };
        if let Some(map) = map {
            let extra = refmap::opportunistic(&entries, &map);
            entries.extend(extra);
        }
    }

    let advertised: Vec<ObjectId> = refs.iter().filter_map(|r| r.oid).collect();
    let pack_opts = PackOptions {
        no_progress: opts.no_progress,
        include_tag: follow_tags,
        server_options: opts.server_options.clone(),
        unpack_limit: (!opts.keep).then(|| unpack_limit(&repo.config)),
        deepen: opts.deepen,
        deepen_relative: opts.deepen_relative,
        deepen_since: opts.deepen_since,
        deepen_not: opts.deepen_not.clone(),
        tips: opts.negotiation_tips.clone(),
    };
    let mut packs: Vec<ReceivedPack> = Vec::new();
    // `--refetch`: no negotiation, everything is sent again.
    let haves = (!opts.refetch).then_some(advertised.as_slice());
    if let Some(p) = session.fetch_pack(repo, &wants, haves, &pack_opts)? {
        packs.push(p);
    }
    if follow_tags {
        // Tags whose target arrived with the pack; fetch tag objects the
        // server did not include.
        // With --dry-run the first tags were not stored, so this second
        // look lists them once more, as with git.
        let mut more = Vec::new();
        let mut tags = Vec::new();
        let seen: &[Entry] = if opts.dry_run { &[] } else { &entries };
        add_followed_tags(repo, &refs, seen, &mut tags, &mut more, true);
        entries.extend(tags);
        let tag_opts = PackOptions {
            deepen: None,
            deepen_relative: false,
            deepen_since: None,
            deepen_not: Vec::new(),
            ..pack_opts.clone()
        };
        if let Some(p) = session.fetch_pack(repo, &more, Some(&advertised), &tag_opts)? {
            packs.push(p);
        }
    }
    let shallow_info = session.take_shallow_info();
    session.close()?;

    let received: std::collections::HashSet<ObjectId> = packs
        .iter()
        .flat_map(|p| p.objects.iter().copied())
        .collect();
    // The shallow boundary: deepening (and --update-shallow) takes the
    // server's word; otherwise refs whose new history the server cut are
    // left out and the boundary stays.
    let current = repo.shallow();
    let mut new_shallow = None;
    if let Some(info) = &shallow_info {
        if deepens || opts.update_shallow {
            new_shallow = Some(shallow::apply(&current, info));
        } else {
            let cut: std::collections::HashSet<ObjectId> = info
                .shallow
                .iter()
                .filter(|id| !current.contains(id))
                .copied()
                .collect();
            if !cut.is_empty() {
                let mut kept = Vec::new();
                for e in entries {
                    if graph::reaches_any(&repo.odb, &e.oid, &received, &cut) {
                        let name = e.local.as_ref().unwrap_or(&e.remote);
                        result
                            .warnings
                            .push(messages::rejected_shallow_root(&name.to_str_lossy()));
                    } else {
                        kept.push(e);
                    }
                }
                entries = kept;
            }
        }
    }
    let boundary: std::collections::HashSet<ObjectId> = match &new_shallow {
        Some(set) => set.iter().copied().collect(),
        None => current.iter().copied().collect(),
    };
    let mut tips: Vec<ObjectId> = entries.iter().map(|e| e.oid).collect();
    // Commits that stop being shallow need their parents now.
    let unshallowed: Vec<ObjectId> = match (&new_shallow, &shallow_info) {
        (Some(_), Some(info)) => info.unshallow.clone(),
        _ => Vec::new(),
    };
    for id in &unshallowed {
        tips.extend(
            graph::commit_info(&repo.odb, id)
                .map(|c| c.parents)
                .unwrap_or_default(),
        );
    }
    if graph::check_connected(&repo.odb, &tips, &received, &boundary).is_err() {
        for p in &packs {
            p.release();
        }
        return Err(Error::fatal(messages::did_not_send_all(&result.url)));
    }
    if let (Some(set), false) = (&new_shallow, opts.dry_run) {
        shallow::write(&repo.shallow_file(), set)?;
    }

    // `--atomic`: every ref update (prunes included) in one transaction,
    // nothing at all when one of them is refused.
    let mut atomic_failed = false;
    let mut atomic_write_failed = false;
    if opts.atomic && !opts.dry_run {
        let stale = if prune {
            update::stale_refs(repo, &prune_specs, &refs)
        } else {
            Vec::new()
        };
        let decided: Vec<Status> = entries
            .iter()
            .map(|e| update::decide(repo, e, opts.force))
            .collect();
        atomic_failed = decided.iter().any(Status::is_error);
        let mut updates = Vec::new();
        if !atomic_failed {
            for (name, old) in &stale {
                updates.push(
                    RefUpdate::delete(name.to_str_lossy().into_owned(), repo.hash, Some(*old))
                        .no_deref(true),
                );
            }
            for (e, status) in entries.iter().zip(&decided) {
                if let (Some(local), true) = (&e.local, status.changes_ref()) {
                    updates.push(update::ref_update(
                        local.as_bstr(),
                        e.oid,
                        status,
                        &opts.reflog_action,
                    ));
                }
            }
            if let Err(err) = update::write_all(repo, updates) {
                eprintln!("error: {err}");
                atomic_failed = true;
                atomic_write_failed = true;
            }
        }
        if !atomic_failed {
            for (name, old) in stale {
                result.lines.push(RefLine {
                    remote: None,
                    local: Some(name),
                    new: old,
                    status: Status::Deleted,
                });
            }
        }
        for (e, status) in entries.iter().zip(decided) {
            let status = if atomic_write_failed && status.changes_ref() {
                Status::Failed(Box::new(status))
            } else {
                status
            };
            result.lines.push(RefLine {
                remote: Some(e.remote.clone()),
                local: e.local.clone(),
                new: e.oid,
                status,
            });
        }
    }

    // Prune first, as git lists deletions first.
    if prune && !(opts.atomic && !opts.dry_run) {
        for (name, old) in update::stale_refs(repo, &prune_specs, &refs) {
            let status = if opts.dry_run {
                Status::Deleted
            } else {
                match update::delete(repo, name.as_bstr(), old) {
                    Ok(()) => Status::Deleted,
                    Err(e) => {
                        result.errors.push(e);
                        continue;
                    }
                }
            };
            result.lines.push(RefLine {
                remote: None,
                local: Some(name),
                new: old,
                status,
            });
        }
    }

    let one_by_one = if opts.atomic && !opts.dry_run {
        &entries[..0]
    } else {
        &entries[..]
    };
    for e in one_by_one {
        let decided = update::decide(repo, e, opts.force);
        let status = match (&e.local, opts.dry_run) {
            (Some(local), false) => {
                update::apply(repo, local.as_bstr(), e.oid, decided, &opts.reflog_action)
            }
            _ => decided,
        };
        result.lines.push(RefLine {
            remote: Some(e.remote.clone()),
            local: e.local.clone(),
            new: e.oid,
            status,
        });
    }

    if follow_head && !opts.dry_run {
        result.head_warning = update_remote_head(repo, remote, &refs, &head_mode);
    }

    if opts.write_fetch_head && !opts.dry_run && !atomic_failed {
        let path = repo.git_dir.join("FETCH_HEAD");
        let text = display::fetch_head(&result.url, &entries);
        let written = if opts.append {
            use std::io::Write as _;
            std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&path)
                .and_then(|mut f| f.write_all(text.as_bytes()))
        } else {
            std::fs::write(&path, text)
        };
        written.map_err(|e| {
            Error::fatal(messages::cannot_open_fetch_head(
                &path,
                &rgit_transport_strerror(&e),
            ))
        })?;
    }
    for p in &packs {
        p.release();
    }
    result.entries = entries;
    Ok(result)
}

fn rgit_transport_strerror(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.rfind(" (os error ") {
        Some(i) => s[..i].to_string(),
        None => s,
    }
}

/// Add tags of `refs` that point into the fetched history: tags missing
/// locally whose (peeled) target is wanted or present. With `after_pack`
/// only targets present locally count (the pack has arrived). Tag objects
/// that are not present are added to `wants`. Tags already in `seen` are
/// skipped; new entries go to `out`.
fn add_followed_tags(
    repo: &LocalRepo,
    refs: &[RemoteRef],
    seen: &[Entry],
    out: &mut Vec<Entry>,
    wants: &mut Vec<ObjectId>,
    after_pack: bool,
) {
    for r in refs {
        let Some(oid) = r.oid else { continue };
        if !r.name.starts_with(b"refs/tags/") || r.name.ends_with(b"^{}") {
            continue;
        }
        let stored_elsewhere = |e: &Entry| {
            e.local
                .as_ref()
                .is_some_and(|l| l.as_slice() != r.name.as_slice())
        };
        if seen.iter().any(|e| {
            e.local
                .as_ref()
                .is_some_and(|l| l.as_slice() == r.name.as_slice())
                || (e.remote == r.name.as_slice() && !stored_elsewhere(e))
        }) {
            continue;
        }
        let name = String::from_utf8_lossy(&r.name);
        if repo.resolve(&name).is_some() {
            continue;
        }
        let peeled = r.peeled.unwrap_or(oid);
        let reachable = repo.has_object(&peeled) || (!after_pack && wants.contains(&peeled));
        if !reachable {
            continue;
        }
        out.push(Entry {
            remote: r.name.clone().into(),
            oid,
            local: Some(r.name.clone().into()),
            force: false,
            origin: Origin::FollowedTag,
            for_merge: false,
        });
        if !repo.has_object(&oid) {
            wants.push(oid);
        }
    }
}

/// `refs/remotes/<name>/HEAD` against the remote's `HEAD`
/// (config/fetch.adoc, `followRemoteHEAD`): `create` (the default) makes
/// it when missing, `always` also moves it, `warn` and
/// `warn-if-not-<branch>` leave it and return the warning to print.
fn update_remote_head(
    repo: &LocalRepo,
    remote: &Remote,
    refs: &[RemoteRef],
    mode: &str,
) -> Option<HeadWarning> {
    let target = refmap::advertised(refs, b"HEAD").and_then(|h| h.symref_target.clone())?;
    let local = remote.fetch.map_src(&target)?;
    let local = local.to_str_lossy().into_owned();
    let name = remote.name.to_str_lossy();
    // Only into the remote's own namespace (not for a mirror refspec).
    let prefix = format!("refs/remotes/{name}/");
    let short = local.strip_prefix(&prefix)?.to_owned();
    repo.resolve(&local)?;
    let head = format!("{prefix}HEAD");
    let current = match repo.refs.read_raw(&head) {
        Ok(None) => None,
        Ok(Some(t)) => Some(t),
        Err(_) => return None,
    };
    let Some(current) = current else {
        let _ = update::write(repo, RefUpdate::symref_create(head, local).message("fetch"));
        return None;
    };
    let ours = match &current {
        rgit_refs::Target::Symbolic(t) => {
            let t = t.to_string();
            if t == local {
                return None;
            }
            Some(t)
        }
        rgit_refs::Target::Object(_) => None,
    };
    if mode == "always" {
        let _ = update::write(
            repo,
            RefUpdate::symref_update(head, local, None)
                .no_deref(true)
                .message("fetch"),
        );
        return None;
    }
    let warn_unless = mode.strip_prefix("warn-if-not-");
    if mode != "warn" && warn_unless.is_none() {
        return None;
    }
    if warn_unless == Some(short.as_str()) {
        return None;
    }
    let text = match (ours, &current) {
        (Some(t), _) => {
            let t = t.strip_prefix(&prefix).unwrap_or(&t).to_owned();
            messages::remote_head_differs(&name, &short, &t)
        }
        (None, rgit_refs::Target::Object(oid)) => {
            messages::remote_head_detached(&name, &short, &oid.to_hex())
        }
        (None, _) => return None,
    };
    Some(HeadWarning {
        text,
        remote: name.into_owned(),
        branch: short,
    })
}

/// The `followRemoteHEAD=warn` message and what the advice names.
#[derive(Debug, Clone, Default)]
pub struct HeadWarning {
    /// The warning (standard output).
    pub text: String,
    /// The remote's name.
    pub remote: String,
    /// The branch the remote's `HEAD` points to.
    pub branch: String,
}
