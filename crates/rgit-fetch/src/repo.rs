// SPDX-License-Identifier: MIT
//! The local repository as a fetch sees it: objects, references, reflog
//! identity and configuration.

use std::path::{Path, PathBuf};

use rgit_config::{ConfigSet, SystemEnv};
use rgit_hash::{HashKind, ObjectId};
use rgit_object::ObjectKind;
use rgit_odb::ObjectDatabase;
use rgit_refs::files::{FilesOptions, FilesStore};
use rgit_refs::reflog::{Committer, FilesReflog, ReflogWriter};
use rgit_refs::{IterItem, IterOptions, LogAllRefUpdates, RefStore, Target};

use crate::{Error, Result};

/// Everything a fetch reads and writes locally.
pub struct LocalRepo {
    /// `$GIT_DIR` (per work tree; `FETCH_HEAD` lives here).
    pub git_dir: PathBuf,
    /// The common directory (objects, refs).
    pub common_dir: PathBuf,
    /// The work tree, if any.
    pub work_tree: Option<PathBuf>,
    /// Hash algorithm.
    pub hash: HashKind,
    /// The configuration.
    pub config: ConfigSet,
    /// Objects (loose, packs, alternates).
    pub odb: ObjectDatabase,
    /// References.
    pub refs: FilesStore,
    /// Identity and time for reflog entries; `None` when no identity can be
    /// determined (then no reflog entries are written).
    pub committer: Option<Committer>,
    /// Configuration warnings to print (`warning: ` prefix).
    pub warnings: Vec<String>,
}

impl LocalRepo {
    /// Open the repository described by `setup` with its configuration.
    pub fn open(setup: &rgit_repo::Setup, config: ConfigSet) -> Result<LocalRepo> {
        let mut warnings = Vec::new();
        let odb = ObjectDatabase::open_for_repo(setup, &config, &mut warnings)?;
        let refs = ref_store(setup, &config);
        let committer = committer(&config);
        let abs = |p: &Path| std::path::absolute(setup.cwd.join(p)).unwrap_or(p.to_path_buf());
        Ok(LocalRepo {
            git_dir: abs(&setup.git_dir),
            common_dir: abs(&setup.common_dir),
            work_tree: setup.work_tree.as_deref().map(abs),
            hash: setup.format.object_format,
            config,
            odb,
            refs,
            committer,
            warnings,
        })
    }

    /// Whether the repository has no work tree.
    pub fn is_bare(&self) -> bool {
        self.work_tree.is_none()
    }

    /// The `objects` directory new packs are written to.
    pub fn objects_dir(&self) -> PathBuf {
        match std::env::var_os("GIT_OBJECT_DIRECTORY").filter(|v| !v.is_empty()) {
            Some(d) => PathBuf::from(d),
            None => self.common_dir.join("objects"),
        }
    }

    /// The branch `HEAD` points to (`refs/heads/...`), also when unborn.
    pub fn current_branch(&self) -> Option<String> {
        match self.refs.read_raw("HEAD") {
            Ok(Some(Target::Symbolic(t))) if t.starts_with("refs/heads/") => Some(t),
            _ => None,
        }
    }

    /// The object a reference resolves to.
    pub fn resolve(&self, name: &str) -> Option<ObjectId> {
        self.refs.resolve_oid(name).ok().flatten()
    }

    /// Every reference below `prefix` (`""`: all of `refs/`) with its
    /// object, in name order.
    pub fn list_refs(&self, prefix: &str) -> Vec<(String, ObjectId)> {
        let opts = IterOptions {
            prefix: prefix.to_owned(),
            include_root_refs: false,
        };
        self.refs
            .iter(&opts)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|i| match i {
                IterItem::Ref(r) => r.oid.map(|o| (r.name, o)),
                _ => None,
            })
            .collect()
    }

    /// The kind of an object, if it exists.
    pub fn kind(&self, id: &ObjectId) -> Option<ObjectKind> {
        self.odb.header(id).ok().flatten().map(|h| h.kind)
    }

    /// Whether the object exists.
    pub fn has_object(&self, id: &ObjectId) -> bool {
        self.odb.contains(id)
    }

    /// See packs written since the database was opened.
    pub fn rescan_packs(&self) {
        if let Some(p) = self.odb.packs() {
            p.set().rescan();
        }
    }

    /// Hooks writing reflog entries for reference updates.
    pub(crate) fn reflog_hooks(&self) -> Option<ReflogWriter> {
        let logs = FilesReflog::new(self.git_dir.clone(), self.common_dir.clone(), self.hash);
        self.committer.clone().map(|c| ReflogWriter::new(logs, c))
    }
}

/// The files reference store with `core.logAllRefUpdates` and lock
/// timeouts from the configuration.
fn ref_store(setup: &rgit_repo::Setup, config: &ConfigSet) -> FilesStore {
    let mut options = FilesOptions::default();
    let log_all = config
        .string("core.logallrefupdates")
        .ok()
        .flatten()
        .and_then(|v| LogAllRefUpdates::parse(&v.to_string()));
    options.log_all_ref_updates = match log_all {
        Some(v) => v,
        None => LogAllRefUpdates::default_for(setup.is_bare()),
    };
    if let Ok(Some(ms)) = config.int("core.packedrefstimeout") {
        options.packed_refs_timeout = rgit_fs::Timeout::from_millis(ms);
    }
    if let Ok(Some(ms)) = config.int("core.filesreflocktimeout") {
        options.ref_lock_timeout = rgit_fs::Timeout::from_millis(ms);
    }
    FilesStore::with_options(
        setup.common_dir.clone(),
        setup.format.object_format,
        options,
    )
}

fn committer(config: &ConfigSet) -> Option<Committer> {
    let env = SystemEnv::default();
    let ic = rgit_ident::IdentConfig::from_config(config).ok()?;
    let id = rgit_ident::Resolver::new(&ic, &env)
        .ident(rgit_ident::Role::Committer, rgit_ident::Strictness::Lenient)
        .ok()?;
    Some(Committer::new(&id.name, &id.email, id.time, id.tz))
}

impl From<String> for Error {
    fn from(s: String) -> Error {
        Error::Fatal(s)
    }
}
