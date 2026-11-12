// SPDX-License-Identifier: MIT
//! Transactions of the files backend.
//!
//! Prepare locks every affected reference (`<name>.lock`, created with
//! `rgit-fs`), reads its current value under the lock, checks the expected
//! old value and writes the new content into the lock file. Deletions of
//! packed references also lock `packed-refs`. Commit first appends reflog
//! entries, then rewrites `packed-refs`, then renames the lock files into
//! place and removes deleted loose files. Any error before commit releases
//! every lock and removes directories that were created only for them.

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use rgit_fs::{LockFile, LockOptions};
use rgit_hash::ObjectId;

use super::FilesStore;
use crate::error::{Error, LockReason, TransactionError};
use crate::loose;
use crate::messages;
use crate::packed::PackedRefs;
use crate::store::{
    HookState, NewValue, OldValue, PreparedTransaction, RefHooks, RefUpdate, ReflogUpdate,
    check_duplicates, check_update_name,
};
use crate::target::Target;

/// How a name entered the transaction (for "multiple updates" messages).
#[derive(Clone, Debug)]
enum Origin {
    /// Named by an update.
    Direct,
    /// Reached by following the symbolic reference in the payload.
    ViaSymref(String),
    /// `HEAD`, added because it points at the updated reference.
    ViaReferent(String),
}

enum Action {
    /// Only verified or logged.
    Keep,
    /// The lock file holds the new content.
    Write,
    /// Remove the loose file (if `loose`) and the reflog. `missing`: the
    /// reference did not exist at all.
    Delete { loose: bool, missing: bool },
}

struct Item {
    name: String,
    path: PathBuf,
    /// Index of the update this lock belongs to.
    update: usize,
    lock: Option<LockFile>,
    action: Action,
    reflogs: Vec<ReflogUpdate>,
}

pub(super) struct Prepared<'s> {
    store: &'s FilesStore,
    items: Vec<Item>,
    packed: Option<(LockFile, PackedRefs)>,
    hook_updates: Vec<(ObjectId, ObjectId, String)>,
}

impl Prepared<'_> {
    fn rollback(&mut self) {
        if let Some((lock, _)) = self.packed.take() {
            lock.rollback();
        }
        for item in self.items.drain(..) {
            if let Some(lock) = item.lock {
                lock.rollback();
                prune_empty_parents(&self.store.git_dir, &item.path);
            }
        }
    }
}

impl Drop for Prepared<'_> {
    fn drop(&mut self) {
        self.rollback();
    }
}

impl PreparedTransaction for Prepared<'_> {
    fn commit(mut self: Box<Self>, hooks: &mut dyn RefHooks) -> Result<(), TransactionError> {
        for item in &self.items {
            for log in &item.reflogs {
                if let Err(msg) = hooks.reflog_append(log) {
                    self.rollback();
                    return Err(TransactionError::Rejected(msg));
                }
            }
        }
        if let Some((mut lock, packed)) = self.packed.take() {
            let path = lock.lock_path().to_owned();
            let result = lock
                .write_all(&packed.serialize())
                .and_then(|()| lock.commit());
            if let Err(source) = result {
                self.rollback();
                return Err(TransactionError::Io { path, source });
            }
        }
        let mut first_error = None;
        for item in std::mem::take(&mut self.items) {
            let Some(lock) = item.lock else { continue };
            match item.action {
                Action::Write => {
                    if std::fs::symlink_metadata(&item.path).is_ok_and(|m| m.is_dir())
                        && !dir_has_files(&item.path)
                    {
                        // Empty directories where the reference goes.
                        let _ = std::fs::remove_dir_all(&item.path);
                    }
                    if let Err(source) = lock.commit() {
                        first_error.get_or_insert(TransactionError::Io {
                            path: item.path.clone(),
                            source,
                        });
                    }
                }
                Action::Delete { loose, missing } => {
                    if loose {
                        match std::fs::remove_file(&item.path) {
                            Ok(()) => {}
                            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                            Err(source) => {
                                first_error.get_or_insert(TransactionError::Io {
                                    path: item.path.clone(),
                                    source,
                                });
                            }
                        }
                    } else if missing
                        && std::fs::symlink_metadata(&item.path).is_ok_and(|m| m.is_dir())
                        && !dir_has_files(&item.path)
                    {
                        // Empty directories where the deleted (missing)
                        // reference would be.
                        let _ = std::fs::remove_dir_all(&item.path);
                    }
                    lock.rollback();
                    hooks.reflog_delete(&item.name);
                    prune_empty_parents(&self.store.git_dir, &item.path);
                }
                Action::Keep => {
                    lock.rollback();
                    prune_empty_parents(&self.store.git_dir, &item.path);
                }
            }
        }
        let _ = hooks.reference_transaction(HookState::Committed, &self.hook_updates);
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn abort(mut self: Box<Self>, hooks: &mut dyn RefHooks) {
        self.rollback();
        let _ = hooks.reference_transaction(HookState::Aborted, &self.hook_updates);
    }
}

