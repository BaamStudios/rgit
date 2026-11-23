// SPDX-License-Identifier: MIT
//! The messages `merge` writes: the default merge commit message
//! (git-fmt-merge-msg(1): "Merge branch 'x' into y", the tag message, the
//! `--log` shortlog) and the `--squash` message (`SQUASH_MSG`).
//!
//! How a head is named was derived from the documentation and checked
//! against the `git` binary: a name that is a local branch, a tag or a
//! remote-tracking branch is called so with the name as given; `<branch>~<n>`
//! and `<branch>^` name the branch ("(early part)" unless they are its
//! tip); everything else is a "commit" with the name as given.

use rgit_hash::ObjectId;
use rgit_object::{Commit, Object, ObjectKind};
use rgit_pathspec::wildmatch::{WildmatchFlags, wildmatch};
use rgit_revwalk::walk::{CommitInfo, CommitSource, Walk, WalkOptions};

use crate::history::pretty;
use crate::messages::merge as msg;
use crate::repository::Repo;

/// How the merge message calls a head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Kind {
    Branch,
    EarlyPart,
    Tag,
    Remote,
    Commit,
    /// A head from `FETCH_HEAD`; the name is its description
    /// ("branch 'x' of <url>").
    Fetch,
}

/// A commit to merge, with the name it was given.
#[derive(Debug, Clone)]
pub(crate) struct Head {
    /// The argument as given (the conflict label, the reflog).
    pub arg: String,
    /// The name in the message.
    pub name: String,
    pub kind: Kind,
    pub commit: ObjectId,
    /// The message of an annotated tag that was named.
    pub tag_message: Option<Vec<u8>>,
    /// The raw object of a signed annotated tag that was named (it goes
    /// into the `mergetag` header of the merge commit).
    pub signed_tag: Option<Vec<u8>>,
}

/// The raw tag `object` when it is a signed tag of `commit` (checked
/// against the `git` binary: only signed tags give a `mergetag` header).
pub(crate) fn signed_tag(repo: &Repo, object: ObjectId, commit: ObjectId) -> Option<Vec<u8>> {
    if object == commit {
        return None;
    }
    let raw = repo.odb().read(&object).ok()??;
    if raw.kind != ObjectKind::Tag {
        return None;
    }
    rgit_object::split_signature(&raw.data).1?;
    Some(raw.data)
}

/// The `mergetag` headers for the merged `parents` (all but the first),
/// in parent order.
pub(super) fn mergetags(heads: &[Head], parents: &[ObjectId]) -> Vec<rgit_object::Header> {
    parents
        .iter()
        .skip(1)
        .filter_map(|p| heads.iter().find(|h| h.commit == *p))
        .filter_map(|h| h.signed_tag.as_ref())
        .map(|raw| {
            let value = raw.strip_suffix(b"\n").unwrap_or(raw);
            rgit_object::Header::new("mergetag", value)
        })
        .collect()
}

/// The full ref name `name` stands for, if any.
pub(super) fn dwim(repo: &Repo, name: &str) -> Option<String> {
    let pseudo = name.starts_with("refs/")
        || (!name.is_empty()
            && name
                .bytes()
                .all(|c| c.is_ascii_uppercase() || c == b'_' || c == b'-')
            && name.ends_with("HEAD"));
    [
        pseudo.then(|| name.to_string()),
        Some(format!("refs/{name}")),
        Some(format!("refs/tags/{name}")),
        Some(format!("refs/heads/{name}")),
        Some(format!("refs/remotes/{name}")),
        Some(format!("refs/remotes/{name}/HEAD")),
    ]
    .into_iter()
    .flatten()
    .find(|c| repo.refs.resolve(c).is_some())
}

/// `name` without a trailing `~<n>` or `^...^`.
fn strip_ancestry(name: &str) -> Option<&str> {
    if let Some(p) = name.rfind('~') {
        let digits = &name[p + 1..];
        if p > 0 && !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
            return Some(&name[..p]);
        }
    }
    let base = name.trim_end_matches('^');
    (base.len() < name.len() && !base.is_empty()).then_some(base)
}

