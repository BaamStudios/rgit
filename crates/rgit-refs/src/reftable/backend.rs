// SPDX-License-Identifier: MIT
//! The reftable backend behind [`FilesStore`] and [`FilesReflog`]: a
//! repository whose git directory has a `reftable/` directory keeps its
//! references and reflogs in the stack there, and the two types forward
//! to this module.
//!
//! Every worktree has a stack of its own for its per-worktree references
//! (`HEAD`, other root references, `refs/bisect/`, `refs/worktree/`,
//! `refs/rewritten/`); all other references are in the stack of the common
//! directory.
//!
//! Root references other than `HEAD` (`ORIG_HEAD`, `CHERRY_PICK_HEAD`,
//! ...) are read from the stack first and then from a file of that name
//! in the git directory: commands that keep the state of a running
//! operation in such files find them through the reference store as
//! well. A transaction that writes such a reference stores it in the
//! stack and removes the file.
//!
//! A transaction writes one table per stack with the reference records and
//! the log records of all its updates, all with the same update index.
//! Reflog entries reach the transaction through the [`RefHooks`] of the
//! caller, which hand them to a [`FilesReflog`]; while a transaction
//! commits, that reflog collects the entries here ([`capture`]) instead of
//! writing a table for each of them.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rgit_fs::SharedRepository;
use rgit_hash::{HashKind, ObjectId};

use super::merged;
use super::record::{LogRecord, LogUpdate, RefRecord, RefValue};
use super::stack::{LockedStack, Stack, StackError, options};
use super::table::Table;
use super::writer::Writer;
use crate::error::{Error, LockReason, TransactionError};
use crate::files::FilesStore;
use crate::name::{RefNameFlags, is_per_worktree, is_pseudo_ref, is_root_ref, is_valid_refname};
use crate::reflog::Entry;
use crate::store::{
    HookState, IterItem, IterOptions, MAX_SYMREF_DEPTH, NewValue, OldValue, PreparedTransaction,
    RefHooks, RefStore, RefUpdate, ReflogUpdate, check_duplicates, check_update_name,
};
use crate::target::{Peeled, Reference, Source, Target};

/// The name of the directory that holds a stack.
pub const REFTABLE_DIR: &str = "reftable";

/// Whether the git directory `dir` stores its references as reftable: it
/// has a `reftable/` directory, and `refs/heads` is not a directory (a
/// reftable repository keeps a file there, see [`create`]).
pub fn uses_reftable(dir: &Path) -> bool {
    dir.join(REFTABLE_DIR).is_dir() && !dir.join("refs").join("heads").is_dir()
}

/// `fatal: cannot lock references`
pub const CANNOT_LOCK_REFERENCES: &str = "cannot lock references";

fn read_error(dir: &Path, e: StackError) -> Error {
    match e {
        StackError::Io { path, source } => Error::Io { path, source },
        other => Error::Io {
            path: dir.to_owned(),
            source: io::Error::other(other.to_string()),
        },
    }
}

fn write_error(e: StackError) -> TransactionError {
    match e {
        StackError::Locked => TransactionError::Rejected(CANNOT_LOCK_REFERENCES.to_owned()),
        StackError::Write(w) => {
            TransactionError::Rejected(format!("reftable: transaction failure: {w}"))
        }
        StackError::Io { path, source } => TransactionError::Io { path, source },
        other => TransactionError::Rejected(format!("reftable: transaction failure: {other}")),
    }
}

/// The raw bytes of the reference name `name` (see [`rgit_bstr`]).
fn raw(name: &str) -> Vec<u8> {
    rgit_bstr::to_bytes(name).into_owned()
}

/// A reference name read from a table, as rgit carries it.
fn text(raw: &[u8]) -> String {
    rgit_bstr::from_bytes(raw).into_owned()
}

fn target_of(rec: &RefRecord) -> Option<Target> {
    match &rec.value {
        RefValue::Deletion => None,
        RefValue::Direct(id) | RefValue::Peeled(id, _) => Some(Target::Object(*id)),
        RefValue::Symbolic(t) => Some(Target::Symbolic(text(t))),
    }
}

/// Whether a symbolic record names something that cannot be a reference;
/// such a record counts as broken, like a loose file with that content.
fn broken_symref(rec: &RefRecord) -> bool {
    matches!(&rec.value, RefValue::Symbolic(t) if !is_valid_refname(t, RefNameFlags::ONELEVEL))
}

/// The log record data of a reflog entry. git stores the message with a
/// line feed at its end, and no more of it than half a block so that
/// the record fits into one.
fn update_of(entry: &Entry) -> LogUpdate {
    let mut message = entry.message.clone();
    message.truncate((options().block_size / 2) as usize);
    message.push(b'\n');
    LogUpdate {
        old: entry.old,
        new: entry.new,
        name: entry.name().to_vec(),
        email: entry.email().to_vec(),
        time: entry.time,
        tz_offset: entry.tz as i16,
        message,
    }
}

fn entry_of(update: &LogUpdate) -> Entry {
    let mut ident = update.name.clone();
    ident.extend_from_slice(b" <");
    ident.extend_from_slice(&update.email);
    ident.push(b'>');
    let mut message = update.message.clone();
    if message.last() == Some(&b'\n') {
        message.pop();
    }
    Entry {
        old: update.old,
        new: update.new,
        ident,
        time: update.time,
        tz: i32::from(update.tz_offset),
        message,
    }
}

/// The record that makes a log exist without an entry: null ids, no
/// identity, no message.
fn existence_marker(name: &[u8], update_index: u64, kind: HashKind) -> LogRecord {
    LogRecord {
        refname: name.to_vec(),
        update_index,
        update: Some(LogUpdate {
            old: kind.null(),
            new: kind.null(),
            name: Vec::new(),
            email: Vec::new(),
            time: 0,
            tz_offset: 0,
            message: Vec::new(),
        }),
    }
}

