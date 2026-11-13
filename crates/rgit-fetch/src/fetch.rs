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
use crate::{Error, Result, graph, messages};

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
    /// The final ref map (what `FETCH_HEAD` lists).
    pub entries: Vec<Entry>,
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
    if tag_opt == TagOpt::All {
        specs.items.push(parse_spec("refs/tags/*:refs/tags/*")?);
    }

    // Merge candidates of the current branch.
    let mut merge_refs: Vec<BString> = Vec::new();
    if !cmdline && remote.is_configured() {
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
    let follow_head =
        !cmdline && remote.is_configured() && follow_remote_head(repo, &remote.name) != "never";

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

    // Never move the checked-out branch under the work tree.
    if !repo.is_bare() && !opts.update_head_ok {
        if let (Some(cur), Some(wt)) = (repo.current_branch(), &repo.work_tree) {
            if entries
                .iter()
                .any(|e| e.local.as_ref().map(|l| l.as_slice()) == Some(cur.as_bytes()))
            {
                return Err(Error::fatal(messages::refusing_to_fetch_into(&cur, wt)));
            }
        }
    }

    // One `want` per ref whose object is missing, duplicates included
    // (as git sends them).
    let mut wants: Vec<ObjectId> = Vec::new();
    for e in &entries {
        if !repo.has_object(&e.oid) {
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
    if cmdline && remote.is_configured() {
        let extra = refmap::opportunistic(&entries, &remote.fetch);
        entries.extend(extra);
    }

    let advertised: Vec<ObjectId> = refs.iter().filter_map(|r| r.oid).collect();
    let pack_opts = PackOptions {
        no_progress: opts.no_progress,
        include_tag: follow_tags,
        server_options: opts.server_options.clone(),
    };
    let mut packs: Vec<ReceivedPack> = Vec::new();
    if let Some(p) = session.fetch_pack(repo, &wants, Some(&advertised), &pack_opts)? {
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
        if let Some(p) = session.fetch_pack(repo, &more, Some(&advertised), &pack_opts)? {
            packs.push(p);
        }
    }
    session.close()?;

    let received: std::collections::HashSet<ObjectId> = packs
        .iter()
        .flat_map(|p| p.objects.iter().copied())
        .collect();
    let tips: Vec<ObjectId> = entries.iter().map(|e| e.oid).collect();
    if graph::check_connected(&repo.odb, &tips, &received).is_err() {
        for p in &packs {
            p.release();
        }
        return Err(Error::fatal(messages::did_not_send_all(&result.url)));
    }

    // Prune first, as git lists deletions first.
    let prune = opts.prune.or(remote.prune).unwrap_or_else(|| {
        repo.config
            .bool("fetch.prune")
            .ok()
            .flatten()
            .unwrap_or(false)
    });
    if prune {
        for (name, old) in update::stale_refs(repo, &specs, &refs) {
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

    for e in &entries {
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
        update_remote_head(repo, remote, &refs);
    }

    if opts.write_fetch_head && !opts.dry_run {
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
        if seen.iter().any(|e| e.remote == r.name.as_slice()) {
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

/// Create `refs/remotes/<name>/HEAD` for the remote's `HEAD` if it does
/// not exist yet (`followRemoteHEAD=create`, the default).
fn update_remote_head(repo: &LocalRepo, remote: &Remote, refs: &[RemoteRef]) {
    let Some(target) = refmap::advertised(refs, b"HEAD").and_then(|h| h.symref_target.clone())
    else {
        return;
    };
    let Some(local) = remote.fetch.map_src(&target) else {
        return;
    };
    let local = local.to_str_lossy().into_owned();
    if repo.resolve(&local).is_none() {
        return;
    }
    let head = format!("refs/remotes/{}/HEAD", remote.name.to_str_lossy());
    if !matches!(repo.refs.read_raw(&head), Ok(None)) {
        return;
    }
    let _ = update::write(repo, RefUpdate::symref_create(head, local).message("fetch"));
}