/// Names the head `arg` (resolved to `commit`; `object` is what the name
/// resolved to before peeling).
pub(super) fn name_head(repo: &Repo, arg: &str, object: ObjectId, commit: ObjectId) -> Head {
    let tag_message = match repo.read_object(&object) {
        Some(Object::Tag(t)) if object != commit => Some(t.message().to_vec()),
        _ => None,
    };
    let mut head = Head {
        arg: arg.to_string(),
        name: arg.to_string(),
        kind: Kind::Commit,
        commit,
        tag_message,
        signed_tag: signed_tag(repo, object, commit),
    };
    if let Some(full) = dwim(repo, arg) {
        head.kind = if full.starts_with("refs/heads/") {
            Kind::Branch
        } else if full.starts_with("refs/tags/") {
            Kind::Tag
        } else if full.starts_with("refs/remotes/") {
            Kind::Remote
        } else {
            Kind::Commit
        };
        return head;
    }
    if head.tag_message.is_some() {
        // An annotated tag named otherwise (by its id): a tag.
        head.kind = Kind::Tag;
        return head;
    }
    if let Some(base) = strip_ancestry(arg) {
        if let Some(full) = dwim(repo, base).filter(|f| f.starts_with("refs/heads/")) {
            head.name = base.to_string();
            head.kind = if repo.refs.resolve(&full) == Some(commit) {
                Kind::Branch
            } else {
                Kind::EarlyPart
            };
        }
    }
    head
}

/// Whether `merge.suppressDest` leaves " into <name>" out.
fn suppressed(repo: &Repo, into: &str) -> bool {
    let mut globs: Option<Vec<Vec<u8>>> = None;
    for e in repo.config.get_all("merge.suppressdest") {
        let list = globs.get_or_insert_with(Vec::new);
        match e.value.as_ref().map(|v| v.to_vec()) {
            Some(v) if !v.is_empty() => list.push(v),
            _ => list.clear(),
        }
    }
    let globs = globs.unwrap_or_else(|| vec![b"main".to_vec(), b"master".to_vec()]);
    globs
        .iter()
        .any(|g| wildmatch(g, into.as_bytes(), WildmatchFlags::NONE))
}

/// The groups of the title line, in this order within one source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    /// A `FETCH_HEAD` line naming only a URL (its `HEAD`).
    Head,
    Branch,
    Remote,
    Tag,
    Commit,
}

/// Where a head comes from (`.` for this repository, empty for a commit
/// named locally, which is shown without a source) and how the title line
/// calls it (the name keeps its quotes).
struct Origin {
    src: String,
    slot: Slot,
    name: String,
}

/// Splits a `FETCH_HEAD` description ("branch 'x' of <url>", "tag 't' of
/// <url>", "remote-tracking branch 'r' of <url>", "'<name>' of <url>" or
/// just "<url>").
fn fetch_origin(desc: &str) -> Origin {
    let typed = [
        ("branch ", Slot::Branch),
        ("remote-tracking branch ", Slot::Remote),
        ("tag ", Slot::Tag),
    ];
    for (prefix, slot) in typed {
        if let Some(rest) = desc.strip_prefix(prefix) {
            let (name, src) = rest.split_once(" of ").unwrap_or((rest, "."));
            return Origin {
                src: src.to_owned(),
                slot,
                name: name.to_owned(),
            };
        }
    }
    match desc.split_once(" of ") {
        Some((name, src)) => Origin {
            src: src.to_owned(),
            slot: Slot::Commit,
            name: name.to_owned(),
        },
        None => Origin {
            src: desc.to_owned(),
            slot: Slot::Head,
            name: "HEAD".to_owned(),
        },
    }
}

fn origin(head: &Head) -> Origin {
    let quoted = format!("'{}'", head.name);
    let (src, slot, name) = match head.kind {
        Kind::Fetch => return fetch_origin(&head.name),
        Kind::Branch => (".", Slot::Branch, quoted),
        Kind::EarlyPart => (".", Slot::Branch, format!("{quoted} {}", msg::EARLY_PART)),
        Kind::Remote => (".", Slot::Remote, quoted),
        Kind::Tag => (".", Slot::Tag, quoted),
        Kind::Commit => ("", Slot::Commit, quoted),
    };
    Origin {
        src: src.to_owned(),
        slot,
        name,
    }
}