fn is_marker(rec: &LogRecord) -> bool {
    rec.update
        .as_ref()
        .is_some_and(|u| u.old.is_null() && u.new.is_null())
}

// --- reflog entries collected during a transaction ----------------------

/// What a committing transaction collects from the reflog hooks.
#[derive(Default)]
pub(crate) struct Capture {
    /// `(name, entry, create the log if missing)`.
    pub appends: Vec<(String, Entry, bool)>,
    /// Names whose logs are deleted.
    pub deletes: Vec<String>,
}

thread_local! {
    static CAPTURE: RefCell<Option<Capture>> = const { RefCell::new(None) };
}

fn capture_begin() {
    CAPTURE.with(|c| *c.borrow_mut() = Some(Capture::default()));
}

fn capture_end() -> Capture {
    CAPTURE.with(|c| c.borrow_mut().take()).unwrap_or_default()
}

/// Hands an entry to the committing transaction, if there is one.
fn capture_append(name: &str, entry: &Entry, create: bool) -> bool {
    CAPTURE.with(|c| match c.borrow_mut().as_mut() {
        Some(cap) => {
            cap.appends.push((name.to_owned(), entry.clone(), create));
            true
        }
        None => false,
    })
}

fn capture_delete(name: &str) -> bool {
    CAPTURE.with(|c| match c.borrow_mut().as_mut() {
        Some(cap) => {
            cap.deletes.push(name.to_owned());
            true
        }
        None => false,
    })
}

/// Whether `name` (inside its stack) is a root reference that may still
/// be kept as a file in the git directory.
fn may_be_file(name: &str) -> bool {
    name != "HEAD" && crate::name::is_root_ref_syntax(name)
}

// --- reading -------------------------------------------------------------

impl FilesStore {
    /// The stack that holds `name` and the name inside it.
    pub(crate) fn rt_stack<'n>(&self, name: &'n str) -> (Arc<Stack>, &'n str) {
        let (dir, rel) = self.locate(name);
        (Stack::open(dir.join(REFTABLE_DIR), self.hash_kind()), rel)
    }

    /// The stack of this worktree's own references.
    pub(crate) fn rt_worktree_stack(&self) -> Arc<Stack> {
        self.rt_stack("HEAD").0
    }

    fn rt_tables(&self, stack: &Stack) -> Result<Vec<Arc<Table>>, Error> {
        stack.tables().map_err(|e| read_error(stack.dir(), e))
    }

    pub(crate) fn rt_read_raw(&self, name: &str) -> Result<Option<Target>, Error> {
        let (stack, rel) = self.rt_stack(name);
        let tables = self.rt_tables(&stack)?;
        let rec =
            merged::read_ref(&tables, &raw(rel)).map_err(|e| read_error(stack.dir(), e.into()))?;
        if rec.as_ref().is_some_and(broken_symref) {
            return Err(Error::Broken(name.to_owned()));
        }
        match rec.as_ref().and_then(target_of) {
            None if may_be_file(rel) => Ok(self.loose_value(name)),
            found => Ok(found),
        }
    }

    fn rt_item(&self, rec: RefRecord) -> Option<IterItem> {
        let broken = broken_symref(&rec);
        let name = rgit_bstr::from_vec(rec.name);
        if !is_valid_refname(name.as_bytes(), RefNameFlags::ONELEVEL) {
            return Some(IterItem::BadName(name));
        }
        if broken {
            return Some(IterItem::Broken(name));
        }
        let (target, oid, peeled) = match rec.value {
            RefValue::Deletion => return None,
            RefValue::Direct(id) => (Target::Object(id), Some(id), Peeled::Unknown),
            RefValue::Peeled(id, p) => (Target::Object(id), Some(id), Peeled::Tag(p)),
            RefValue::Symbolic(t) => {
                let oid = match self.resolve(&name) {
                    Ok(Some(r)) => r.oid,
                    Ok(None) | Err(Error::TooDeep(_)) => None,
                    Err(_) => return Some(IterItem::Broken(name)),
                };
                let t = text(&t);
                (Target::Symbolic(t), oid, Peeled::Unknown)
            }
        };
        Some(IterItem::Ref(Reference {
            name,
            target,
            oid,
            peeled,
            source: Source::Loose,
        }))
    }

    pub(crate) fn rt_iter(&self, options: &IterOptions) -> Result<Vec<IterItem>, Error> {
        let prefix = options.prefix.as_str();
        let linked = self.linked();
        let mut items: BTreeMap<Vec<u8>, IterItem> = BTreeMap::new();
        let common = Stack::open(self.git_dir().join(REFTABLE_DIR), self.hash_kind());
        let own = self.rt_worktree_stack();
        let mut collect = |stack: &Arc<Stack>, from: &str, own_refs: bool| -> Result<(), Error> {
            let tables = self.rt_tables(stack)?;
            let recs = merged::refs_with_prefix(&tables, &raw(from))
                .map_err(|e| read_error(stack.dir(), e.into()))?;
            for rec in recs {
                let name = text(&rec.name);
                let below_refs = name.starts_with("refs/");
                let keep = if !below_refs {
                    own_refs && options.include_root_refs && is_root_ref(&name)
                } else if linked {
                    // Per-worktree names come from the worktree's stack only.
                    own_refs == is_per_worktree(&name)
                } else {
                    true
                };
                if !keep || !name.starts_with(prefix) {
                    continue;
                }
                let key = rec.name.clone();
                if let Some(item) = self.rt_item(rec) {
                    items.insert(key, item);
                }
            }
            Ok(())
        };
        if prefix.starts_with("refs/") || !options.include_root_refs {
            let from = if prefix.starts_with("refs/") {
                prefix
            } else {
                "refs/"
            };
            collect(&common, from, !linked)?;
            if linked {
                collect(&own, from, true)?;
            }
        } else {
            // Root references sort in front of `refs/`; read everything.
            collect(&common, "", !linked)?;
            if linked {
                collect(&own, "", true)?;
            }
            // Root references kept as files.
            let (dir, _) = self.locate("HEAD");
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let Ok(name) = entry.file_name().into_string() else {
                    continue;
                };
                if !may_be_file(&name)
                    || !is_root_ref(&name)
                    || is_pseudo_ref(&name)
                    || !name.starts_with(prefix)
                    || items.contains_key(name.as_bytes())
                {
                    continue;
                }
                if let Some(target) = self.loose_value(&name) {
                    let (oid, target) = match target {
                        Target::Object(id) => (Some(id), Target::Object(id)),
                        Target::Symbolic(t) => (
                            self.resolve(&name).ok().flatten().and_then(|r| r.oid),
                            Target::Symbolic(t),
                        ),
                    };
                    items.insert(
                        name.clone().into_bytes(),
                        IterItem::Ref(Reference {
                            name,
                            target,
                            oid,
                            peeled: Peeled::Unknown,
                            source: Source::Loose,
                        }),
                    );
                }
            }
        }
        Ok(items.into_values().collect())
    }
}