/// Removes empty directories from the parent of `path` upwards, keeping
/// `$GIT_DIR/refs` and its direct subdirectories (`refs/heads`, ...).
pub(super) fn prune_empty_parents(git_dir: &Path, path: &Path) {
    let Ok(rel) = path.strip_prefix(git_dir) else {
        return;
    };
    let mut rel = rel.to_path_buf();
    while rel.pop() {
        if rel.components().count() < 3 {
            break;
        }
        if std::fs::remove_dir(git_dir.join(&rel)).is_err() {
            break;
        }
    }
}

/// Names that `name` would need as directories (`refs`, `refs/heads`, ...).
fn dir_prefixes(name: &str) -> impl Iterator<Item = &str> {
    name.match_indices('/').map(move |(i, _)| &name[..i])
}

/// First reference file (sorted, ignoring `*.lock`) below `dir`, as a name
/// relative to `rel`.
fn find_ref_below(dir: &Path, rel: &str) -> Option<String> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut names: Vec<(String, bool)> = entries
        .flatten()
        .filter_map(|e| {
            let is_dir = e.file_type().ok()?.is_dir();
            Some((e.file_name().into_string().ok()?, is_dir))
        })
        .collect();
    names.sort();
    for (n, is_dir) in names {
        let child_rel = format!("{rel}/{n}");
        if is_dir {
            if let Some(found) = find_ref_below(&dir.join(&n), &child_rel) {
                return Some(found);
            }
        } else if !n.ends_with(".lock") {
            return Some(child_rel);
        }
    }
    None
}

/// Whether the directory tree below `dir` contains anything but
/// directories.
fn dir_has_files(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| match e.file_type() {
        Ok(t) if t.is_dir() => dir_has_files(&e.path()),
        _ => true,
    })
}

/// A name waiting to be locked.
#[derive(Clone, Debug)]
struct Queued {
    update: usize,
    name: String,
    /// Symbolic references followed to reach `name`.
    parents: Vec<String>,
    /// `HEAD`, locked only to log the change of its branch.
    log_only: bool,
}

/// The effect of one update, known once its final reference is checked.
struct Outcome {
    old: ObjectId,
    new: ObjectId,
    final_name: String,
    delete: bool,
}

/// Result of [`State::lock`].
struct Locked {
    lock: Option<LockFile>,
    /// Why the reference could not be created if it is missing.
    blocked: Option<LockReason>,
    /// Only empty directories occupy the path; removed if it is missing.
    empty_dir: bool,
}

struct State<'s, 'h> {
    store: &'s FilesStore,
    hooks: &'h mut dyn RefHooks,
    packed: PackedRefs,
    occupied: HashMap<String, Origin>,
    /// The names of the updates.
    all_names: Vec<String>,
    /// The references finally changed or checked, in order.
    final_names: Vec<String>,
    /// Final names that did not exist before.
    creations: Vec<String>,
    /// What `HEAD` points to when the transaction starts.
    head_referent: Option<String>,
    results: Vec<Option<Outcome>>,
    prepared: Prepared<'s>,
}

impl State<'_, '_> {
    fn lock_options(&self) -> LockOptions {
        let o = &self.store.options;
        LockOptions::new()
            .timeout(o.ref_lock_timeout)
            .follow_symlinks(false)
            .shared(o.shared)
            .fsync(o.fsync)
    }

    fn occupy(&mut self, name: &str, origin: Origin) -> Result<(), TransactionError> {
        if let Some(existing) = self.occupied.get(name) {
            // Like git, two symbolic references leading to the same
            // reference are not reported here (the second lock fails).
            if matches!(
                (&origin, existing),
                (Origin::ViaSymref(_), Origin::ViaSymref(_))
            ) {
                return Ok(());
            }
            return Err(match (&origin, existing) {
                (Origin::ViaSymref(s), _) | (_, Origin::ViaSymref(s)) => {
                    TransactionError::DuplicateViaSymref {
                        name: name.to_owned(),
                        symref: s.clone(),
                    }
                }
                (Origin::ViaReferent(r), _) | (_, Origin::ViaReferent(r)) => {
                    TransactionError::DuplicateViaReferent {
                        name: name.to_owned(),
                        referent: r.clone(),
                    }
                }
                _ => TransactionError::Duplicate(name.to_owned()),
            });
        }
        self.occupied.insert(name.to_owned(), origin);
        Ok(())
    }