/// The title line (git-fmt-merge-msg(1)): per source in the order of
/// appearance "HEAD, branches 'a' and 'b', remote-tracking branch 'r',
/// tag 't', commit 'c' of <url>", sources separated by "; ", then " into
/// <branch>".
fn title(repo: &Repo, heads: &[Head], into: &str) -> String {
    let mut sources: Vec<(String, Vec<Origin>)> = Vec::new();
    for o in heads.iter().map(origin) {
        match sources.iter_mut().find(|(s, _)| *s == o.src) {
            Some((_, list)) => list.push(o),
            None => sources.push((o.src.clone(), vec![o])),
        }
    }
    let mut t = String::from("Merge ");
    for (i, (src, list)) in sources.iter().enumerate() {
        if i > 0 {
            t.push_str("; ");
        }
        let mut parts = Vec::new();
        for slot in [Slot::Head, Slot::Branch, Slot::Remote, Slot::Tag, Slot::Commit] {
            let names: Vec<&str> = list
                .iter()
                .filter(|o| o.slot == slot)
                .map(|o| o.name.as_str())
                .collect();
            if names.is_empty() {
                continue;
            }
            let (one, many) = match slot {
                Slot::Head => {
                    parts.push("HEAD".to_owned());
                    continue;
                }
                Slot::Branch => (msg::KIND_BRANCH, msg::KIND_BRANCHES),
                Slot::Remote => (msg::KIND_REMOTE, msg::KIND_REMOTES),
                Slot::Tag => (msg::KIND_TAG, msg::KIND_TAGS),
                Slot::Commit => (msg::KIND_COMMIT, msg::KIND_COMMITS),
            };
            let mut p = String::from(if names.len() > 1 { many } else { one });
            p.push(' ');
            for (j, n) in names.iter().enumerate() {
                if j > 0 {
                    p.push_str(if j + 1 == names.len() { " and " } else { ", " });
                }
                p.push_str(n);
            }
            parts.push(p);
        }
        // Only the HEAD of a URL: the URL alone.
        if parts.len() == 1 && list.iter().all(|o| o.slot == Slot::Head) {
            t.push_str(src);
            continue;
        }
        t.push_str(&parts.join(", "));
        if !src.is_empty() && src != "." {
            t.push_str(" of ");
            t.push_str(src);
        }
    }
    if !suppressed(repo, into) {
        t.push_str(" into ");
        t.push_str(into);
    }
    t
}

/// The heading of a fetched head in the `--log` part: "<name>" from this
/// repository, "'<name>' of <url>" from another, "tag '<name>'" with the
/// source for tags, only the source for anything else.
fn fetch_log_heading(desc: &str) -> String {
    let o = fetch_origin(desc);
    let local = o.src == ".";
    match o.slot {
        Slot::Branch | Slot::Remote if local => o.name.trim_matches('\'').to_owned(),
        Slot::Branch | Slot::Remote => format!("{} of {}", o.name, o.src),
        Slot::Tag if local => format!("{} {}", msg::KIND_TAG, o.name),
        Slot::Tag => format!("{} {} of {}", msg::KIND_TAG, o.name, o.src),
        Slot::Head | Slot::Commit => o.src,
    }
}

/// A walk source over the repository's commits.
struct Commits<'a>(&'a Repo);

impl CommitSource for Commits<'_> {
    type Error = ();
    fn load(&mut self, id: &ObjectId) -> Result<CommitInfo, ()> {
        match self.0.read_object(id) {
            Some(Object::Commit(c)) => Ok(CommitInfo {
                parents: c.parents().map_err(|_| ())?,
                time: c
                    .committer()
                    .and_then(|s| s.time())
                    .map_or(0, |t| t.seconds),
            }),
            _ => Err(()),
        }
    }
}

/// Commits reachable from `tip` but not from `exclude`, newest first.
pub(super) fn new_commits(repo: &Repo, tip: &ObjectId, exclude: Option<&ObjectId>) -> Vec<ObjectId> {
    new_commits_of(repo, std::slice::from_ref(tip), exclude)
}

/// Commits reachable from `tips` but not from `exclude`, newest first.
fn new_commits_of(repo: &Repo, tips: &[ObjectId], exclude: Option<&ObjectId>) -> Vec<ObjectId> {
    let mut walk = Walk::new(Commits(repo), WalkOptions::default());
    for tip in tips {
        if walk.push(tip, false).is_err() {
            return Vec::new();
        }
    }
    if let Some(x) = exclude {
        if walk.push(x, true).is_err() {
            return Vec::new();
        }
    }
    walk.collect_all().unwrap_or_default()
}

fn read_commit(repo: &Repo, id: &ObjectId) -> Option<Commit> {
    match repo.read_object(id)? {
        Object::Commit(c) => Some(c),
        _ => None,
    }
}

/// A name and address.
type Ident = (Vec<u8>, Vec<u8>);

