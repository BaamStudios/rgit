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
pub(super) enum Kind {
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
pub(super) struct Head {
    /// The argument as given (the conflict label, the reflog).
    pub arg: String,
    /// The name in the message.
    pub name: String,
    pub kind: Kind,
    pub commit: ObjectId,
    /// The message of an annotated tag that was named.
    pub tag_message: Option<Vec<u8>>,
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

/// The title line of several heads: "Merge branches 'a' and 'b', tag 'c'".
fn title_many(repo: &Repo, heads: &[Head], into: &str) -> String {
    // Groups of one kind, in the order they first appear.
    let mut groups: Vec<(&'static str, &'static str, Vec<String>)> = Vec::new();
    for h in heads {
        let (one, many) = match h.kind {
            Kind::Branch | Kind::EarlyPart => (msg::KIND_BRANCH, msg::KIND_BRANCHES),
            Kind::Tag => (msg::KIND_TAG, msg::KIND_TAGS),
            Kind::Remote => (msg::KIND_REMOTE, msg::KIND_REMOTES),
            Kind::Commit | Kind::Fetch => (msg::KIND_COMMIT, msg::KIND_COMMITS),
        };
        let mut name = format!("'{}'", h.name);
        if h.kind == Kind::EarlyPart {
            name.push(' ');
            name.push_str(msg::EARLY_PART);
        }
        match groups.iter_mut().find(|g| g.0 == one) {
            Some(g) => g.2.push(name),
            None => groups.push((one, many, vec![name])),
        }
    }
    let mut t = String::from("Merge ");
    for (i, (one, many, names)) in groups.iter().enumerate() {
        if i > 0 {
            t.push_str(", ");
        }
        t.push_str(if names.len() > 1 { many } else { one });
        t.push(' ');
        for (j, n) in names.iter().enumerate() {
            if j > 0 {
                t.push_str(if j + 1 == names.len() { " and " } else { ", " });
            }
            t.push_str(n);
        }
    }
    if !suppressed(repo, into) {
        t.push_str(" into ");
        t.push_str(into);
    }
    t
}

/// The title line: "Merge branch 'side' into topic".
fn title(repo: &Repo, head: &Head, into: &str) -> String {
    let mut t = String::from("Merge ");
    if head.kind == Kind::Fetch {
        // "'<name>' of <url>" names a commit; "branch ..." and "tag ..."
        // name themselves.
        if head.name.starts_with('\'') {
            t.push_str(msg::KIND_COMMIT);
            t.push(' ');
        }
        t.push_str(&head.name);
        if !suppressed(repo, into) {
            t.push_str(" into ");
            t.push_str(into);
        }
        return t;
    }
    let kind = match head.kind {
        Kind::Branch | Kind::EarlyPart => msg::KIND_BRANCH,
        Kind::Tag => msg::KIND_TAG,
        Kind::Remote => msg::KIND_REMOTE,
        Kind::Commit | Kind::Fetch => msg::KIND_COMMIT,
    };
    t.push_str(kind);
    t.push_str(" '");
    t.push_str(&head.name);
    t.push('\'');
    if head.kind == Kind::EarlyPart {
        t.push(' ');
        t.push_str(msg::EARLY_PART);
    }
    if !suppressed(repo, into) {
        t.push_str(" into ");
        t.push_str(into);
    }
    t
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

/// The `--log` part: "* side:" and up to `limit` subjects of the commits
/// merged (merges left out).
fn shortlog(repo: &Repo, head: &Head, ours: Option<&ObjectId>, limit: usize) -> Vec<u8> {
    let subjects: Vec<Vec<u8>> = new_commits(repo, &head.commit, ours)
        .iter()
        .filter_map(|id| read_commit(repo, id))
        .filter(|c| c.parents().map_or(0, |p| p.len()) < 2)
        .map(|c| pretty::subject(&pretty::message_lines(c.message())))
        .collect();
    if subjects.is_empty() {
        return Vec::new();
    }
    let mut out = b"* ".to_vec();
    match head.kind {
        Kind::Branch | Kind::Remote => out.extend_from_slice(head.name.as_bytes()),
        // "branch 'x' of <url>" is listed as "'x' of <url>".
        Kind::Fetch => out.extend_from_slice(
            head.name
                .strip_prefix("branch ")
                .unwrap_or(&head.name)
                .as_bytes(),
        ),
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
pub(super) fn default_message(
    repo: &Repo,
    heads: &[Head],
    into: &str,
    ours: Option<&ObjectId>,
    log: usize,
) -> Vec<u8> {
    let mut out = match heads {
        [one] => title(repo, one, into),
        _ => title_many(repo, heads, into),
    }
    .into_bytes();
    out.push(b'\n');
    for head in heads {
        if let Some(t) = &head.tag_message {
            let body = rgit_commit::message::stripspace(t, None);
            if !body.is_empty() {
                out.push(b'\n');
                out.extend_from_slice(&body);
            }
        }
    }
    out.extend_from_slice(&log_part(repo, heads, ours, log));
    out
}

/// The shortlog to append to a message (with its leading empty line).
pub(super) fn log_part(repo: &Repo, heads: &[Head], ours: Option<&ObjectId>, log: usize) -> Vec<u8> {
    let mut out = Vec::new();
    if log == 0 {
        return out;
    }
    for head in heads {
        let s = shortlog(repo, head, ours, log);
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
