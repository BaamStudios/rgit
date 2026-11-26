// SPDX-License-Identifier: MIT
//! Shared glue for the reference commands (`update-ref`, `show-ref`,
//! `for-each-ref`, `pack-refs`): the reference store with its settings,
//! object lookups over loose objects and packs, unique abbreviations and a
//! object names through the revision parser.

use std::path::Path;

use rgit_config::{ConfigSet, SystemEnv};
use rgit_hash::{HashKind, ObjectId, Prefix};
use rgit_object::ObjectKind;
use rgit_odb::{LooseOptions, LooseStore};
use rgit_pack::{DeltaBaseCache, PackSet};
use rgit_refs::files::{FilesOptions, FilesStore};
use rgit_refs::reflog::{Committer, FilesReflog, ReflogWriter};
use rgit_refs::{LogAllRefUpdates, RefStore};
use rgit_repo::Setup;

use crate::Context;
use crate::revisions;
use rgit_revwalk::revision::{
    Flags, Options as RevOptions, Repo, Resolver, WorkTreeInfo, WorkTreeLookup,
};

/// The configuration of the repository (all standard scopes, with the
/// repository facts for `includeIf`).
pub fn load_config(ctx: &Context, setup: &Setup) -> Result<ConfigSet, crate::Die> {
    revisions::load_config(ctx, setup)
}

/// `core.sharedRepository` (default `umask`; a bad value counts as the
/// default here, the config reader reports it elsewhere).
pub fn shared_repository(config: &ConfigSet) -> rgit_fs::SharedRepository {
    config
        .string("core.sharedrepository")
        .ok()
        .flatten()
        .and_then(|v| rgit_fs::SharedRepository::parse(&v.to_string()).ok())
        .unwrap_or_default()
}

/// The files reference store of the repository, with
/// `core.logAllRefUpdates` from the configuration.
pub fn ref_store(setup: &Setup, config: &ConfigSet) -> FilesStore {
    let mut options = FilesOptions::default();
    let log_all = config
        .string("core.logallrefupdates")
        .ok()
        .flatten()
        .and_then(|v| LogAllRefUpdates::parse(&v.to_string()));
    options.shared = shared_repository(config);
    options.log_all_ref_updates = match log_all {
        Some(v) => v,
        None if setup.is_bare() => LogAllRefUpdates::False,
        None => LogAllRefUpdates::True,
    };
    if let Ok(Some(ms)) = config.int("core.packedrefstimeout") {
        options.packed_refs_timeout = rgit_fs::Timeout::from_millis(ms);
    }
    if let Ok(Some(ms)) = config.int("core.filesreflocktimeout") {
        options.ref_lock_timeout = rgit_fs::Timeout::from_millis(ms);
    }
    if setup.format.ref_storage == rgit_repo::RefStorage::Reftable {
        reftable_options(config);
    }
    FilesStore::with_options(
        setup.common_dir.clone(),
        setup.format.object_format,
        options,
    )
    .with_worktree(setup.git_dir.clone())
}

/// Applies the `reftable.*` settings to the reftable stacks of this
/// process. Values outside of the format's limits end the command, as in
/// git, which checks them when it opens the reference store.
fn reftable_options(config: &ConfigSet) {
    use crate::messages::refs as msg;
    let mut o = rgit_refs::reftable::stack::options();
    let die = |text: &str| -> ! {
        eprintln!("fatal: {text}");
        std::process::exit(128);
    };
    if let Ok(Some(n)) = config.int("reftable.blocksize") {
        if n > i64::from(rgit_refs::reftable::writer::MAX_BLOCK_SIZE) {
            die(msg::REFTABLE_BLOCK_SIZE_TOO_LARGE);
        }
        if n > 0 {
            o.block_size = n as u32;
        }
    }
    if let Ok(Some(n)) = config.int("reftable.restartinterval") {
        if n > i64::from(u16::MAX) {
            die(msg::REFTABLE_RESTART_INTERVAL_TOO_LARGE);
        }
        if n > 0 {
            o.restart_interval = n as u16;
        }
    }
    if let Ok(Some(b)) = config.bool("reftable.indexobjects") {
        o.index_objects = b;
    }
    if let Ok(Some(n)) = config.int("reftable.geometricfactor") {
        if n > 0 {
            o.geometric_factor = n.min(255) as u8;
        }
    }
    if let Ok(Some(ms)) = config.int("reftable.locktimeout") {
        o.lock_timeout = rgit_fs::Timeout::from_millis(ms);
    }
    rgit_refs::reftable::stack::set_options(o);
}