/// The name and address of the current author and committer.
fn current_idents(repo: &Repo) -> [Option<Ident>; 2] {
    use rgit_ident::ident::{IdentConfig, Resolver, Role, Strictness};
    let env = rgit_config::SystemEnv::default();
    let Ok(cfg) = IdentConfig::from_config(&repo.config) else {
        return [None, None];
    };
    let r = Resolver::new(&cfg, &env);
    [Role::Author, Role::Committer].map(|role| {
        r.name_email(role, Strictness::Lenient)
            .ok()
            .map(|(n, e)| (n.to_vec(), e.to_vec()))
    })
}

/// The credit lines before a shortlog (observed with the `git` binary):
/// "By <author>" when somebody else wrote some of the commits (with
/// counts for two authors, "and others" for more; most commits first,
/// then the newest), and "Via <committer>" when somebody else committed
/// the newest one.
fn credits(commits: &[Commit], me: &[Option<Ident>; 2], comment: &str) -> Vec<u8> {
    let mut authors: Vec<(Ident, usize)> = Vec::new();
    for c in commits {
        let Some(a) = c.author() else { continue };
        let key = (a.name.to_vec(), a.email.to_vec());
        match authors.iter_mut().find(|(k, _)| *k == key) {
            Some((_, n)) => *n += 1,
            None => authors.push((key, 1)),
        }
    }
    let mut out = Vec::new();
    let all_mine = authors.iter().all(|(k, _)| Some(k) == me[0].as_ref());
    if !authors.is_empty() && !all_mine {
        authors.sort_by_key(|a| std::cmp::Reverse(a.1));
        let name = |i: usize| String::from_utf8_lossy(&authors[i].0.0).into_owned();
        let line = match authors.len() {
            1 => msg::credit_by_one(&name(0)),
            2 => msg::credit_by_two(&name(0), authors[0].1, &name(1), authors[1].1),
            _ => msg::credit_by_many(&name(0), authors[0].1),
        };
        out.extend_from_slice(format!("{comment} {line}\n").as_bytes());
    }
    if let Some(c) = commits.first().and_then(|c| c.committer()) {
        let key = (c.name.to_vec(), c.email.to_vec());
        if Some(&key) != me[1].as_ref() {
            let line = msg::credit_via(&String::from_utf8_lossy(&key.0));
            out.extend_from_slice(format!("{comment} {line}\n").as_bytes());
        }
    }
    out
}

/// The `--log` part: the credit lines, "* side:" and up to `limit`
/// subjects of the commits merged (merges left out).
fn shortlog(
    repo: &Repo,
    head: &Head,
    ours: Option<&ObjectId>,
    limit: usize,
    me: &[Option<Ident>; 2],
    comment: &str,
) -> Vec<u8> {
    let commits: Vec<Commit> = new_commits(repo, &head.commit, ours)
        .iter()
        .filter_map(|id| read_commit(repo, id))
        .filter(|c| c.parents().map_or(0, |p| p.len()) < 2)
        .collect();
    let subjects: Vec<Vec<u8>> = commits
        .iter()
        .map(|c| pretty::subject(&pretty::message_lines(c.message())))
        .collect();
    if subjects.is_empty() {
        return Vec::new();
    }
    let mut out = credits(&commits, me, comment);
    out.extend_from_slice(b"* ");
    match head.kind {
        Kind::Branch | Kind::Remote => out.extend_from_slice(head.name.as_bytes()),
        Kind::Fetch => out.extend_from_slice(fetch_log_heading(&head.name).as_bytes()),
        Kind::EarlyPart => {
            out.extend_from_slice(format!("'{}' {}", head.name, msg::EARLY_PART).as_bytes())
        }
        Kind::Tag => out.extend_from_slice(format!("{} '{}'", msg::KIND_TAG, head.name).as_bytes()),
        Kind::Commit => {
            out.extend_from_slice(format!("{} '{}'", msg::KIND_COMMIT, head.name).as_bytes())
        }
    }
    out.push(b':');
    if subjects.len() > limit {
        out.extend_from_slice(msg::commits_count(subjects.len()).as_bytes());
    }
    out.push(b'\n');
    for s in subjects.iter().take(limit) {
        out.extend_from_slice(b"  ");
        out.extend_from_slice(s);
        out.push(b'\n');
    }
    if subjects.len() > limit {
        out.extend_from_slice(b"  ...\n");
    }
    out
}