// --- transactions --------------------------------------------------------

/// How a name entered the transaction.
#[derive(Clone, Debug)]
enum Origin {
    Direct,
    ViaSymref(String),
    ViaReferent(String),
}

struct Item {
    name: String,
    update: usize,
}

#[derive(Clone, Debug)]
struct Queued {
    update: usize,
    name: String,
    parents: Vec<String>,
    log_only: bool,
}

struct Outcome {
    old: ObjectId,
    new: ObjectId,
    final_name: String,
    delete: bool,
    /// The deleted reference was a symbolic one (deleted itself).
    symref_delete: bool,
}

pub(crate) struct Prepared<'s> {
    store: &'s FilesStore,
    /// The locked stacks, by directory.
    stacks: Vec<(PathBuf, LockedStack)>,
    /// `(stack, name inside the stack, value)`.
    writes: Vec<(usize, String, RefValue)>,
    reflogs: Vec<ReflogUpdate>,
    /// Names whose logs go away with the reference.
    log_deletes: Vec<String>,
    /// Files of root references that the transaction replaces or deletes.
    stale_files: Vec<PathBuf>,
    hook_updates: Vec<(String, String, String)>,
}

struct State<'s, 'h> {
    store: &'s FilesStore,
    hooks: &'h mut dyn RefHooks,
    occupied: HashMap<String, Origin>,
    all_names: Vec<String>,
    final_names: Vec<String>,
    creations: Vec<String>,
    head_referent: Option<String>,
    results: Vec<Option<Outcome>>,
    items: Vec<Item>,
    prepared: Prepared<'s>,
}

impl<'s> Prepared<'s> {
    /// Locks the stack in `dir` unless this transaction holds it already.
    fn stack_index(&mut self, stack: &Arc<Stack>) -> Result<usize, TransactionError> {
        if let Some(i) = self.stacks.iter().position(|(d, _)| d == stack.dir()) {
            return Ok(i);
        }
        let locked = stack
            .lock(self.store.options().shared)
            .map_err(write_error)?
            .fsync(self.store.options().fsync);
        self.stacks.push((stack.dir().to_owned(), locked));
        Ok(self.stacks.len() - 1)
    }

    /// Reads `name` from its stack as locked by this transaction.
    fn read(&mut self, name: &str) -> Result<Option<Target>, TransactionError> {
        if is_pseudo_ref(name) {
            return Ok(self.store.read_raw(name)?);
        }
        let (stack, rel) = self.store.rt_stack(name);
        let i = self.stack_index(&stack)?;
        let rec = merged::read_ref(self.stacks[i].1.tables(), &raw(rel))
            .map_err(|e| write_error(e.into()))?;
        // A broken record counts as absent, so that it can be replaced.
        if rec.as_ref().is_some_and(broken_symref) {
            return Ok(None);
        }
        match rec.as_ref().and_then(target_of) {
            None if may_be_file(rel) => Ok(self.store.loose_value(name)),
            found => Ok(found),
        }
    }

    /// Follows symbolic references from `name`: the object at the end,
    /// `Err(())` for a chain that is too deep.
    fn resolve_oid(&mut self, name: &str) -> Result<Option<ObjectId>, ()> {
        let mut current = name.to_owned();
        for _ in 0..MAX_SYMREF_DEPTH {
            match self.read(&current) {
                Ok(Some(Target::Object(id))) => return Ok(Some(id)),
                Ok(Some(Target::Symbolic(next))) => current = next,
                Ok(None) | Err(_) => return Ok(None),
            }
        }
        Err(())
    }

    /// The first live reference of the stack of `name` below `prefix`.
    fn first_below(&mut self, prefix: &str) -> Result<Option<String>, TransactionError> {
        let (stack, rel) = self.store.rt_stack(prefix);
        let outer = &prefix[..prefix.len() - rel.len()];
        let i = self.stack_index(&stack)?;
        let rel_raw = raw(rel);
        let mut it = merged::merged_refs(self.stacks[i].1.tables(), &rel_raw)
            .map_err(|e| write_error(e.into()))?;
        while let Some(rec) = it.next_record().map_err(|e| write_error(e.into()))? {
            if !rec.name.starts_with(&rel_raw) {
                break;
            }
            if !rec.is_deletion() {
                return Ok(Some(format!("{outer}{}", text(&rec.name))));
            }
        }
        Ok(None)
    }
}

/// Names that `name` would need as directories.
fn dir_prefixes(name: &str) -> impl Iterator<Item = &str> {
    name.match_indices('/').map(move |(i, _)| &name[..i])
}

