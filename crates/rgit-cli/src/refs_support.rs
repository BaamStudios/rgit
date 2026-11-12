// SPDX-License-Identifier: MIT
//! Shared glue for the reference commands (`update-ref`, `show-ref`,
//! `for-each-ref`, `pack-refs`): the reference store with its settings,
//! object lookups over loose objects and packs, unique abbreviations and a
//! small object-name parser.
//!
//! The object-name parser understands full and abbreviated object ids,
//! reference names with git's lookup rules (`gitrevisions(7)`, "<refname>"),
//! `@`, and the suffixes `^`, `^<n>`, `~<n>`, `^{}` and `^{<type>}`. The
//! full revision syntax is #32.

use std::path::Path;

use rgit_config::{CommandLine, ConfigSet, LoadOptions, SystemEnv};
use rgit_hash::{HashKind, ObjectId, Prefix};
use rgit_object::ObjectKind;
use rgit_odb::{LooseOptions, LooseStore};
use rgit_pack::{DeltaBaseCache, PackSet};
use rgit_refs::files::{FilesOptions, FilesStore};
use rgit_refs::{LogAllRefUpdates, RefStore};
use rgit_repo::Setup;

use crate::Context;
use crate::repo;

/// The configuration of the repository (all standard scopes).
pub fn load_config(ctx: &Context, setup: &Setup) -> ConfigSet {
    let env = SystemEnv::default();
    let opts = LoadOptions {
        env: &env,
        repo: None,
        includes: true,
    };
    let repo_cfg = rgit_config::sources::RepoConfig {
        common_dir: setup.common_dir.clone(),
        git_dir: setup.git_dir.clone(),
        worktree_config: None,
    };
    let cl: CommandLine = repo::command_line(ctx);
    let mut set = ConfigSet::new();
    let _ = rgit_config::sources::load_standard(
        opts,
        Some(&repo_cfg),
        &cl,
        rgit_config::sources::ScopeFilter::default(),
        &mut set,
    );
    set
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

    /// The only object whose id starts with `hex`.
    pub fn unique_prefix(&self, hex: &str) -> Option<ObjectId> {
        let prefix = Prefix::from_hex(self.hash, hex.as_bytes()).ok()?;
        let found = self.prefix_matches(&prefix, 2);
        (found.len() == 1).then(|| found[0])
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

/// The reference a short name stands for, with git's lookup order.
pub fn dwim_ref(store: &FilesStore, name: &str) -> Option<String> {
    let name = if name == "@" { "HEAD" } else { name };
    let candidates = [
        name.to_owned(),
        format!("refs/{name}"),
        format!("refs/tags/{name}"),
        format!("refs/heads/{name}"),
        format!("refs/remotes/{name}"),
        format!("refs/remotes/{name}/HEAD"),
    ];
    for c in candidates {
        if c.split('/').any(|p| p.is_empty() || p == "." || p == "..") {
            continue;
        }
        if c == name && !c.starts_with("refs/") && !rgit_refs::name::is_root_ref_syntax(&c) {
            continue;
        }
        if let Ok(Some(r)) = store.resolve(&c) {
            if r.oid.is_some() {
                return Some(c);
            }
        }
    }
    None
}

/// Resolves an object name (see the module documentation).
pub fn get_oid(store: &FilesStore, objects: &Objects, spec: &str) -> Option<ObjectId> {
    let (base, suffix) = split_suffix(spec);
    let mut id = base_oid(store, objects, base)?;
    let mut rest = suffix;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("^{") {
            let end = r.find('}')?;
            let ty = &r[..end];
            rest = &r[end + 1..];
            id = match ty {
                "" => objects.peel(&id)?,
                "object" => {
                    objects.kind(&id)?;
                    id
                }
                _ => {
                    let want = ObjectKind::from_name(ty.as_bytes())?;
                    peel_to(objects, id, want)?
                }
            };
        } else if let Some(r) = rest.strip_prefix('^') {
            let digits = r.bytes().take_while(u8::is_ascii_digit).count();
            let n: usize = if digits == 0 {
                1
            } else {
                r[..digits].parse().ok()?
            };
            rest = &r[digits..];
            let commit = peel_to(objects, id, ObjectKind::Commit)?;
            if n == 0 {
                id = commit;
            } else {
                id = *parents(objects, &commit)?.get(n - 1)?;
            }
        } else if let Some(r) = rest.strip_prefix('~') {
            let digits = r.bytes().take_while(u8::is_ascii_digit).count();
            let n: usize = if digits == 0 {
                1
            } else {
                r[..digits].parse().ok()?
            };
            rest = &r[digits..];
            id = peel_to(objects, id, ObjectKind::Commit)?;
            for _ in 0..n {
                id = *parents(objects, &id)?.first()?;
            }
        } else {
            return None;
        }
    }
    Some(id)
}

/// Splits `name^{..}~2` into `name` and the suffix chain.
fn split_suffix(spec: &str) -> (&str, &str) {
    match spec.find(['^', '~']) {
        Some(i) if i > 0 => (&spec[..i], &spec[i..]),
        _ => (spec, ""),
    }
}

fn base_oid(store: &FilesStore, objects: &Objects, name: &str) -> Option<ObjectId> {
    let hash = objects.hash();
    let hexlen = hash.hex_len();
    let is_hex = !name.is_empty() && name.bytes().all(|b| b.is_ascii_hexdigit());
    if is_hex && name.len() == hexlen {
        return ObjectId::from_hex(hash, name.as_bytes()).ok();
    }
    if let Some(r) = dwim_ref(store, name) {
        return store.resolve_oid(&r).ok().flatten();
    }
    if is_hex && name.len() >= 4 && name.len() < hexlen {
        return objects.unique_prefix(&name.to_ascii_lowercase());
    }
    None
}

fn parents(objects: &Objects, id: &ObjectId) -> Option<Vec<ObjectId>> {
    let (kind, data) = objects.read(id)?;
    if kind != ObjectKind::Commit {
        return None;
    }
    rgit_object::Commit::parse(objects.hash(), &data)
        .ok()?
        .parents()
        .ok()
}

fn peel_to(objects: &Objects, mut id: ObjectId, want: ObjectKind) -> Option<ObjectId> {
    for _ in 0..64 {
        let (kind, data) = objects.read(&id)?;
        if kind == want {
            return Some(id);
        }
        match kind {
            ObjectKind::Tag => {
                id = rgit_object::Tag::parse(objects.hash(), &data)
                    .ok()?
                    .object()
                    .ok()?
            }
            ObjectKind::Commit if want == ObjectKind::Tree => {
                return rgit_object::Commit::parse(objects.hash(), &data)
                    .ok()?
                    .tree()
                    .ok();
            }
            _ => return None,
        }
    }
    None
}

/// Hooks for reference updates from the command line: object checks and a
/// minimal reflog writer (append and delete).
///
/// The full reflog support of #30 (`ReflogWriter`) replaces the writer
/// part once it is on `main`.
pub struct UpdateHooks<'a> {
    objects: &'a Objects,
    git_dir: std::path::PathBuf,
    ident: Option<Vec<u8>>,
    store: FilesStore,
}