/// The packs and loose objects of one object directory.
struct Layer {
    packs: PackSet,
    loose: LooseStore,
}

/// Loose objects plus packs of the repository, then those of its
/// alternates (`objects/info/alternates`, recursively, and
/// `$GIT_ALTERNATE_OBJECT_DIRECTORIES`).
pub struct Objects {
    layers: Vec<Layer>,
    hash: HashKind,
}

impl Objects {
    /// Opens the object directory of `setup`.
    pub fn open(setup: &Setup) -> Objects {
        let hash = setup.format.object_format;
        let dir = rgit_odb::objects_dir(&setup.common_dir);
        Objects::at(&dir, hash)
    }

    fn at(dir: &Path, hash: HashKind) -> Objects {
        let layer = |d: &Path| Layer {
            packs: PackSet::open(d.join("pack"), hash),
            loose: LooseStore::new(d, hash, LooseOptions::default()),
        };
        let mut layers = vec![layer(dir)];
        layers.extend(rgit_odb::resolve_alternates(dir).iter().map(|d| layer(d)));
        Objects { layers, hash }
    }

    /// The hash algorithm.
    pub fn hash(&self) -> HashKind {
        self.hash
    }

    /// Whether the object exists.
    pub fn contains(&self, id: &ObjectId) -> bool {
        self.layers
            .iter()
            .any(|l| l.packs.contains(id) || l.loose.contains(id))
    }

    /// Type and content of an object.
    pub fn read(&self, id: &ObjectId) -> Option<(ObjectKind, Vec<u8>)> {
        let mut cache = DeltaBaseCache::new(0);
        for l in &self.layers {
            if let Ok(Some(o)) = l.packs.read(id, &mut cache, &mut |_| {}) {
                let kind = match o.kind {
                    rgit_pack::ObjectKind::Commit => ObjectKind::Commit,
                    rgit_pack::ObjectKind::Tree => ObjectKind::Tree,
                    rgit_pack::ObjectKind::Blob => ObjectKind::Blob,
                    rgit_pack::ObjectKind::Tag => ObjectKind::Tag,
                };
                return Some((kind, o.data));
            }
            if let Ok(Some(raw)) = l.loose.read(id) {
                return Some((raw.kind, raw.data));
            }
        }
        None
    }

    /// The type of an object.
    pub fn kind(&self, id: &ObjectId) -> Option<ObjectKind> {
        for l in &self.layers {
            if let Ok(Some(h)) = l.loose.header(id) {
                return Some(h.kind);
            }
        }
        self.read(id).map(|(k, _)| k)
    }

    /// Follows annotated tags until a non-tag object. `None` if an object
    /// on the way is missing.
    pub fn peel(&self, id: &ObjectId) -> Option<ObjectId> {
        let mut cur = *id;
        for _ in 0..64 {
            let (kind, data) = self.read(&cur)?;
            if kind != ObjectKind::Tag {
                return Some(cur);
            }
            cur = rgit_object::Tag::parse(self.hash, &data)
                .ok()?
                .object()
                .ok()?;
        }
        None
    }

    /// The object the annotated tag `id` finally points to. `None` when
    /// `id` is not a tag, and for a tag (or a tag in its chain) whose
    /// target is missing or not of the type the tag claims.
    pub fn peel_tag(&self, id: &ObjectId) -> Option<ObjectId> {
        if self.kind(id) != Some(ObjectKind::Tag) {
            return None;
        }
        let mut cur = *id;
        for _ in 0..64 {
            let (kind, data) = self.read(&cur)?;
            if kind != ObjectKind::Tag {
                return Some(cur);
            }
            let tag = rgit_object::Tag::parse(self.hash, &data).ok()?;
            let next = tag.object().ok()?;
            if self.kind(&next)? != tag.target_kind().ok()? {
                return None;
            }
            cur = next;
        }
        None
    }