/// The default message of merging `head` into `into` (`HEAD` when
/// detached), with the tag message and a shortlog of at most `log`
/// commits.
pub(crate) fn default_message(
    repo: &Repo,
    heads: &[Head],
    into: &str,
    ours: Option<&ObjectId>,
    log: usize,
) -> Vec<u8> {
    let mut out = title(repo, heads, into).into_bytes();
    out.push(b'\n');
    for head in heads {
        if let Some(t) = &head.tag_message {
            let (text, sig) = rgit_object::split_signature(t);
            let body = rgit_commit::message::stripspace(text, None);
            if !body.is_empty() {
                out.push(b'\n');
                out.extend_from_slice(&body);
            }
            // The signature and what checking it says, as comments
            // (observed with the `git` binary).
            if let (Some(sig), Some(raw)) = (sig, &head.signed_tag) {
                let mut shown = sig.to_vec();
                if !shown.ends_with(b"\n") {
                    shown.push(b'\n');
                }
                if let (Ok(settings), Some((payload, signature))) = (
                    crate::signing::Settings::from_config(&repo.config),
                    crate::signing::verify::tag_signature(raw),
                ) {
                    let c = crate::signing::verify::check(&settings, &payload, &signature);
                    shown.extend_from_slice(&c.output);
                }
                let comment = crate::comment::comment(&repo.config)
                    .map(|c| c.or_hash())
                    .unwrap_or_else(|_| b"#".to_vec());
                out.push(b'\n');
                for line in shown.split_inclusive(|&b| b == b'\n') {
                    out.extend_from_slice(&comment);
                    if line != b"\n" {
                        out.push(b' ');
                    }
                    out.extend_from_slice(line);
                }
            }
        }
    }
    out.extend_from_slice(&log_part(repo, heads, ours, log));
    out
}

/// The shortlog to append to a message (with its leading empty line).
pub(crate) fn log_part(repo: &Repo, heads: &[Head], ours: Option<&ObjectId>, log: usize) -> Vec<u8> {
    let mut out = Vec::new();
    if log == 0 {
        return out;
    }
    let me = current_idents(repo);
    let comment = crate::commands::commit::comment_string(repo, b"").unwrap_or_else(|_| "#".to_owned());
    for head in heads {
        let s = shortlog(repo, head, ours, log, &me, &comment);
        if !s.is_empty() {
            out.push(b'\n');
            out.extend_from_slice(&s);
        }
    }
    out
}

/// `SQUASH_MSG`: "Squashed commit of the following:" and the commits
/// merged in `log --pretty=medium` form.
pub(super) fn squash_message(repo: &Repo, theirs: &[ObjectId], ours: Option<&ObjectId>) -> Vec<u8> {
    let mut out = msg::SQUASHED_HEADER.as_bytes().to_vec();
    out.extend_from_slice(b"\n\n");
    let style = pretty::Style {
        pretty: pretty::Pretty::Medium,
        mailmap: None,
        date_mode: rgit_date::DateMode::default(),
        date_ctx: rgit_date::DateContext::from_env(),
        commit_color: None,
        zero: false,
        tab_width: None,
    };
    let mut first = true;
    // Like `log --no-merges`.
    for id in new_commits_of(repo, theirs, ours) {
        let Some(commit) = read_commit(repo, &id) else {
            continue;
        };
        if commit.parents().map_or(0, |p| p.len()) > 1 {
            continue;
        }
        let merge_parents: Vec<String> = Vec::new();
        let id = &id;
        let raw = match repo.odb().read(id) {
            Ok(Some(r)) if r.kind == ObjectKind::Commit => r.data,
            _ => Vec::new(),
        };
        let name = id.to_hex();
        let entry = pretty::Entry {
            name: &name,
            shown_parents: &[],
            merge_parents: &merge_parents,
            commit: &commit,
            raw: &raw,
            decoration: &[],
            signature: &[],
        };
        pretty::write_entry(&mut out, &style, &entry, first);
        first = false;
    }
    out
}

/// The comment block listing the conflicted paths (appended to
/// `MERGE_MSG`).
pub(super) fn conflicts_block(comment: &str, paths: &[Vec<u8>], quote: bool) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(b'\n');
    out.extend_from_slice(comment.as_bytes());
    out.push(b' ');
    out.extend_from_slice(msg::CONFLICTS.as_bytes());
    out.push(b'\n');
    for p in paths {
        out.extend_from_slice(comment.as_bytes());
        out.push(b'\t');
        crate::quote::write_quoted(&mut out, p, quote);
        out.push(b'\n');
    }
    out
}