    /// Locks `name`.
    ///
    /// When something is in the way of the lock (a file where a parent
    /// directory has to be, or a directory with files at the reference's
    /// path), the name is checked against existing references and the other
    /// names of the transaction. The conflict only matters when the
    /// reference turns out to be missing (and its old value allows that),
    /// so it is returned in [`Locked::blocked`] instead of failing here.
    fn lock(&mut self, name: &str) -> Result<Locked, LockReason> {
        let git_dir = &self.store.git_dir;
        let path = git_dir.join(name);
        let prefix_file = dir_prefixes(name)
            .any(|p| std::fs::symlink_metadata(git_dir.join(p)).is_ok_and(|m| !m.is_dir()));
        let mut empty_dir = false;
        let mut in_the_way = prefix_file;
        if !prefix_file && std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
            in_the_way = dir_has_files(&path);
            empty_dir = !in_the_way;
        }
        let blocked = if in_the_way {
            Some(
                self.availability(name, true)
                    .unwrap_or_else(|| LockReason::BlockingDirectory {
                        dir: path.clone(),
                        name: name.to_owned(),
                    }),
            )
        } else {
            None
        };
        if prefix_file {
            return Ok(Locked {
                lock: None,
                blocked,
                empty_dir: false,
            });
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| LockReason::Io(e.to_string()))?;
        }
        let lock = LockFile::acquire(&path, &self.lock_options())?;
        Ok(Locked {
            lock: Some(lock),
            blocked,
            empty_dir,
        })
    }

    /// Whether `name` can be created next to the existing references and
    /// the other names of the transaction. Checks every parent name first
    /// (an existing reference, then a name of the transaction), then names
    /// below `name/`.
    ///
    /// While locking (`include_loose`), loose references count and only the
    /// names of the updates are known; afterwards only packed references
    /// count and the referents of symbolic references are known too.
    fn availability(&self, name: &str, include_loose: bool) -> Option<LockReason> {
        let git_dir = &self.store.git_dir;
        let extra: &[String] = if include_loose {
            &[]
        } else {
            &self.final_names
        };
        let others = || {
            self.all_names
                .iter()
                .chain(extra)
                .filter(|n| n.as_str() != name)
        };
        for prefix in dir_prefixes(name) {
            let loose = include_loose
                && std::fs::symlink_metadata(git_dir.join(prefix)).is_ok_and(|m| !m.is_dir());
            if loose || self.packed.find(prefix).is_some() {
                return Some(LockReason::NameConflict {
                    existing: prefix.to_owned(),
                    new: name.to_owned(),
                });
            }
            if others().any(|n| n == prefix) {
                return Some(LockReason::CannotProcessBoth {
                    name: name.to_owned(),
                    other: prefix.to_owned(),
                });
            }
        }
        let dir_prefix = format!("{name}/");
        let loose = if include_loose {
            find_ref_below(&git_dir.join(name), name)
        } else {
            None
        };
        let packed = self
            .packed
            .with_prefix(&dir_prefix)
            .next()
            .map(|p| p.name.clone());
        let existing = match (loose, packed) {
            (Some(a), Some(b)) => Some(if a.as_bytes() <= b.as_bytes() { a } else { b }),
            (a, b) => a.or(b),
        };
        if let Some(existing) = existing {
            return Some(LockReason::NameConflict {
                existing,
                new: name.to_owned(),
            });
        }
        others()
            .find(|n| n.starts_with(&dir_prefix))
            .map(|other| LockReason::CannotProcessBoth {
                name: name.to_owned(),
                other: other.clone(),
            })
    }

    /// Reads `name` (loose, then packed). Broken loose files count as absent.
    fn read(&self, name: &str) -> Result<Option<Target>, Error> {
        match self.store.read_raw_with(name, &self.packed) {
            Err(Error::Broken(_)) => Ok(None),
            other => other,
        }
    }

    fn resolve_oid(&self, name: &str) -> Option<ObjectId> {
        match self.store.resolve_with(name, &self.packed) {
            Ok(Some(r)) => r.oid,
            _ => None,
        }
    }

    fn push_item(&mut self, name: &str, update: usize, lock: Option<LockFile>) -> usize {
        self.prepared.items.push(Item {
            name: name.to_owned(),
            path: self.store.git_dir.join(name),
            update,
            lock,
            action: Action::Keep,
            reflogs: Vec::new(),
        });
        self.prepared.items.len() - 1
    }

    /// Locks and checks one queued name. Following a symbolic reference
    /// appends its referent to the queue (processed after everything queued
    /// before it, as git does); `HEAD` is appended when its branch changes.
    fn process(
        &mut self,
        updates: &[RefUpdate],
        queue: &mut Vec<Queued>,
        index: usize,
    ) -> Result<(), TransactionError> {
        let q = queue[index].clone();
        let u = &updates[q.update];
        let cannot_lock = |reason: LockReason| TransactionError::CannotLock {
            name: u.name.clone(),
            reason,
        };
        if q.log_only {
            let locked = self.lock(&q.name).map_err(cannot_lock)?;
            self.push_item(&q.name, q.update, locked.lock);
            return Ok(());
        }

        if q.name != "HEAD"
            && !q.parents.iter().any(|p| p == "HEAD")
            && self.head_referent.as_deref() == Some(q.name.as_str())
        {
            self.occupy("HEAD", Origin::ViaReferent(q.name.clone()))?;
            queue.push(Queued {
                update: q.update,
                name: "HEAD".to_owned(),
                parents: Vec::new(),
                log_only: true,
            });
        }

        // A referent reached through two symbolic references is locked
        // once; like git, the second update may only leave it unchanged.
        let shared = !q.parents.is_empty() && self.prepared.items.iter().any(|i| i.name == q.name);
        let locked = if shared {
            Locked {
                lock: None,
                blocked: None,
                empty_dir: false,
            }
        } else {
            self.lock(&q.name).map_err(cannot_lock)?
        };
        let item = self.push_item(&q.name, q.update, locked.lock);
        let current = self
            .read(&q.name)
            .map_err(|e| cannot_lock(LockReason::Read(e)))?;
        if let (Some(Target::Symbolic(next)), false) = (&current, u.no_deref) {
            self.occupy(next, Origin::ViaSymref(q.name.clone()))?;
            let mut parents = q.parents.clone();
            parents.push(q.name.clone());
            queue.push(Queued {
                update: q.update,
                name: next.clone(),
                parents,
                log_only: false,
            });
            return Ok(());
        }
        let final_name = q.name.clone();
        self.final_names.push(final_name.clone());

        let old_oid = match &current {
            Some(Target::Object(id)) => Some(*id),
            Some(Target::Symbolic(t)) => self.resolve_oid(t),
            None => None,
        };
        let unreadable = match &current {
            Some(Target::Symbolic(t)) => matches!(
                self.store.resolve_with(t, &self.packed),
                Err(Error::TooDeep(_))
            ),
            _ => false,
        };
        if unreadable && u.old.is_some() {
            return Err(cannot_lock(LockReason::ErrorReading));
        }
        self.check_old(u, &final_name, current.as_ref(), old_oid)?;
        if current.is_none() {
            if let Some(reason) = locked.blocked {
                return Err(cannot_lock(reason));
            }
            self.creations.push(final_name.clone());
            // Empty directories where a missing reference belongs go now;
            // in front of an existing (packed) one they go when it is
            // written.
            if locked.empty_dir {
                let path = &self.prepared.items[item].path;
                std::fs::remove_dir_all(path).map_err(|source| TransactionError::Io {
                    path: path.clone(),
                    source,
                })?;
            }
        }

        let null = ObjectId::null(self.store.kind);
        let Some(new) = &u.new else {
            return Ok(());
        };
        let (new_oid, content) = match new {
            NewValue::Object(id) if id.is_null() => (null, None),
            NewValue::Object(id) => (*id, Some(loose::serialize(&Target::Object(*id)))),
            NewValue::Symbolic(t) => (
                self.resolve_oid(t).unwrap_or(null),
                Some(loose::serialize(&Target::Symbolic(t.clone()))),
            ),
        };
        let old_or_null = old_oid.unwrap_or(null);
        self.results[q.update] = Some(Outcome {
            old: old_or_null,
            new: new_oid,
            final_name: final_name.clone(),
            delete: content.is_none(),
        });
        let unchanged = match (new, &current) {
            (NewValue::Object(n), Some(Target::Object(o))) => n == o,
            (NewValue::Symbolic(n), Some(Target::Symbolic(o))) => n == o,
            _ => false,
        };
        let item = &mut self.prepared.items[item];
        if shared && (content.is_none() || !unchanged) {
            return Err(TransactionError::Rejected(messages::couldnt_write(
                &final_name,
                &format!("{}.lock", item.path.display()),
            )));
        }
        match content {
            // An unchanged value is not rewritten (a packed reference stays
            // packed), but still logged.
            Some(_) if unchanged => {}
            Some(bytes) => {
                if let Some(lock) = item.lock.as_mut() {
                    lock.write_all(&bytes)
                        .map_err(|source| TransactionError::Io {
                            path: lock.lock_path().to_owned(),
                            source,
                        })?;
                }
                item.action = Action::Write;
            }
            None => {
                let loose_exists = matches!(
                    self.store.read_loose(&final_name),
                    Ok(super::LooseRead::Value(_) | super::LooseRead::Broken)
                );
                item.action = Action::Delete {
                    loose: loose_exists,
                    missing: current.is_none(),
                };
            }
        }
        Ok(())
    }

    /// Attaches reflog entries to every locked name of each update: the
    /// symbolic references followed, the reference written, and `HEAD` when
    /// its branch changed. A deleted reference loses its log instead.
    fn collect_reflogs(&mut self, updates: &[RefUpdate]) {
        let opts = self.store.options.log_all_ref_updates;
        let mut hook_updates = Vec::new();
        for (k, u) in updates.iter().enumerate() {
            if let Some(o) = &self.results[k] {
                hook_updates.push((o.old, o.new, u.name.clone()));
            }
        }
        for item in &mut self.prepared.items {
            let Some(o) = &self.results[item.update] else {
                continue;
            };
            if o.delete && item.name == o.final_name {
                continue;
            }
            let u = &updates[item.update];
            item.reflogs.push(ReflogUpdate {
                name: item.name.clone(),
                old: o.old,
                new: o.new,
                message: u.message.clone(),
                create: u.create_reflog || opts.should_autocreate(&item.name),
            });
        }
        self.prepared.hook_updates = hook_updates;
    }

    fn check_old(
        &self,
        u: &RefUpdate,
        final_name: &str,
        current: Option<&Target>,
        old_oid: Option<ObjectId>,
    ) -> Result<(), TransactionError> {
        let fail = |reason: LockReason| {
            Err(TransactionError::CannotLock {
                name: u.name.clone(),
                reason,
            })
        };
        let Some(old) = &u.old else { return Ok(()) };
        let null = ObjectId::null(self.store.kind);
        // "Must not exist" behaves the same for both spellings.
        let old = match old {
            OldValue::SymbolicAbsent => &OldValue::Object(null),
            other => other,
        };
        match (old, current) {
            (OldValue::Object(x), None) if x.is_null() => Ok(()),
            (OldValue::Object(x), Some(Target::Symbolic(_))) if x.is_null() => {
                if old_oid.is_none() {
                    fail(LockReason::DanglingSymrefExists)
                } else {
                    fail(LockReason::AlreadyExists)
                }
            }
            (OldValue::Object(x), Some(_)) if x.is_null() => fail(LockReason::AlreadyExists),
            (OldValue::Object(_), None) => fail(LockReason::Unresolvable(final_name.to_owned())),
            (OldValue::Object(x), Some(Target::Symbolic(_))) => match old_oid {
                None => fail(LockReason::MissingButExpected(x.to_hex())),
                Some(o) if o != *x => fail(LockReason::IsAt {
                    actual: o.to_hex(),
                    expected: x.to_hex(),
                }),
                Some(_) => Ok(()),
            },
            (OldValue::Object(x), Some(Target::Object(o))) => {
                if o == x {
                    Ok(())
                } else {
                    fail(LockReason::IsAt {
                        actual: o.to_hex(),
                        expected: x.to_hex(),
                    })
                }
            }
            (OldValue::Symbolic(_), None) => fail(LockReason::Unresolvable(final_name.to_owned())),
            (OldValue::Symbolic(t), Some(Target::Object(_))) => {
                fail(LockReason::ExpectedSymref(t.clone()))
            }
            (OldValue::Symbolic(t), Some(Target::Symbolic(a))) => {
                if a == t {
                    Ok(())
                } else {
                    Err(TransactionError::SymrefMismatch {
                        name: final_name.to_owned(),
                        actual: a.clone(),
                        expected: t.clone(),
                    })
                }
            }
            (OldValue::SymbolicAbsent, _) => Ok(()),
        }
    }

    /// Conflicts of the references to be created with packed references
    /// and between names of the transaction, checked after every loose
    /// reference is locked.
    fn check_availability(&self, creations: &[String]) -> Result<(), TransactionError> {
        for name in creations {
            match self.availability(name, false) {
                None => {}
                Some(LockReason::NameConflict { existing, new }) => {
                    return Err(TransactionError::NameConflict { existing, new });
                }
                Some(LockReason::CannotProcessBoth { name, other }) => {
                    return Err(TransactionError::CannotProcessBoth { name, other });
                }
                Some(other) => {
                    return Err(TransactionError::CannotLock {
                        name: name.clone(),
                        reason: other,
                    });
                }
            }
        }
        Ok(())
    }
}