    /// The pack holding `id` and the offset of its entry.
    fn locate_packed(
        layer: &Layer,
        id: &ObjectId,
    ) -> Option<(std::sync::Arc<rgit_pack::Pack>, u64)> {
        layer.packs.packs().into_iter().find_map(|p| {
            let pos = p.index().lookup(id)?;
            let off = p.index().offset_at(pos).ok()?;
            Some((p, off))
        })
    }

    /// Bytes the stored copy of `id` takes on disk (loose before packed).
    pub fn disk_size(&self, id: &ObjectId) -> u64 {
        for l in &self.layers {
            if l.loose.contains(id) {
                return std::fs::metadata(l.loose.path_of(id)).map_or(0, |m| m.len());
            }
            if let Some((p, off)) = Objects::locate_packed(l, id) {
                return p.entry_end(off).map_or(0, |e| e - off);
            }
        }
        0
    }

    /// The delta base of the stored copy of `id` (the null id when it is
    /// not stored as a delta).
    pub fn delta_base(&self, id: &ObjectId) -> ObjectId {
        for l in &self.layers {
            if l.loose.contains(id) {
                break;
            }
            if let Some((p, off)) = Objects::locate_packed(l, id) {
                if let Ok(Some(base)) = p.delta_base(off) {
                    return base;
                }
                break;
            }
        }
        self.hash.null()
    }

    /// Number of objects (for the default abbreviation length).
    pub fn approximate_count(&self) -> usize {
        let mut n = 0usize;
        for l in &self.layers {
            n += l
                .packs
                .packs()
                .iter()
                .map(|p| p.index().len() as usize)
                .sum::<usize>();
            let _ = l.loose.for_each_object(&mut |_| n += 1);
        }
        n
    }

    /// Ids starting with `prefix`, at most `limit`.
    fn prefix_matches(&self, prefix: &Prefix, limit: usize) -> Vec<ObjectId> {
        let mut found: Vec<ObjectId> = Vec::new();
        for l in &self.layers {
            for id in l.packs.lookup_prefix(prefix, limit) {
                if !found.contains(&id) {
                    found.push(id);
                }
            }
            let _ = l.loose.for_each_prefix_match(prefix, &mut |id| {
                if !found.contains(&id) {
                    found.push(id);
                }
            });
        }
        found
    }

    /// The shortest unique abbreviation of `id` with at least `min` digits
    /// (git's minimum of 4 applies).
    pub fn abbrev(&self, id: &ObjectId, min: usize) -> String {
        let hex = id.to_hex();
        let full = hex.len();
        let mut len = min.clamp(4, full);
        while len < full {
            let Ok(prefix) = Prefix::from_hex(self.hash, &hex.as_bytes()[..len]) else {
                break;
            };
            let others = self
                .prefix_matches(&prefix, 2)
                .into_iter()
                .filter(|o| o != id)
                .count();
            if others == 0 {
                break;
            }
            len += 1;
        }
        hex[..len].to_owned()
    }
}

/// Object names through the revision parser (`gitrevisions(7)`).
pub struct Revs {
    objects: revisions::Objects,
    refs: revisions::Refs,
    config: ConfigSet,
    index: revisions::IndexFile,
    work_tree: Option<revisions::WorkTree>,
    info: Option<WorkTreeInfo>,
    opts: RevOptions,
}

