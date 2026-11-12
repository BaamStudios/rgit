// SPDX-License-Identifier: MIT
//! The backend-independent reference store interface.
//!
//! [`RefStore`] is implemented by the files backend
//! ([`crate::files::FilesStore`]); the reftable backend (#31) implements the
//! same trait. Updates go through [`Transaction`]: queue [`RefUpdate`]s,
//! [`prepare`](Transaction::prepare) (lock and check every reference), then
//! [`commit`](Transaction::commit) or [`abort`](Transaction::abort).

use rgit_hash::{HashKind, ObjectId};

use crate::error::{Error, TransactionError};
use crate::name::{RefNameFlags, is_root_ref, is_valid_refname};
use crate::target::{Reference, Resolved, Target};

/// Maximum number of reads when following symbolic references, as in git:
/// a chain of up to four symbolic references resolves, five do not.
pub const MAX_SYMREF_DEPTH: usize = 5;

/// New value of an update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewValue {
    /// Point at an object. The null id deletes the reference.
    Object(ObjectId),
    /// Make the reference symbolic.
    Symbolic(String),
}

/// Expected current value of an update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OldValue {
    /// The reference resolves to this object. The null id means: the
    /// reference must not exist.
    Object(ObjectId),
    /// The reference is a symbolic reference to this name.
    Symbolic(String),
    /// The reference must not exist (`symref-create`, `symref-verify`
    /// without a target).
    SymbolicAbsent,
}

/// One queued change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefUpdate {
    /// Reference name as given.
    pub name: String,
    /// New value; `None` only verifies `old`.
    pub new: Option<NewValue>,
    /// Expected current value; `None` skips the check.
    pub old: Option<OldValue>,
    /// Do not follow `name` if it is a symbolic reference.
    pub no_deref: bool,
    /// Reflog message.
    pub message: String,
    /// Create the reflog even if `core.logAllRefUpdates` would not.
    pub create_reflog: bool,
}

impl RefUpdate {
    fn new(name: impl Into<String>, new: Option<NewValue>, old: Option<OldValue>) -> Self {
        Self {
            name: name.into(),
            new,
            old,
            no_deref: false,
            message: String::new(),
            create_reflog: false,
        }
    }

    /// `update <ref> <new> [<old>]`
    pub fn update(name: impl Into<String>, new: ObjectId, old: Option<ObjectId>) -> Self {
        Self::new(name, Some(NewValue::Object(new)), old.map(OldValue::Object))
    }

    /// `create <ref> <new>`: the reference must not exist.
    pub fn create(name: impl Into<String>, new: ObjectId) -> Self {
        let null = ObjectId::null(new.kind());
        Self::new(
            name,
            Some(NewValue::Object(new)),
            Some(OldValue::Object(null)),
        )
    }

    /// `delete <ref> [<old>]`
    pub fn delete(name: impl Into<String>, kind: HashKind, old: Option<ObjectId>) -> Self {
        Self::new(
            name,
            Some(NewValue::Object(ObjectId::null(kind))),
            old.map(OldValue::Object),
        )
    }

    /// `verify <ref> <old>` (the null id: must not exist).
    pub fn verify(name: impl Into<String>, old: ObjectId) -> Self {
        Self::new(name, None, Some(OldValue::Object(old)))
    }

    /// `symref-update <ref> <target> [ref <old-target> | oid <old-oid>]`
    pub fn symref_update(
        name: impl Into<String>,
        target: impl Into<String>,
        old: Option<OldValue>,
    ) -> Self {
        Self::new(name, Some(NewValue::Symbolic(target.into())), old)
    }

    /// `symref-create <ref> <target>`
    pub fn symref_create(name: impl Into<String>, target: impl Into<String>) -> Self {
        Self::new(
            name,
            Some(NewValue::Symbolic(target.into())),
            Some(OldValue::SymbolicAbsent),
        )
    }

    /// `symref-delete <ref> [<old-target>]` (implies `no_deref`).
    pub fn symref_delete(name: impl Into<String>, kind: HashKind, old: Option<String>) -> Self {
        Self::new(
            name,
            Some(NewValue::Object(ObjectId::null(kind))),
            old.map(OldValue::Symbolic),
        )
        .no_deref(true)
    }

    /// `symref-verify <ref> [<old-target>]` (implies `no_deref`).
    pub fn symref_verify(name: impl Into<String>, old: Option<String>) -> Self {
        let old = match old {
            Some(t) => OldValue::Symbolic(t),
            None => OldValue::SymbolicAbsent,
        };
        Self::new(name, None, Some(old)).no_deref(true)
    }

    /// Sets `no_deref`.
    pub fn no_deref(mut self, yes: bool) -> Self {
        self.no_deref = yes;
        self
    }

    /// Sets the reflog message.
    pub fn message(mut self, msg: impl Into<String>) -> Self {
        self.message = msg.into();
        self
    }

    /// Sets `create_reflog`.
    pub fn create_reflog(mut self, yes: bool) -> Self {
        self.create_reflog = yes;
        self
    }