pub(super) fn prepare<'s>(
    store: &'s FilesStore,
    updates: Vec<RefUpdate>,
    hooks: &mut dyn RefHooks,
) -> Result<Prepared<'s>, TransactionError> {
    // Per update, in order: names, then the new object (git checks both
    // while queueing); duplicates only once everything is queued.
    for u in &updates {
        check_update_name(u)?;
        if let Some(NewValue::Object(id)) = &u.new {
            if !id.is_null() {
                hooks
                    .check_object(&u.name, id)
                    .map_err(TransactionError::Rejected)?;
            }
        }
    }
    check_duplicates(&updates)?;
    let packed = store.packed_refs()?;
    let head_referent = match store.read_loose_ref("HEAD") {
        Ok(Some(Target::Symbolic(t))) => Some(t),
        _ => None,
    };
    let mut state = State {
        store,
        hooks,
        packed,
        occupied: HashMap::new(),
        all_names: updates.iter().map(|u| u.name.clone()).collect(),
        final_names: Vec::new(),
        creations: Vec::new(),
        head_referent,
        results: updates.iter().map(|_| None).collect(),
        prepared: Prepared {
            store,
            items: Vec::new(),
            packed: None,
            hook_updates: Vec::new(),
        },
    };
    for u in &updates {
        state.occupy(&u.name, Origin::Direct)?;
    }
    let mut queue: Vec<Queued> = (0..updates.len())
        .map(|k| Queued {
            update: k,
            name: updates[k].name.clone(),
            parents: Vec::new(),
            log_only: false,
        })
        .collect();
    let mut index = 0;
    while index < queue.len() {
        let first_item = state.prepared.items.len();
        if let Err(e) = state.process(&updates, &mut queue, index) {
            // Like git: when the reference turned out to be missing, the
            // directories created for its lock stay. Otherwise every
            // directory created for a lock is pruned on rollback.
            let missing = matches!(
                e,
                TransactionError::CannotLock {
                    reason: LockReason::Unresolvable(_),
                    ..
                }
            );
            if missing {
                for item in state.prepared.items.drain(first_item..) {
                    if let Some(lock) = item.lock {
                        lock.rollback();
                    }
                }
            }
            return Err(e);
        }
        index += 1;
    }
    // Checked in processing order: the names of the updates, then the
    // referents of symbolic references.
    let creations = std::mem::take(&mut state.creations);
    state.check_availability(&creations)?;

    // Deletions of packed references rewrite packed-refs.
    let packed_deletes: Vec<String> = state
        .prepared
        .items
        .iter()
        .filter(|i| matches!(i.action, Action::Delete { .. }))
        .filter(|i| state.packed.find(&i.name).is_some())
        .map(|i| i.name.clone())
        .collect();
    if !packed_deletes.is_empty() {
        let opts = LockOptions::new()
            .timeout(store.options.packed_refs_timeout)
            .shared(store.options.shared)
            .fsync(store.options.fsync);
        let lock = LockFile::acquire(store.packed_refs_path(), &opts)
            .map_err(TransactionError::PackedLock)?;
        let mut packed = store.packed_refs()?;
        for name in &packed_deletes {
            packed.remove(name);
        }
        state.prepared.packed = Some((lock, packed));
    }

    state.collect_reflogs(&updates);
    let hook_updates = state.prepared.hook_updates.clone();
    state
        .hooks
        .reference_transaction(HookState::Prepared, &hook_updates)
        .map_err(TransactionError::Rejected)?;
    let State { prepared, .. } = state;
    Ok(prepared)
}