impl<'a> UpdateHooks<'a> {
    /// Hooks for the repository of `setup`.
    pub fn new(setup: &Setup, config: &ConfigSet, objects: &'a Objects) -> Self {
        let env = SystemEnv::default();
        let ident = rgit_ident::IdentConfig::from_config(config)
            .ok()
            .and_then(|ic| {
                rgit_ident::Resolver::new(&ic, &env)
                    .ident(rgit_ident::Role::Committer, rgit_ident::Strictness::Lenient)
                    .ok()
            })
            .map(|id| id.to_bstring().to_vec());
        UpdateHooks {
            objects,
            git_dir: setup.cwd.join(&setup.common_dir),
            ident,
            store: ref_store(setup, config),
        }
    }
}

/// Folds whitespace runs of a reflog message into single spaces and drops
/// leading and trailing whitespace.
fn normalize_reflog_message(msg: &str) -> String {
    msg.split([' ', '\t', '\n', '\r'])
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
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
        use std::io::Write;
        let path = self.git_dir.join("logs").join(&update.name);
        if !update.create && !path.is_file() {
            return Ok(());
        }
        // An unchanged value is not rewritten, so it gets no entry.
        if update.old == update.new
            && matches!(self.store.read_raw(&update.name), Ok(Some(rgit_refs::Target::Object(o))) if o == update.old)
        {
            return Ok(());
        }
        let Some(ident) = &self.ident else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut line = Vec::new();
        line.extend_from_slice(update.old.to_hex().as_bytes());
        line.push(b' ');
        line.extend_from_slice(update.new.to_hex().as_bytes());
        line.push(b' ');
        line.extend_from_slice(ident);
        let msg = normalize_reflog_message(&update.message);
        if !msg.is_empty() {
            line.push(b'\t');
            line.extend_from_slice(msg.as_bytes());
        }
        line.push(b'\n');
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| f.write_all(&line))
            .map_err(|e| format!("unable to append to '{}': {e}", path.display()))
    }

    fn reflog_delete(&mut self, name: &str) {
        let path = self.git_dir.join("logs").join(name);
        let _ = std::fs::remove_file(&path);
        let logs = self.git_dir.join("logs").join("refs");
        let mut dir = path.parent().map(Path::to_path_buf);
        while let Some(d) = dir {
            if d == logs || !d.starts_with(&logs) || d.parent() == Some(logs.as_path()) {
                break;
            }
            if std::fs::remove_dir(&d).is_err() {
                break;
            }
            dir = d.parent().map(Path::to_path_buf);
        }
    }
}