impl State<'_, '_> {
    fn occupy(&mut self, name: &str, origin: Origin) -> Result<(), TransactionError> {
        if let Some(existing) = self.occupied.get(name) {
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
            self.items.push(Item {
                name: q.name.clone(),
                update: q.update,
            });
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
        self.items.push(Item {
            name: q.name.clone(),
            update: q.update,
        });
        let current = self.prepared.read(&q.name)?;
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

        let (old_oid, unreadable) = match &current {
            Some(Target::Object(id)) => (Some(*id), false),
            Some(Target::Symbolic(t)) => match self.prepared.resolve_oid(t) {
                Ok(o) => (o, false),
                Err(()) => (None, true),
            },
            None => (None, false),
        };
        if unreadable && u.old.is_some() {
            return Err(cannot_lock(LockReason::ErrorReading));
        }
        crate::files::check_old(
            self.store.hash_kind(),
            u,
            &final_name,
            current.as_ref(),
            old_oid,
        )?;
        if current.is_none() {
            self.creations.push(final_name.clone());
        }

        let null = ObjectId::null(self.store.hash_kind());
        let Some(new) = &u.new else {
            return Ok(());
        };
        let (new_oid, value) = match new {
            NewValue::Object(id) if id.is_null() => (null, RefValue::Deletion),
            NewValue::Object(id) => (
                *id,
                match self.hooks.peel(id) {
                    Some(p) => RefValue::Peeled(*id, p),
                    None => RefValue::Direct(*id),
                },
            ),
            NewValue::Symbolic(t) => (
                self.prepared.resolve_oid(t).ok().flatten().unwrap_or(null),
                RefValue::Symbolic(raw(t)),
            ),
        };
        let delete = value == RefValue::Deletion;
        self.results[q.update] = Some(Outcome {
            old: old_oid.unwrap_or(null),
            new: new_oid,
            final_name: final_name.clone(),
            delete,
            symref_delete: delete && matches!(current, Some(Target::Symbolic(_))),
        });
        let unchanged = match (new, &current) {
            (NewValue::Object(n), Some(Target::Object(o))) => n == o,
            (NewValue::Symbolic(n), Some(Target::Symbolic(o))) => n == o,
            _ => false,
        };
        // Neither an unchanged value nor the deletion of a reference that
        // does not exist is written.
        if unchanged || (delete && current.is_none()) {
            return Ok(());
        }
        let (stack, rel) = self.store.rt_stack(&final_name);
        let i = self.prepared.stack_index(&stack)?;
        if may_be_file(rel) {
            let file = self.store.ref_path(&final_name);
            if file.is_file() {
                self.prepared.stale_files.push(file);
            }
        }
        self.prepared.writes.push((i, rel.to_owned(), value));
        Ok(())
    }

    /// Whether `name` can be created next to the existing references and
    /// the other names of the transaction.
    fn availability(&mut self, name: &str) -> Result<(), TransactionError> {
        let others: Vec<String> = self
            .all_names
            .iter()
            .chain(&self.final_names)
            .filter(|n| n.as_str() != name)
            .cloned()
            .collect();
        for prefix in dir_prefixes(name) {
            if self.prepared.read(prefix)?.is_some() {
                return Err(TransactionError::NameConflict {
                    existing: prefix.to_owned(),
                    new: name.to_owned(),
                });
            }
            if others.iter().any(|n| n == prefix) {
                return Err(TransactionError::CannotProcessBoth {
                    name: name.to_owned(),
                    other: prefix.to_owned(),
                });
            }
        }
        let below = format!("{name}/");
        if let Some(existing) = self.prepared.first_below(&below)? {
            return Err(TransactionError::NameConflict {
                existing,
                new: name.to_owned(),
            });
        }
        if let Some(other) = others.iter().find(|n| n.starts_with(&below)) {
            return Err(TransactionError::CannotProcessBoth {
                name: name.to_owned(),
                other: other.clone(),
            });
        }
        Ok(())
    }

    /// Reflog entries for every name of each update, and the lines for
    /// the `reference-transaction` hook.
    fn collect_reflogs(&mut self, updates: &[RefUpdate]) {
        let opts = self.store.options().log_all_ref_updates;
        let zero = ObjectId::null(self.store.hash_kind()).to_hex();
        let mut hook_updates = Vec::new();
        for (k, o) in self.results.iter().enumerate() {
            if updates[k].new.is_none() {
                let old = match &updates[k].old {
                    Some(OldValue::Object(id)) => id.to_hex(),
                    Some(OldValue::Symbolic(t)) => format!("ref:{t}"),
                    _ => continue,
                };
                hook_updates.push((old, zero.clone(), updates[k].name.clone()));
                continue;
            }
            let Some(o) = o else { continue };
            let old = match &updates[k].old {
                Some(OldValue::Object(id)) => id.to_hex(),
                Some(OldValue::Symbolic(t)) => format!("ref:{t}"),
                _ => zero.clone(),
            };
            let new = match &updates[k].new {
                Some(NewValue::Symbolic(t)) => format!("ref:{t}"),
                _ => o.new.to_hex(),
            };
            hook_updates.push((old, new, o.final_name.clone()));
        }
        for item in &self.items {
            let Some(o) = &self.results[item.update] else {
                continue;
            };
            if o.delete && item.name == o.final_name && !o.symref_delete {
                if !self.prepared.log_deletes.contains(&item.name) {
                    self.prepared.log_deletes.push(item.name.clone());
                }
                continue;
            }
            let u = &updates[item.update];
            self.prepared.reflogs.push(ReflogUpdate {
                name: item.name.clone(),
                old: o.old,
                new: o.new,
                message: u.message.clone(),
                create: u.create_reflog || opts.should_autocreate(&item.name),
            });
        }
        self.prepared.hook_updates = hook_updates;
    }
}

pub(crate) fn prepare<'s>(
    store: &'s FilesStore,
    updates: Vec<RefUpdate>,
    hooks: &mut dyn RefHooks,
) -> Result<Prepared<'s>, TransactionError> {
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
    let queued = crate::store::queued_hook_lines(&updates, store.hash_kind());
    hooks
        .reference_transaction(HookState::Preparing, &queued)
        .map_err(TransactionError::Rejected)?;
    let mut prepared = Prepared {
        store,
        stacks: Vec::new(),
        writes: Vec::new(),
        reflogs: Vec::new(),
        log_deletes: Vec::new(),
        stale_files: Vec::new(),
        hook_updates: Vec::new(),
    };
    // Every transaction takes the lock of its worktree's stack, also one
    // that changes nothing.
    let own = store.rt_worktree_stack();
    prepared.stack_index(&own)?;
    let head_referent = match prepared.read("HEAD") {
        Ok(Some(Target::Symbolic(t))) => Some(t),
        _ => None,
    };
    let mut state = State {
        store,
        hooks,
        occupied: HashMap::new(),
        all_names: updates.iter().map(|u| u.name.clone()).collect(),
        final_names: Vec::new(),
        creations: Vec::new(),
        head_referent,
        results: updates.iter().map(|_| None).collect(),
        items: Vec::new(),
        prepared,
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
        state.process(&updates, &mut queue, index)?;
        index += 1;
    }
    for name in std::mem::take(&mut state.creations) {
        state.availability(&name)?;
    }
    state.collect_reflogs(&updates);
    let hook_updates = state.prepared.hook_updates.clone();
    state
        .hooks
        .reference_transaction(HookState::Prepared, &hook_updates)
        .map_err(TransactionError::Rejected)?;
    Ok(state.prepared)
}

