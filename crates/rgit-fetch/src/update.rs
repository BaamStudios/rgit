// SPDX-License-Identifier: MIT
//! Local ref updates of a fetch: the fast-forward and tag rules, writing
//! through reference transactions with reflog entries, and pruning.

use bstr::{BStr, BString, ByteSlice};
use rgit_hash::ObjectId;
use rgit_object::ObjectKind;
use rgit_protocol::v2::RemoteRef;
use rgit_refs::{NoHooks, RefHooks, RefStore, RefUpdate, Transaction};
use rgit_remote::RefspecSet;

use crate::refmap::Entry;
use crate::repo::LocalRepo;
use crate::{graph, messages};

/// What happened (or would happen) to one local ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Created; the remote ref is a branch (`[new branch]`).
    NewBranch,
    /// Created; the remote ref is a tag (`[new tag]`).
    NewTag,
    /// Created; any other remote ref (`[new ref]`).
    NewRef,
    /// Fast-forwarded from `old`.
    FastForward(ObjectId),
    /// Forced (non-fast-forward) update from `old`.
    Forced(ObjectId),
    /// A tag moved from `old` (forced).
    TagUpdate(ObjectId),
    /// Already pointing at the new object.
    UpToDate,
    /// Not updated: not a fast-forward and not forced.
    RejectedNonFastForward,
    /// Not updated: an existing tag would change.
    RejectedTag,
    /// The update was attempted but writing the ref failed.
    Failed(Box<Status>),
    /// Stored in `FETCH_HEAD` only.
    FetchHead,
    /// Deleted by `--prune`.
    Deleted,
}

impl Status {
    /// Whether the local ref was (or would be) changed.
    pub fn changes_ref(&self) -> bool {
        matches!(
            self,
            Status::NewBranch
                | Status::NewTag
                | Status::NewRef
                | Status::FastForward(_)
                | Status::Forced(_)
                | Status::TagUpdate(_)
        )
    }

    /// Whether the update was refused or failed (exit code 1).
    pub fn is_error(&self) -> bool {
        matches!(
            self,
            Status::RejectedNonFastForward | Status::RejectedTag | Status::Failed(_)
        )
    }

    fn reflog_message(&self) -> &'static str {
        match self {
            Status::NewBranch => messages::LOG_STORING_HEAD,
            Status::NewTag => messages::LOG_STORING_TAG,
            Status::NewRef => messages::LOG_STORING_REF,
            Status::FastForward(_) => messages::LOG_FAST_FORWARD,
            Status::Forced(_) => messages::LOG_FORCED_UPDATE,
            Status::TagUpdate(_) => messages::LOG_UPDATING_TAG,
            _ => "",
        }
    }
}

/// One line of the fetch summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefLine {
    /// The remote ref (full name); `None` for a pruned ref.
    pub remote: Option<BString>,
    /// The local ref (full name); `None` for `FETCH_HEAD` only.
    pub local: Option<BString>,
    /// The new object (the old one for a pruned ref).
    pub new: ObjectId,
    /// The outcome.
    pub status: Status,
}

/// Decide the update of `entry` without writing anything.
pub fn decide(repo: &LocalRepo, entry: &Entry, force: bool) -> Status {
    let Some(local) = &entry.local else {
        return Status::FetchHead;
    };
    let local = local.to_str_lossy();
    let Some(old) = repo.resolve(&local) else {
        return if entry.remote.starts_with(b"refs/tags/") {
            Status::NewTag
        } else if entry.remote.starts_with(b"refs/heads/") {
            Status::NewBranch
        } else {
            Status::NewRef
        };
    };
    if old == entry.oid {
        return Status::UpToDate;
    }
    let forced = force || entry.force;
    if local.starts_with("refs/tags/") {
        return if forced {
            Status::TagUpdate(old)
        } else {
            Status::RejectedTag
        };
    }
    let commits = repo.kind(&old) == Some(ObjectKind::Commit)
        && repo.kind(&entry.oid) == Some(ObjectKind::Commit);
    if commits && graph::is_ancestor(&repo.odb, &old, &entry.oid) {
        Status::FastForward(old)
    } else if forced {
        Status::Forced(old)
    } else {
        Status::RejectedNonFastForward
    }
}

