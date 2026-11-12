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

/// The files reference store of the repository, with
/// `core.logAllRefUpdates` from the configuration.
pub fn ref_store(setup: &Setup, config: &ConfigSet) -> FilesStore {
    let mut options = FilesOptions::default();
    let log_all = config
        .string("core.logallrefupdates")
        .ok()
        .flatten()
        .and_then(|v| LogAllRefUpdates::parse(&v.to_string()));
    options.log_all_ref_updates = match log_all {
        Some(v) => v,
        None if setup.is_bare() => LogAllRefUpdates::False,
        None => LogAllRefUpdates::True,
    };
    FilesStore::with_options(
        setup.cwd.join(&setup.common_dir),
        setup.format.object_format,
        options,
    )
}

/// Loose objects plus packs. Alternates are not read yet.
pub struct Objects {
    packs: PackSet,
    loose: LooseStore,
    hash: HashKind,
}

impl Objects {
    /// Opens the object directory of `setup`.
    pub fn open(setup: &Setup) -> Objects {
        let hash = setup.format.object_format;
        let dir = rgit_odb::objects_dir(&setup.cwd.join(&setup.common_dir));
        Objects::at(&dir, hash)
    }

    fn at(dir: &Path, hash: HashKind) -> Objects {
        Objects {
            packs: PackSet::open(dir.join("pack"), hash),
            loose: LooseStore::new(dir, hash, LooseOptions::default()),
            hash,
        }
    }

    /// The hash algorithm.
    pub fn hash(&self) -> HashKind {
        self.hash
    }

    /// Whether the object exists.
    pub fn contains(&self, id: &ObjectId) -> bool {
        self.packs.contains(id) || self.loose.contains(id)
    }

    /// Type and content of an object.
    pub fn read(&self, id: &ObjectId) -> Option<(ObjectKind, Vec<u8>)> {
        let mut cache = DeltaBaseCache::new(0);
        if let Ok(Some(o)) = self.packs.read(id, &mut cache, &mut |_| {}) {
            let kind = match o.kind {
                rgit_pack::ObjectKind::Commit => ObjectKind::Commit,
                rgit_pack::ObjectKind::Tree => ObjectKind::Tree,
                rgit_pack::ObjectKind::Blob => ObjectKind::Blob,
                rgit_pack::ObjectKind::Tag => ObjectKind::Tag,
            };
            return Some((kind, o.data));
        }
        let raw = self.loose.read(id).ok()??;
        Some((raw.kind, raw.data))
    }

    /// The type of an object.
    pub fn kind(&self, id: &ObjectId) -> Option<ObjectKind> {
        if let Ok(Some(h)) = self.loose.header(id) {
            return Some(h.kind);
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

    /// Ids starting with `prefix`, at most `limit`.
    fn prefix_matches(&self, prefix: &Prefix, limit: usize) -> Vec<ObjectId> {
        let mut found = self.packs.lookup_prefix(prefix, limit);
        let _ = self.loose.for_each_prefix_match(prefix, &mut |id| {
            if !found.contains(&id) {
                found.push(id);
            }
        });
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
        let opts = revisions::options(&config, kind)?;
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

    /// The object `name` stands for, if any (no messages).
    pub fn resolve(&self, name: &str) -> Option<ObjectId> {
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
        let flags = Flags {
            quiet: true,
            ..Flags::default()
        };
        resolver.resolve_with(name, flags).ok()
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
            setup.absolute_git_dir(),
            setup.cwd.join(&setup.common_dir),
            setup.format.object_format,
        );
        UpdateHooks {
            objects,
            writer: committer.map(|c| ReflogWriter::new(logs, c)),
            store: ref_store(setup, config),
            symbolic: Default::default(),
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
}