impl Prepared<'_> {
    /// Writes one table per stack and releases the locks.
    fn write(&mut self, capture: Capture) -> Result<(), TransactionError> {
        let kind = self.store.hash_kind();
        let shared = self.store.options().shared;
        let stacks = std::mem::take(&mut self.stacks);
        let writes = std::mem::take(&mut self.writes);
        let mut touched = Vec::new();
        for (i, (_, locked)) in stacks.into_iter().enumerate() {
            let index = locked.next_update_index();
            let mut refs: BTreeMap<Vec<u8>, RefValue> = BTreeMap::new();
            for (stack, name, value) in &writes {
                if *stack == i {
                    refs.insert(raw(name), value.clone());
                }
            }
            let mut logs: BTreeMap<Vec<u8>, LogRecord> = BTreeMap::new();
            let of_stack = |name: &str| -> Option<String> {
                let (stack, rel) = self.store.rt_stack(name);
                (stack.dir() == locked.stack().dir()).then(|| rel.to_owned())
            };
            for name in &capture.deletes {
                let Some(rel) = of_stack(name) else { continue };
                let old = merged::logs_of(locked.tables(), &raw(&rel))
                    .map_err(|e| write_error(e.into()))?;
                for mut rec in old {
                    rec.update = None;
                    logs.insert(rec.key(), rec);
                }
            }
            for (name, entry, create) in &capture.appends {
                let Some(rel) = of_stack(name) else { continue };
                if !*create {
                    let has = !merged::logs_of(locked.tables(), &raw(&rel))
                        .map_err(|e| write_error(e.into()))?
                        .is_empty();
                    if !has {
                        continue;
                    }
                }
                let rec = LogRecord {
                    refname: raw(&rel),
                    update_index: index,
                    update: Some(update_of(entry)),
                };
                logs.insert(rec.key(), rec);
            }
            if refs.is_empty() && logs.is_empty() {
                continue;
            }
            let mut writer = Writer::new(locked.write_options(), index, index);
            for (name, value) in refs {
                writer
                    .add_ref(&RefRecord {
                        name,
                        update_index: index,
                        value,
                    })
                    .map_err(|e| write_error(e.into()))?;
            }
            for rec in logs.into_values() {
                writer.add_log(&rec).map_err(|e| write_error(e.into()))?;
            }
            let data = writer.finish().map_err(|e| write_error(e.into()))?;
            let stack = Arc::clone(locked.stack());
            locked.add_table(&data, index, index).map_err(write_error)?;
            touched.push(stack);
        }
        let _ = kind;
        if options().auto_compact {
            for stack in touched {
                // A failed compaction leaves a longer but valid stack.
                let _ = stack.auto_compact(shared);
            }
        }
        Ok(())
    }
}

impl PreparedTransaction for Prepared<'_> {
    fn commit(mut self: Box<Self>, hooks: &mut dyn RefHooks) -> Result<(), TransactionError> {
        capture_begin();
        for log in &self.reflogs {
            if let Err(msg) = hooks.reflog_append(log) {
                capture_end();
                self.stacks.clear();
                return Err(TransactionError::Rejected(msg));
            }
        }
        for name in &self.log_deletes {
            hooks.reflog_delete(name);
        }
        let mut capture = capture_end();
        // The log of a deleted reference goes with it, whoever the hooks
        // are.
        for name in &self.log_deletes {
            if !capture.deletes.contains(name) {
                capture.deletes.push(name.clone());
            }
        }
        self.write(capture)?;
        for file in &self.stale_files {
            let _ = std::fs::remove_file(file);
        }
        let _ = hooks.reference_transaction(HookState::Committed, &self.hook_updates);
        Ok(())
    }

    fn abort(mut self: Box<Self>, hooks: &mut dyn RefHooks) {
        self.stacks.clear();
        let _ = hooks.reference_transaction(HookState::Aborted, &self.hook_updates);
    }
}

// --- reflogs -------------------------------------------------------------

/// The reflogs of one worktree's view of a reftable repository.
pub(crate) struct Logs<'a> {
    /// Directory holding the stack of `name`, and the name inside it.
    pub locate: &'a dyn Fn(&str) -> (PathBuf, String),
    pub kind: HashKind,
    pub shared: SharedRepository,
}