    /// Whether this update deletes the reference.
    pub fn is_delete(&self) -> bool {
        matches!(&self.new, Some(NewValue::Object(id)) if id.is_null())
    }
}

/// `core.logAllRefUpdates`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LogAllRefUpdates {
    /// `false`: only append to reflogs that exist.
    False,
    /// `true` (the default in repositories with a working tree): create
    /// reflogs for branches, remote-tracking branches, notes and root refs.
    #[default]
    True,
    /// `always`: create reflogs for every reference.
    Always,
}

impl LogAllRefUpdates {
    /// Parses the config value (`true`/`false` spellings or `always`).
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "always" => Some(Self::Always),
            "true" | "yes" | "on" | "1" | "" => Some(Self::True),
            "false" | "no" | "off" | "0" => Some(Self::False),
            _ => None,
        }
    }

    /// Whether an update of `name` should create its reflog.
    pub fn should_autocreate(self, name: &str) -> bool {
        match self {
            Self::False => false,
            Self::Always => true,
            Self::True => {
                name.starts_with("refs/heads/")
                    || name.starts_with("refs/remotes/")
                    || name.starts_with("refs/notes/")
                    || is_root_ref(name)
            }
        }
    }
}

/// One reflog line to append, handed to [`RefHooks::reflog_append`] while
/// the reference is still locked (#30 writes the file).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReflogUpdate {
    /// The reference whose log gets the line.
    pub name: String,
    /// Old value (null if the reference did not exist).
    pub old: ObjectId,
    /// New value (null if the reference is deleted or dangling).
    pub new: ObjectId,
    /// Message (`-m`).
    pub message: String,
    /// Create the log file if it does not exist yet
    /// (`--create-reflog` or `core.logAllRefUpdates`).
    pub create: bool,
}

/// Phases reported to the `reference-transaction` hook (#76).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookState {
    /// All references are locked and checked.
    Prepared,
    /// The transaction was committed.
    Committed,
    /// The transaction was aborted.
    Aborted,
}

impl HookState {
    /// The argument passed to the hook.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Committed => "committed",
            Self::Aborted => "aborted",
        }
    }
}

/// Callbacks into higher layers. Every method has a no-op default.
pub trait RefHooks {
    /// Checks a new object value before anything is locked (object
    /// existence, commits on branches). An `Err` aborts the transaction
    /// with that message.
    fn check_object(&mut self, name: &str, oid: &ObjectId) -> Result<(), String> {
        let _ = (name, oid);
        Ok(())
    }

    /// Appends a reflog entry (#30). Called while the reference is locked,
    /// before the new value becomes visible.
    fn reflog_append(&mut self, update: &ReflogUpdate) -> Result<(), String> {
        let _ = update;
        Ok(())
    }

    /// Removes the reflog of a deleted reference (#30).
    fn reflog_delete(&mut self, name: &str) {
        let _ = name;
    }

    /// The `reference-transaction` hook (#76). `updates` lists the
    /// effective updates as `(old, new, name)`. Only the `prepared` state
    /// can abort the transaction.
    fn reference_transaction(
        &mut self,
        state: HookState,
        updates: &[(ObjectId, ObjectId, String)],
    ) -> Result<(), String> {
        let _ = (state, updates);
        Ok(())
    }
}

/// Hooks that do nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoHooks;

impl RefHooks for NoHooks {}

/// Options for [`RefStore::iter`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IterOptions {
    /// Only names starting with this string (e.g. `refs/tags/`). Empty
    /// means every reference below `refs/`.
    pub prefix: String,
    /// Also list root references such as `HEAD` and `ORIG_HEAD`
    /// (`for-each-ref --include-root-refs`).
    pub include_root_refs: bool,
}

impl IterOptions {
    /// References whose name starts with `prefix`.
    pub fn prefix(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
            include_root_refs: false,
        }
    }
}

/// One item of an iteration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IterItem {
    /// A readable reference.
    Ref(Reference),
    /// A loose file whose content is not a reference
    /// ([`crate::messages::ignoring_broken_ref`]).
    Broken(String),
    /// A loose file or packed entry whose name breaks the rules
    /// ([`crate::messages::ignoring_broken_name`]).
    BadName(String),
}

impl IterItem {
    /// The reference name.
    pub fn name(&self) -> &str {
        match self {
            IterItem::Ref(r) => &r.name,
            IterItem::Broken(n) | IterItem::BadName(n) => n,
        }
    }
}

/// A prepared (locked and checked) transaction of some backend.
pub trait PreparedTransaction {
    /// Makes every update visible.
    fn commit(self: Box<Self>, hooks: &mut dyn RefHooks) -> Result<(), TransactionError>;
    /// Releases every lock without changing anything.
    fn abort(self: Box<Self>, hooks: &mut dyn RefHooks);
}

/// A reference backend.
pub trait RefStore {
    /// Hash algorithm of the repository.
    fn hash_kind(&self) -> HashKind;

    /// Reads one reference without following symbolic references.
    /// `Ok(None)` when it does not exist.
    fn read_raw(&self, name: &str) -> Result<Option<Target>, Error>;