/// Write the update `status` of `local` to `new`, with the reflog message
/// `<action>: <what>`. A failure turns the status into
/// [`Status::Failed`] (and the error is printed, as git does).
pub fn apply(
    repo: &LocalRepo,
    local: &BStr,
    new: ObjectId,
    status: Status,
    action: &str,
) -> Status {
    let msg = format!("{action}: {}", status.reflog_message());
    apply_with_message(repo, local, new, status, &msg)
}

/// [`apply`] with a complete reflog message (`clone: from <url>`).
pub fn apply_with_message(
    repo: &LocalRepo,
    local: &BStr,
    new: ObjectId,
    status: Status,
    msg: &str,
) -> Status {
    if !status.changes_ref() {
        return status;
    }
    let old = match &status {
        Status::FastForward(o) | Status::Forced(o) | Status::TagUpdate(o) => *o,
        _ => repo.hash.null(),
    };
    let update = RefUpdate::update(local.to_str_lossy().into_owned(), new, Some(old)).message(msg);
    match write(repo, update) {
        Ok(()) => status,
        Err(e) => {
            eprintln!("error: {e}");
            Status::Failed(Box::new(status))
        }
    }
}

/// Run one reference update (any [`RefUpdate`], e.g. a symref creation)
/// with the repository's reflog hooks.
pub fn write(repo: &LocalRepo, update: RefUpdate) -> Result<(), String> {
    let mut writer = repo.reflog_hooks();
    let hooks: &mut dyn RefHooks = match &mut writer {
        Some(w) => w,
        None => &mut NoHooks,
    };
    let store: &dyn RefStore = &repo.refs;
    let mut tx = Transaction::new(store);
    tx.push(update).map_err(|e| e.to_string())?;
    tx.commit(hooks).map_err(|e| e.to_string())
}

/// Local refs that a pattern refspec of `specs` would update but whose
/// remote ref no longer exists in `refs`, with their current object.
pub fn stale_refs(
    repo: &LocalRepo,
    specs: &RefspecSet,
    refs: &[RemoteRef],
) -> Vec<(BString, ObjectId)> {
    let mut out: Vec<(BString, ObjectId)> = Vec::new();
    for spec in specs.positive() {
        if !spec.pattern {
            continue;
        }
        let Some(dst) = &spec.dst else { continue };
        let prefix = dst
            .split_str("*")
            .next()
            .unwrap_or(b"")
            .to_str_lossy()
            .into_owned();
        // Only directories: `refs/remotes/origin/` lists below it.
        let list_prefix = match prefix.rfind('/') {
            Some(i) => &prefix[..=i],
            None => "",
        };
        for (name, oid) in repo.list_refs(list_prefix) {
            // Symbolic refs (`refs/remotes/<name>/HEAD`) are not pruned.
            if matches!(
                repo.refs.read_raw(&name),
                Ok(Some(rgit_refs::Target::Symbolic(_)))
            ) {
                continue;
            }
            let Some(src) = spec.map_dst(name.as_bytes()) else {
                continue;
            };
            if specs.is_excluded(&src) {
                continue;
            }
            if refs.iter().any(|r| r.name == src.as_slice()) {
                continue;
            }
            let name = BString::from(name);
            if !out.iter().any(|(n, _)| n == &name) {
                out.push((name, oid));
            }
        }
    }
    out
}

/// Delete a pruned ref (and its reflog).
pub fn delete(repo: &LocalRepo, name: &BStr, old: ObjectId) -> Result<(), String> {
    write(
        repo,
        RefUpdate::delete(name.to_str_lossy().into_owned(), repo.hash, Some(old)).no_deref(true),
    )
}