fn io_error(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

impl Logs<'_> {
    fn stack(&self, name: &str) -> (Arc<Stack>, String) {
        let (dir, rel) = (self.locate)(name);
        (Stack::open(dir.join(REFTABLE_DIR), self.kind), rel)
    }

    fn records(&self, name: &str) -> io::Result<Vec<LogRecord>> {
        let (stack, rel) = self.stack(name);
        let tables = stack.tables().map_err(io_error)?;
        merged::logs_of(&tables, &raw(&rel)).map_err(io_error)
    }

    pub fn exists(&self, name: &str) -> bool {
        self.records(name).is_ok_and(|r| !r.is_empty())
    }

    /// The entries of `name`, oldest first; `None` without a log.
    pub fn read(&self, name: &str) -> io::Result<Option<Vec<Entry>>> {
        let records = self.records(name)?;
        if records.is_empty() {
            return Ok(None);
        }
        Ok(Some(
            records
                .iter()
                .rev()
                .filter(|r| !is_marker(r))
                .filter_map(|r| r.update.as_ref().map(entry_of))
                .collect(),
        ))
    }

    /// Locks the stack of `name`, lets `build` choose the records (given
    /// the locked stack, the name inside it and the next update index)
    /// and writes them as one table covering `index..=max`.
    fn write(
        &self,
        name: &str,
        build: impl FnOnce(&LockedStack, &[u8], u64) -> io::Result<(Vec<LogRecord>, u64)>,
    ) -> io::Result<()> {
        let (stack, rel) = self.stack(name);
        let locked = stack.lock(self.shared).map_err(io_error)?;
        let index = locked.next_update_index();
        let (mut records, max) = build(&locked, &raw(&rel), index)?;
        if records.is_empty() {
            return Ok(());
        }
        records.sort_by_key(LogRecord::key);
        records.dedup_by_key(|r| r.key());
        let mut writer = Writer::new(locked.write_options(), index, max.max(index));
        for rec in &records {
            writer.add_log(rec).map_err(io_error)?;
        }
        let data = writer.finish().map_err(io_error)?;
        locked
            .add_table(&data, index, max.max(index))
            .map_err(io_error)?;
        if options().auto_compact {
            let _ = stack.auto_compact(self.shared);
        }
        Ok(())
    }

    /// Appends `entry`; without `create` only to an existing log. Returns
    /// whether the entry was taken.
    pub fn append(&self, name: &str, entry: &Entry, create: bool) -> io::Result<bool> {
        if capture_append(name, entry, create) {
            return Ok(true);
        }
        let mut written = false;
        self.write(name, |locked, rel, index| {
            if !create
                && merged::logs_of(locked.tables(), rel)
                    .map_err(io_error)?
                    .is_empty()
            {
                return Ok((Vec::new(), index));
            }
            written = true;
            Ok((
                vec![LogRecord {
                    refname: rel.to_vec(),
                    update_index: index,
                    update: Some(update_of(entry)),
                }],
                index,
            ))
        })?;
        Ok(written)
    }

    /// Makes the log of `name` exist.
    pub fn create(&self, name: &str) -> io::Result<()> {
        let kind = self.kind;
        self.write(name, |locked, rel, index| {
            if !merged::logs_of(locked.tables(), rel)
                .map_err(io_error)?
                .is_empty()
            {
                return Ok((Vec::new(), index));
            }
            Ok((vec![existence_marker(rel, index, kind)], index))
        })
    }

    /// Removes every entry of `name`.
    pub fn delete(&self, name: &str) -> io::Result<()> {
        if capture_delete(name) {
            return Ok(());
        }
        self.write(name, |locked, rel, index| {
            let mut old = merged::logs_of(locked.tables(), rel).map_err(io_error)?;
            for rec in &mut old {
                rec.update = None;
            }
            Ok((old, index))
        })
    }

    /// Replaces the log of `name` by `entries` (oldest first). Entries
    /// that are kept stay at their update index; an empty log keeps
    /// existing through a marker.
    pub fn write_entries(&self, name: &str, entries: &[Entry]) -> io::Result<()> {
        let kind = self.kind;
        self.write(name, |locked, rel, index| {
            let old = merged::logs_of(locked.tables(), rel).map_err(io_error)?;
            // Oldest first, like `entries`.
            let old: Vec<&LogRecord> = old.iter().rev().filter(|r| !is_marker(r)).collect();
            let same = |rec: &LogRecord, e: &Entry| {
                rec.update.as_ref().is_some_and(|u| {
                    let o = entry_of(u);
                    o.new == e.new
                        && o.time == e.time
                        && o.tz == e.tz
                        && o.ident == e.ident
                        && o.message == e.message
                })
            };
            // The new entries are a subsequence of the old ones when
            // entries were only dropped (and old values rewritten).
            let mut matched = Vec::with_capacity(entries.len());
            let mut at = 0;
            for e in entries {
                match (at..old.len()).find(|&i| same(old[i], e)) {
                    Some(i) => {
                        matched.push(i);
                        at = i + 1;
                    }
                    None => break,
                }
            }
            let mut out = Vec::new();
            let mut max = index;
            if matched.len() == entries.len() {
                for (i, rec) in old.iter().enumerate() {
                    match matched.iter().position(|&m| m == i) {
                        Some(k) => out.push(LogRecord {
                            refname: rel.to_vec(),
                            update_index: rec.update_index,
                            update: Some(update_of(&entries[k])),
                        }),
                        None => out.push(LogRecord {
                            refname: rel.to_vec(),
                            update_index: rec.update_index,
                            update: None,
                        }),
                    }
                }
            } else {
                // Something else changed: start over with new indices.
                for rec in &old {
                    out.push(LogRecord {
                        refname: rel.to_vec(),
                        update_index: rec.update_index,
                        update: None,
                    });
                }
                for (k, e) in entries.iter().enumerate() {
                    max = index + k as u64;
                    out.push(LogRecord {
                        refname: rel.to_vec(),
                        update_index: max,
                        update: Some(update_of(e)),
                    });
                }
            }
            if entries.is_empty() {
                out.push(existence_marker(rel, index, kind));
            }
            Ok((out, max))
        })
    }

    /// Gives `to` the entries of `from`, at the same update indices;
    /// with `remove`, `from` loses them. Both names must be in one stack.
    pub fn transfer(&self, from: &str, to: &str, remove: bool) -> io::Result<()> {
        let (_, to_rel) = self.stack(to);
        self.write(from, |locked, rel, index| {
            let old = merged::logs_of(locked.tables(), rel).map_err(io_error)?;
            let mut out = Vec::new();
            // Entries the target still has would shine through.
            for mut rec in merged::logs_of(locked.tables(), &raw(&to_rel)).map_err(io_error)? {
                rec.update = None;
                out.push(rec);
            }
            for rec in old {
                let mut copy = rec.clone();
                copy.refname = raw(&to_rel);
                out.retain(|r: &LogRecord| r.key() != copy.key());
                out.push(copy);
                if remove {
                    let mut gone = rec;
                    gone.update = None;
                    out.push(gone);
                }
            }
            Ok((out, index))
        })
    }

    /// Names with a log in the stack of `dir`.
    pub fn names_in(&self, dir: &Path) -> io::Result<Vec<String>> {
        let stack = Stack::open(dir.join(REFTABLE_DIR), self.kind);
        let tables = stack.tables().map_err(io_error)?;
        Ok(merged::log_names(&tables)
            .map_err(io_error)?
            .into_iter()
            .map(rgit_bstr::from_vec)
            .collect())
    }
}

