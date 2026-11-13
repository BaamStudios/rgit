// SPDX-License-Identifier: MIT
//! The core of `git push`: matching refs, client-side checks, the
//! `receive-pack` exchange with the pack, and the status table.
//!
//! - [`matching`]: refspec expansion against local and remote refs.
//! - [`send`]: connection, commands, pack and status report.
//! - [`output`]: the table, hints and `Everything up-to-date`.

pub mod matching;
pub mod output;
pub mod send;

use std::collections::HashSet;

use rgit_hash::{HashKind, ObjectId};
use rgit_object::{Commit, ObjectKind, Tag};
use rgit_odb::ObjectDatabase;

/// What happened to one ref. The numbers git prints for the rejections
/// (`atomic push failed for ref ... status: <n>`) are [`Status::code`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// To be sent.
    None,
    Ok,
    UpToDate,
    RejectNonFf,
    RejectAlreadyExists,
    RejectFetchFirst,
    RejectNeedsForce,
    RemoteReject,
    RemoteFailure,
    AtomicFailed,
}

impl Status {
    pub fn is_error(self) -> bool {
        !matches!(self, Status::None | Status::Ok | Status::UpToDate)
    }

    /// git's number for the status (as observed with `--atomic`).
    pub fn code(self) -> u32 {
        match self {
            Status::None => 0,
            Status::Ok => 1,
            Status::RejectNonFf => 2,
            Status::RejectAlreadyExists => 3,
            Status::RejectFetchFirst => 5,
            Status::RejectNeedsForce => 6,
            Status::UpToDate => 9,
            Status::RemoteReject => 10,
            Status::RemoteFailure => 11,
            Status::AtomicFailed => 13,
        }
    }
}

/// One remote ref the push touches.
#[derive(Debug, Clone)]
pub struct PushRef {
    /// Full remote ref name.
    pub dst: String,
    /// Its value on the server; `None` if it does not exist.
    pub old: Option<ObjectId>,
    /// The value to set; `None` deletes.
    pub new: Option<ObjectId>,
    /// The local source as named: a full ref name, `HEAD` or an object
    /// name.
    pub src: Option<String>,
    pub force: bool,
    pub status: Status,
    /// A forced update that is not a fast-forward.
    pub forced_update: bool,
    /// The server's reason for `ng`.
    pub remote_msg: Option<String>,
}

/// Object lookups for the checks.
pub struct Objects<'a> {
    pub odb: &'a ObjectDatabase,
    pub hash: HashKind,
}

impl Objects<'_> {
    pub fn kind(&self, id: &ObjectId) -> Option<ObjectKind> {
        self.odb.header(id).ok().flatten().map(|h| h.kind)
    }

    /// Follow tags to the first non-tag object.
    fn peel(&self, id: &ObjectId) -> Option<(ObjectId, ObjectKind)> {
        let mut cur = *id;
        for _ in 0..64 {
            let raw = self.odb.read(&cur).ok()??;
            if raw.kind != ObjectKind::Tag {
                return Some((cur, raw.kind));
            }
            cur = Tag::parse(self.hash, &raw.data).ok()?.object().ok()?;
        }
        None
    }

    /// Whether commit `anc` is reachable from commit `desc`.
    fn is_ancestor(&self, anc: &ObjectId, desc: &ObjectId) -> bool {
        let mut seen = HashSet::new();
        let mut todo = vec![*desc];
        while let Some(c) = todo.pop() {
            if c == *anc {
                return true;
            }
            if !seen.insert(c) {
                continue;
            }
            let Ok(Some(raw)) = self.odb.read(&c) else {
                continue;
            };
            if let Ok(commit) = Commit::parse(self.hash, &raw.data) {
                todo.extend(commit.parents().unwrap_or_default());
            }
        }
        false
    }

    /// The shortest unique abbreviation, at least 7 digits.
    pub fn abbrev(&self, id: &ObjectId) -> String {
        let hex = id.to_hex();
        for len in 7..hex.len() {
            let Ok(p) = rgit_hash::Prefix::from_hex(self.hash, &hex.as_bytes()[..len]) else {
                break;
            };
            if !matches!(
                self.odb.resolve_prefix(&p),
                Ok(rgit_odb::PrefixMatch::Ambiguous(_))
            ) {
                return hex[..len].to_owned();
            }
        }
        hex
    }
}

/// The client-side checks (`git-push.adoc`, "PUSH RULES"): up to date,
/// creations and deletions, tags, fast-forwards.
pub fn check_refs(refs: &mut [PushRef], objects: &Objects<'_>) {
    for r in refs.iter_mut() {
        let (Some(old), Some(new)) = (r.old, r.new) else {
            continue;
        };
        if old == new {
            r.status = Status::UpToDate;
            continue;
        }
        let have_old = objects.odb.contains(&old);
        let commits = match (objects.peel(&old), objects.peel(&new)) {
            (Some((o, ObjectKind::Commit)), Some((n, ObjectKind::Commit))) => Some((o, n)),
            _ => None,
        };
        let ff = have_old && commits.is_some_and(|(o, n)| objects.is_ancestor(&o, &n));
        if r.force {
            r.forced_update = !ff;
        } else if r.dst.starts_with("refs/tags/") {
            r.status = Status::RejectAlreadyExists;
        } else if !have_old {
            r.status = Status::RejectFetchFirst;
        } else if commits.is_none() {
            r.status = Status::RejectNeedsForce;
        } else if !ff {
            r.status = Status::RejectNonFf;
        }
    }
}