    /// Lists references sorted by name (byte order), as git iterates them.
    fn iter(&self, options: &IterOptions) -> Result<Vec<IterItem>, Error>;

    /// Locks and checks `updates`. On error nothing is changed and no lock
    /// file is left behind.
    fn prepare<'s>(
        &'s self,
        updates: Vec<RefUpdate>,
        hooks: &mut dyn RefHooks,
    ) -> Result<Box<dyn PreparedTransaction + 's>, TransactionError>;

    /// Follows symbolic references from `name`. `Ok(None)` when `name`
    /// itself does not exist; a dangling chain yields `oid: None`.
    fn resolve(&self, name: &str) -> Result<Option<Resolved>, Error> {
        let mut chain = vec![name.to_owned()];
        let mut current = name.to_owned();
        for _ in 0..MAX_SYMREF_DEPTH {
            match self.read_raw(&current)? {
                None if chain.len() == 1 => return Ok(None),
                None => return Ok(Some(Resolved { chain, oid: None })),
                Some(Target::Object(oid)) => {
                    return Ok(Some(Resolved {
                        chain,
                        oid: Some(oid),
                    }));
                }
                Some(Target::Symbolic(next)) => {
                    chain.push(next.clone());
                    current = next;
                }
            }
        }
        Err(Error::TooDeep(name.to_owned()))
    }

    /// The object `name` resolves to, if any.
    fn resolve_oid(&self, name: &str) -> Result<Option<ObjectId>, Error> {
        Ok(self.resolve(name)?.and_then(|r| r.oid))
    }
}

/// Lifecycle of a [`Transaction`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransactionState {
    /// Updates can be queued.
    Open,
    /// Locks are held.
    Prepared,
    /// Committed or aborted.
    Closed,
}

/// A set of updates applied atomically: either all of them or none.
pub struct Transaction<'s> {
    store: &'s dyn RefStore,
    updates: Vec<RefUpdate>,
    prepared: Option<Box<dyn PreparedTransaction + 's>>,
    state: TransactionState,
}

impl<'s> Transaction<'s> {
    /// Starts an empty transaction on `store`.
    pub fn new(store: &'s dyn RefStore) -> Self {
        Self {
            store,
            updates: Vec::new(),
            prepared: None,
            state: TransactionState::Open,
        }
    }

    /// Current state.
    pub fn state(&self) -> TransactionState {
        self.state
    }

    /// Queued updates.
    pub fn updates(&self) -> &[RefUpdate] {
        &self.updates
    }

    /// Queues an update. Fails with [`TransactionError::Closed`] after
    /// prepare, commit or abort.
    pub fn push(&mut self, update: RefUpdate) -> Result<(), TransactionError> {
        if self.state != TransactionState::Open {
            return Err(TransactionError::Closed);
        }
        self.updates.push(update);
        Ok(())
    }

    /// Locks and checks every queued update. On error the transaction is
    /// closed and nothing is left locked.
    pub fn prepare(&mut self, hooks: &mut dyn RefHooks) -> Result<(), TransactionError> {
        if self.state != TransactionState::Open {
            return Err(TransactionError::Closed);
        }
        self.state = TransactionState::Closed;
        let updates = std::mem::take(&mut self.updates);
        let prepared = self.store.prepare(updates, hooks)?;
        self.prepared = Some(prepared);
        self.state = TransactionState::Prepared;
        Ok(())
    }

    /// Prepares if necessary, then commits.
    pub fn commit(mut self, hooks: &mut dyn RefHooks) -> Result<(), TransactionError> {
        if self.state == TransactionState::Open {
            self.prepare(hooks)?;
        }
        self.state = TransactionState::Closed;
        match self.prepared.take() {
            Some(p) => p.commit(hooks),
            None => Err(TransactionError::Closed),
        }
    }

    /// Releases all locks; nothing changes.
    pub fn abort(mut self, hooks: &mut dyn RefHooks) {
        self.state = TransactionState::Closed;
        if let Some(p) = self.prepared.take() {
            p.abort(hooks);
        }
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if let Some(p) = self.prepared.take() {
            p.abort(&mut NoHooks);
        }
    }
}

/// Checks that the name and the symbolic target (if any) of `update` are
/// well-formed.
pub fn check_update_name(update: &RefUpdate) -> Result<(), TransactionError> {
    if !is_valid_refname(update.name.as_bytes(), RefNameFlags::ONELEVEL) {
        return Err(TransactionError::BadName(update.name.clone()));
    }
    if let Some(NewValue::Symbolic(t)) = &update.new {
        if !is_valid_refname(t.as_bytes(), RefNameFlags::ONELEVEL) {
            return Err(TransactionError::BadName(t.clone()));
        }
    }
    Ok(())
}

/// Fails if a name appears twice in `updates`.
pub fn check_duplicates(updates: &[RefUpdate]) -> Result<(), TransactionError> {
    let mut names: Vec<&str> = updates.iter().map(|u| u.name.as_str()).collect();
    names.sort_unstable();
    if let Some(w) = names.windows(2).find(|w| w[0] == w[1]) {
        return Err(TransactionError::Duplicate(w[0].to_owned()));
    }
    Ok(())
}