// --- renaming and copying ------------------------------------------------

impl FilesStore {
    /// Renames (or with `copy` copies) the direct reference `old` to `new`
    /// in a reftable repository, the way git records it: one table in
    /// which `old` is deleted at one update index and `new` created at the
    /// next (a copy only creates), the log entries of `old` reappear
    /// under `new`, and `new` gets an entry for each step, made by
    /// `committer` with `message`. Entries that `new` already had stay.
    /// `HEAD` pointing to `old` is not changed, only its log gets the
    /// deletion.
    pub fn rename_reftable(
        &self,
        old: &str,
        new: &str,
        committer: &crate::reflog::Committer,
        message: &str,
        copy: bool,
    ) -> Result<(), TransactionError> {
        let cannot_lock = |reason: LockReason| TransactionError::CannotLock {
            name: old.to_owned(),
            reason,
        };
        let kind = self.hash_kind();
        let null = kind.null();
        let (stack, old_rel) = self.rt_stack(old);
        let (_, new_rel) = self.rt_stack(new);
        let locked = stack
            .lock(self.options().shared)
            .map_err(write_error)?
            .fsync(self.options().fsync);
        let tables = locked.tables().to_vec();
        let read = |name: &str| merged::read_ref(&tables, &raw(name));
        let fmt = |e: super::FormatError| write_error(e.into());
        let value = match read(old_rel).map_err(fmt)? {
            Some(RefRecord {
                value: v @ (RefValue::Direct(_) | RefValue::Peeled(..)),
                ..
            }) => v,
            _ => return Err(cannot_lock(LockReason::Unresolvable(old.to_owned()))),
        };
        let oid = match &value {
            RefValue::Direct(id) | RefValue::Peeled(id, _) => *id,
            _ => null,
        };
        // The new name must not need an existing reference as a directory
        // nor be one for existing references; the name that goes away
        // does not count.
        let counts = |name: &str| copy || name != old_rel;
        for prefix in dir_prefixes(new_rel) {
            if counts(prefix) && read(prefix).map_err(fmt)?.is_some() {
                return Err(TransactionError::NameConflict {
                    existing: prefix.to_owned(),
                    new: new.to_owned(),
                });
            }
        }
        let below = format!("{new_rel}/");
        for rec in merged::refs_with_prefix(&tables, &raw(&below)).map_err(fmt)? {
            let name = text(&rec.name);
            if counts(&name) {
                return Err(TransactionError::NameConflict {
                    existing: name,
                    new: new.to_owned(),
                });
            }
        }

        let first = locked.next_update_index();
        let last = if copy { first } else { first + 1 };
        let mut writer = Writer::new(locked.write_options(), first, last);
        let mut refs = vec![RefRecord {
            name: raw(new_rel),
            update_index: last,
            value,
        }];
        if !copy {
            refs.push(RefRecord {
                name: raw(old_rel),
                update_index: first,
                value: RefValue::Deletion,
            });
        }
        refs.sort_by(|a, b| a.name.cmp(&b.name));
        for r in &refs {
            writer.add_ref(r).map_err(|e| write_error(e.into()))?;
        }

        let old_logs = merged::logs_of(&tables, &raw(old_rel)).map_err(fmt)?;
        let logging =
            !old_logs.is_empty() || self.options().log_all_ref_updates.should_autocreate(new);
        let mut logs: BTreeMap<Vec<u8>, LogRecord> = BTreeMap::new();
        let mut put = |rec: LogRecord| {
            logs.insert(rec.key(), rec);
        };
        let entry = |name: &str, index: u64, from: ObjectId, to: ObjectId| LogRecord {
            refname: raw(name),
            update_index: index,
            update: Some(update_of(&Entry::new(
                from,
                to,
                committer,
                &rgit_bstr::to_bytes(message),
            ))),
        };
        for rec in &old_logs {
            if is_marker(rec) {
                continue;
            }
            let mut moved = rec.clone();
            moved.refname = raw(new_rel);
            put(moved);
        }
        if !copy {
            for rec in &old_logs {
                let mut gone = rec.clone();
                gone.update = None;
                put(gone);
            }
        }
        if logging {
            if copy {
                put(entry(new_rel, first, null, oid));
            } else {
                put(entry(new_rel, first, oid, null));
                put(entry(new_rel, last, null, oid));
                // `HEAD` on the renamed branch notes that it went away.
                let head_on_old = !self.linked()
                    && matches!(
                        read("HEAD").map_err(fmt)?.as_ref().and_then(target_of),
                        Some(Target::Symbolic(t)) if t == old_rel
                    );
                if head_on_old {
                    put(entry("HEAD", first, oid, null));
                }
            }
        }
        for rec in logs.values() {
            writer.add_log(rec).map_err(|e| write_error(e.into()))?;
        }
        let data = writer.finish().map_err(|e| write_error(e.into()))?;
        locked.add_table(&data, first, last).map_err(write_error)?;
        if options().auto_compact {
            let _ = stack.auto_compact(self.options().shared);
        }
        Ok(())
    }
}