impl Revs {
    /// The revision parser for the repository of `setup`.
    pub fn new(ctx: &Context, setup: &Setup) -> Result<Revs, crate::Die> {
        let kind = setup.format.object_format;
        let config = revisions::load_config(ctx, setup)?;
        let opts = revisions::options(ctx, &config, kind)?;
        let shallow = std::env::var_os("GIT_SHALLOW_FILE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| setup.common_dir.join("shallow"));
        Ok(Revs {
            objects: revisions::Objects::new(revisions::objects_dir(setup), kind, &shallow),
            refs: revisions::Refs::new(setup, kind),
            config,
            index: revisions::IndexFile::new(setup, kind),
            work_tree: setup.work_tree.clone().map(revisions::WorkTree),
            info: revisions::work_tree_info(setup),
            opts,
        })
    }

    /// The revision parser for `setup` with an already loaded `config`;
    /// invalid revision settings fall back to the defaults instead of
    /// ending the command.
    pub fn with_config(setup: &Setup, config: ConfigSet) -> Revs {
        let kind = setup.format.object_format;
        let opts = revisions::options_lenient(&config, kind);
        let shallow = std::env::var_os("GIT_SHALLOW_FILE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| setup.common_dir.join("shallow"));
        Revs {
            objects: revisions::Objects::new(revisions::objects_dir(setup), kind, &shallow),
            refs: revisions::Refs::new(setup, kind),
            config,
            index: revisions::IndexFile::new(setup, kind),
            work_tree: setup.work_tree.clone().map(revisions::WorkTree),
            info: revisions::work_tree_info(setup),
            opts,
        }
    }

    /// The object `name` stands for, if any (no messages).
    pub fn resolve(&self, name: &str) -> Option<ObjectId> {
        let flags = Flags {
            quiet: true,
            ..Flags::default()
        };
        self.with_resolver(|r| r.resolve_with(name, flags).ok())
    }

    /// Run `f` with a revision parser over this repository.
    pub fn with_resolver<T>(&self, f: impl FnOnce(&Resolver<'_>) -> T) -> T {
        let cfg = revisions::Config(&self.config);
        let repo = Repo {
            objects: &self.objects,
            refs: &self.refs,
            config: &cfg,
            index: Some(&self.index),
            work_tree: self.work_tree.as_ref().map(|w| w as &dyn WorkTreeLookup),
            work_tree_info: self.info.clone(),
        };
        let resolver = Resolver::new(repo, self.opts.clone());
        f(&resolver)
    }

    /// Whether commit `anc` is reachable from (or equal to) commit `desc`.
    pub fn is_ancestor(&self, anc: &ObjectId, desc: &ObjectId) -> bool {
        let cfg = revisions::Config(&self.config);
        let repo = Repo {
            objects: &self.objects,
            refs: &self.refs,
            config: &cfg,
            index: Some(&self.index),
            work_tree: self.work_tree.as_ref().map(|w| w as &dyn WorkTreeLookup),
            work_tree_info: self.info.clone(),
        };
        let resolver = Resolver::new(repo, self.opts.clone());
        resolver
            .is_ancestor_of_any(anc, std::slice::from_ref(desc))
            .unwrap_or(false)
    }
}

/// Hooks for reference updates from the command line: object checks, and
/// reflog entries through [`ReflogWriter`] (#30).
pub struct UpdateHooks<'a> {
    objects: &'a Objects,
    writer: Option<ReflogWriter>,
    store: FilesStore,
    /// References that become symbolic: their entry is written even when
    /// the resolved value stays the same.
    symbolic: std::collections::HashSet<String>,
    /// Runs the `reference-transaction` hook when set
    /// ([`UpdateHooks::with_transaction_hook`]).
    transaction_hook: Option<rgit_hook::Hooks>,
}

impl<'a> UpdateHooks<'a> {
    /// Hooks for the repository of `setup`.
    pub fn new(setup: &Setup, config: &ConfigSet, objects: &'a Objects) -> Self {
        let env = SystemEnv::default();
        let committer = rgit_ident::IdentConfig::from_config(config)
            .ok()
            .and_then(|ic| {
                rgit_ident::Resolver::new(&ic, &env)
                    .ident(rgit_ident::Role::Committer, rgit_ident::Strictness::Lenient)
                    .ok()
            })
            .map(|id| Committer::new(&id.name, &id.email, id.time, id.tz));
        let logs = FilesReflog::new(
            setup.git_dir.clone(),
            setup.common_dir.clone(),
            setup.format.object_format,
        )
        .with_shared(shared_repository(config));
        UpdateHooks {
            objects,
            writer: committer.map(|c| ReflogWriter::new(logs, c)),
            store: ref_store(setup, config),
            symbolic: Default::default(),
            // Every reference transaction runs `reference-transaction`.
            transaction_hook: crate::hooks::with_context(
                &crate::hooks::current_context(),
                setup,
                config,
            )
            .ok(),
        }
    }

    /// Also run the `reference-transaction` hook of `hooks` (githooks(5)):
    /// once per phase with `<old> <new> <refname>` lines on its standard
    /// input; a failure in the `prepared` phase aborts the transaction.
    pub fn with_transaction_hook(mut self, hooks: rgit_hook::Hooks) -> Self {
        self.transaction_hook = Some(hooks);
        self
    }

    /// Hooks that write no reflog entries (the caller writes them with
    /// [`UpdateHooks::log`]).
    pub fn without_reflogs(mut self) -> Self {
        self.writer = None;
        self
    }

    /// Who makes the updates (for reflog entries written elsewhere).
    pub fn committer(&self) -> Option<&Committer> {
        self.writer.as_ref().map(|w| &w.committer)
    }

    /// The reflog files.
    pub fn reflogs(&self) -> Option<&FilesReflog> {
        self.writer.as_ref().map(|w| &w.store)
    }

    /// Appends `<old> <new> <committer>\t<message>` to the log of `name`;
    /// with `create`, a missing log is created.
    pub fn log(&self, name: &str, old: ObjectId, new: ObjectId, message: &str, create: bool) {
        if let Some(w) = &self.writer {
            let _ = w
                .store
                .log_update(name, old, new, &w.committer, message.as_bytes(), create);
        }
    }

    /// Notes that `name` is made a symbolic reference.
    pub fn mark_symbolic(&mut self, name: &str) {
        self.symbolic.insert(name.to_owned());
    }
}

impl rgit_refs::RefHooks for UpdateHooks<'_> {
    fn check_object(&mut self, name: &str, oid: &ObjectId) -> Result<(), String> {
        match self.objects.kind(oid) {
            None => Err(rgit_refs::messages::nonexistent_object(name, oid)),
            Some(k) if k != ObjectKind::Commit && name.starts_with("refs/heads/") => {
                Err(rgit_refs::messages::non_commit_on_branch(oid, name))
            }
            Some(_) => Ok(()),
        }
    }

    fn reflog_append(&mut self, update: &rgit_refs::ReflogUpdate) -> Result<(), String> {
        // A symbolic update to a dangling target, or a dangling reference
        // that stays dangling, gets no entry.
        if update.new.is_null() && (update.old.is_null() || self.symbolic.contains(&update.name)) {
            return Ok(());
        }
        // An unchanged value is not rewritten, so it gets no entry.
        if update.old == update.new
            && !self.symbolic.contains(&update.name)
            && matches!(self.store.read_raw(&update.name), Ok(Some(rgit_refs::Target::Object(o))) if o == update.old)
        {
            return Ok(());
        }
        match &mut self.writer {
            Some(w) => w.reflog_append(update),
            None => Ok(()),
        }
    }

    fn reflog_delete(&mut self, name: &str) {
        if let Some(w) = &mut self.writer {
            w.reflog_delete(name);
        }
    }

    fn peel(&mut self, oid: &ObjectId) -> Option<ObjectId> {
        self.objects.peel_tag(oid)
    }

    fn reference_transaction(
        &mut self,
        state: rgit_refs::HookState,
        updates: &[(String, String, String)],
    ) -> Result<(), String> {
        let Some(hooks) = &self.transaction_hook else {
            return Ok(());
        };
        if updates.is_empty() {
            return Ok(());
        }
        let mut input = Vec::new();
        for (old, new, name) in updates {
            input.extend_from_slice(format!("{old} {new} {name}\n").as_bytes());
        }
        let opts = rgit_hook::RunOptions::with_args([state.as_str()])
            .stdin(rgit_hook::Input::Data(&input));
        let outcome = hooks.run("reference-transaction", &opts)?;
        let can_abort = matches!(
            state,
            rgit_refs::HookState::Preparing | rgit_refs::HookState::Prepared
        );
        if can_abort && !outcome.ok() {
            return Err(crate::messages::hook::transaction_aborted_by_hook(
                state.as_str(),
            ));
        }
        Ok(())
    }
}