// --- maintenance and setup -----------------------------------------------

impl FilesStore {
    /// Whether the repository keeps its references in the reftable format.
    pub fn is_reftable(&self) -> bool {
        self.reftable
    }

    /// Compacts the stack of this worktree (`git pack-refs` on a reftable
    /// repository): everything into one table, or with `auto` only what
    /// the geometric heuristic asks for.
    pub fn compact(&self, auto: bool) -> Result<(), StackError> {
        let stack = self.rt_worktree_stack();
        let shared = self.options().shared;
        if auto {
            stack
                .lock(shared)?
                .fsync(self.options().fsync)
                .auto_compact()
        } else {
            stack.compact_all(shared)
        }
    }
}

/// Creates the stack of a new repository or worktree in the git directory
/// `git_dir`: the `reftable/` directory with an empty `tables.list`, and
/// the stand-ins that make older tools recognise the directory as a
/// repository (`HEAD` pointing to `refs/heads/.invalid` and a file
/// `refs/heads`).
pub fn create(git_dir: &Path, kind: HashKind, shared: SharedRepository) -> io::Result<()> {
    let dir = git_dir.join(REFTABLE_DIR);
    std::fs::create_dir_all(&dir)?;
    let _ = shared.apply(&dir);
    let stack = Stack::open(&dir, kind);
    stack
        .lock(shared)
        .map_err(io_error)?
        .touch()
        .map_err(io_error)?;
    let head = git_dir.join("HEAD");
    std::fs::write(&head, HEAD_STAND_IN)?;
    let _ = shared.apply(&head);
    let refs = git_dir.join("refs");
    std::fs::create_dir_all(&refs)?;
    let _ = shared.apply(&refs);
    let heads = refs.join("heads");
    if !heads.is_file() {
        let _ = std::fs::remove_dir(&heads);
        std::fs::write(&heads, HEADS_STAND_IN)?;
        let _ = shared.apply(&heads);
    }
    Ok(())
}

/// Content of `HEAD` in a reftable repository.
pub const HEAD_STAND_IN: &str = "ref: refs/heads/.invalid\n";
/// Content of the file `refs/heads` in a reftable repository.
pub const HEADS_STAND_IN: &str = "this repository uses the reftable format\n";

/// The root reference `name` in the stack of the reftable repository or
/// worktree in `git_dir`, as the files format would store it.
pub(crate) fn root_ref_content(git_dir: &Path, name: &str) -> io::Result<Vec<u8>> {
    let dir = git_dir.join(REFTABLE_DIR);
    for kind in HashKind::ALL {
        let stack = Stack::open(&dir, kind);
        let tables = match stack.tables() {
            Ok(t) => t,
            Err(StackError::HashMismatch(_)) => continue,
            Err(e) => return Err(io_error(e)),
        };
        return match merged::read_ref(&tables, &raw(name)).map_err(io_error)? {
            Some(rec) => Ok(match rec.value {
                RefValue::Symbolic(t) => [b"ref: ".as_slice(), &t, b"\n"].concat(),
                RefValue::Direct(id) | RefValue::Peeled(id, _) => {
                    format!("{}\n", id.to_hex()).into_bytes()
                }
                RefValue::Deletion => Vec::new(),
            }),
            None => Err(io::Error::from(io::ErrorKind::NotFound)),
        };
    }
    Err(io::Error::from(io::ErrorKind::NotFound))
}

/// The object format of the tables in the stack of `git_dir`; `None` for
/// a stack without tables.
pub(crate) fn stack_hash(git_dir: &Path) -> Option<HashKind> {
    let dir = git_dir.join(REFTABLE_DIR);
    let list = std::fs::read_to_string(dir.join("tables.list")).ok()?;
    let first = list.lines().find(|l| !l.is_empty())?;
    let data = std::fs::read(dir.join(first)).ok()?;
    Table::parse(first, data).ok().map(|t| t.hash_kind())
}

/// Fills the new stack of the git directory `git_dir` with `refs` and
/// `logs` (entries oldest first) the way `git refs migrate` does: one
/// table with every reference at update index 1 and the log entries
/// numbered from 1 in the order given. Peeled values are not stored.
pub fn import(
    git_dir: &Path,
    kind: HashKind,
    shared: SharedRepository,
    refs: &[(String, Target)],
    logs: &[(String, Vec<Entry>)],
) -> io::Result<()> {
    create(git_dir, kind, shared)?;
    let stack = Stack::open(git_dir.join(REFTABLE_DIR), kind);
    let locked = stack.lock(shared).map_err(io_error)?;
    let count: usize = logs.iter().map(|(_, e)| e.len()).sum();
    let max = (count as u64).max(1);
    let mut writer = Writer::new(locked.write_options(), 1, max);
    let mut records: Vec<RefRecord> = refs
        .iter()
        .map(|(name, target)| RefRecord {
            name: raw(name),
            update_index: 1,
            value: match target {
                Target::Object(id) => RefValue::Direct(*id),
                Target::Symbolic(t) => RefValue::Symbolic(raw(t)),
            },
        })
        .collect();
    records.sort_by(|a, b| a.name.cmp(&b.name));
    records.dedup_by(|a, b| a.name == b.name);
    for rec in &records {
        writer.add_ref(rec).map_err(io_error)?;
    }
    let mut log_records = Vec::with_capacity(count);
    let mut index = 0;
    for (name, entries) in logs {
        for entry in entries {
            index += 1;
            log_records.push(LogRecord {
                refname: raw(name),
                update_index: index,
                update: Some(update_of(entry)),
            });
        }
    }
    log_records.sort_by_key(LogRecord::key);
    for rec in &log_records {
        writer.add_log(rec).map_err(io_error)?;
    }
    if writer.is_empty() {
        return Ok(());
    }
    let data = writer.finish().map_err(io_error)?;
    locked.add_table(&data, 1, max).map_err(io_error)
}
