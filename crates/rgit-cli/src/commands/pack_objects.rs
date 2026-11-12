// SPDX-License-Identifier: MIT
//! `git pack-objects` (Documentation/git-pack-objects.adoc), first part:
//! `--stdout` packs from an object list or from `--revs`, without delta
//! search (existing deltas are reused). See `rgit_pack::write`.
//!
//! Not yet: writing `<base-name>-<hash>.{pack,idx}` (needs the `.idx`
//! writer from index-pack), progress output, `--include-tag`,
//! `--shallow`, `--incremental`, `--local`, `--unpacked`, filters,
//! bitmaps. `--thin` is accepted, but the pack is never thin (which is
//! always valid). Alternates are not read yet.

use std::io::{BufRead, BufWriter, Write};
use std::path::Path;

use rgit_config::{CommandLine, ConfigSet, LoadOptions, SystemEnv};
use rgit_hash::{HashKind, ObjectId, Prefix};
use rgit_odb::{LooseOptions, LooseStore, ObjectStore};
use rgit_pack::write::{self as pw, ListedObject, PackSetSource, SourceError, Tip, WriteOptions};
use rgit_pack::{Object, ObjectKind, PackSet};
use rgit_refs::files::FilesStore;
use rgit_refs::name::{RefNameFlags, is_valid_refname};
use rgit_refs::{IterItem, IterOptions, MAX_SYMREF_DEPTH, RefStore, Target};

use crate::messages::pack_objects as msg;
use crate::prelude::*;
use crate::repo::{self, Mode};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let quiet = Cell::new(false);
    let progress = Cell::new(false);
    let all_progress = Cell::new(false);
    let all_progress_implied = Cell::new(false);
    let window = Cell::new(10);
    let window_memory = Cell::new(0u64);
    let depth = Cell::new(50);
    let reuse_delta = Cell::new(true);
    let reuse_object = Cell::new(true);
    let ofs_delta = Cell::new(false);
    let threads = Cell::new(0);
    let non_empty = Cell::new(false);
    let revs = Cell::new(false);
    let all = Cell::new(false);
    let stdout = Cell::new(false);
    let sparse = Cell::new(true);
    let thin = Cell::new(false);
    let local = Cell::new(false);
    let path_walk = Cell::new(false);
    let use_bitmap = Cell::new(false);
    let name_hash_version = Cell::new(1);
    let compression = Cell::new(i32::MIN);
    // Options that change what is packed or written, not implemented yet:
    // remember the first one given and refuse to run.
    let unsupported: RefCell<Option<String>> = RefCell::new(None);
    let reject = |name: &'static str| {
        let unsupported = &unsupported;
        move |_: Option<&str>, unset: bool| -> Result<(), CallbackError> {
            if !unset {
                unsupported.borrow_mut().get_or_insert_with(|| format!("--{name}"));
            }
            Ok(())
        }
    };
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag('q', "quiet", &quiet, msg::H_QUIET),
            Opt::flag(None, "progress", &progress, msg::H_PROGRESS),
            Opt::flag(None, "all-progress", &all_progress, msg::H_ALL_PROGRESS),
            Opt::flag(
                None,
                "all-progress-implied",
                &all_progress_implied,
                msg::H_ALL_PROGRESS_IMPLIED,
            ),
            Opt::callback(
                None,
                "index-version",
                Some("<version>[,<offset>]"),
                msg::H_INDEX_VERSION,
                reject("index-version"),
            )
            .literal_argh()
            .noneg(),
            Opt::callback(
                None,
                "max-pack-size",
                Some("n"),
                msg::H_MAX_PACK_SIZE,
                reject("max-pack-size"),
            )
            .noneg(),
            Opt::flag(None, "local", &local, msg::H_LOCAL),
            Opt::callback(None, "incremental", None, msg::H_INCREMENTAL, reject("incremental"))
                .noarg(),
            Opt::integer(None, "window", &window, msg::H_WINDOW),
            Opt::unsigned(None, "window-memory", &window_memory, msg::H_WINDOW_MEMORY),
            Opt::integer(None, "depth", &depth, msg::H_DEPTH),
            Opt::flag(None, "reuse-delta", &reuse_delta, msg::H_REUSE_DELTA),
            Opt::flag(None, "reuse-object", &reuse_object, msg::H_REUSE_OBJECT),
            Opt::flag(None, "delta-base-offset", &ofs_delta, msg::H_DELTA_BASE_OFFSET),
            Opt::integer(None, "threads", &threads, msg::H_THREADS),
            Opt::flag(None, "non-empty", &non_empty, msg::H_NON_EMPTY),
            Opt::flag(None, "revs", &revs, msg::H_REVS),
            Opt::callback(None, "unpacked", None, msg::H_UNPACKED, reject("unpacked"))
                .noarg()
                .noneg(),
            Opt::flag(None, "all", &all, msg::H_ALL).noneg(),
            Opt::callback(None, "reflog", None, msg::H_REFLOG, reject("reflog"))
                .noarg()
                .noneg(),
            Opt::callback(
                None,
                "indexed-objects",
                None,
                msg::H_INDEXED_OBJECTS,
                reject("indexed-objects"),
            )
            .noarg()
            .noneg(),
            Opt::callback(
                None,
                "stdin-packs",
                Some("mode"),
                msg::H_STDIN_PACKS,
                reject("stdin-packs"),
            )
            .optional(None),
            Opt::flag(None, "stdout", &stdout, msg::H_STDOUT),
            Opt::callback(None, "include-tag", None, msg::H_INCLUDE_TAG, reject("include-tag"))
                .noarg(),
            Opt::callback(
                None,
                "keep-unreachable",
                None,
                msg::H_KEEP_UNREACHABLE,
                reject("keep-unreachable"),
            )
            .noarg(),
            Opt::callback(
                None,
                "pack-loose-unreachable",
                None,
                msg::H_PACK_LOOSE_UNREACHABLE,
                reject("pack-loose-unreachable"),
            )
            .noarg(),
            Opt::callback(
                None,
                "unpack-unreachable",
                Some("time"),
                msg::H_UNPACK_UNREACHABLE,
                reject("unpack-unreachable"),
            )
            .optional(None),
            Opt::callback(None, "cruft", None, msg::H_CRUFT, reject("cruft")).noarg(),
            Opt::callback(
                None,
                "cruft-expiration",
                Some("time"),
                msg::H_CRUFT_EXPIRATION,
                reject("cruft-expiration"),
            )
            .optional(None),
            Opt::flag(None, "sparse", &sparse, msg::H_SPARSE),
            Opt::flag(None, "thin", &thin, msg::H_THIN),
            Opt::flag(None, "path-walk", &path_walk, msg::H_PATH_WALK),
            Opt::callback(None, "shallow", None, msg::H_SHALLOW, reject("shallow")).noarg(),
            Opt::callback(
                None,
                "honor-pack-keep",
                None,
                msg::H_HONOR_PACK_KEEP,
                reject("honor-pack-keep"),
            )
            .noarg(),
            Opt::callback(None, "keep-pack", Some("name"), msg::H_KEEP_PACK, reject("keep-pack")),
            Opt::integer(None, "compression", &compression, msg::H_COMPRESSION),
            Opt::callback(
                None,
                "keep-true-parents",
                None,
                msg::H_KEEP_TRUE_PARENTS,
                reject("keep-true-parents"),
            )
            .noarg(),
            Opt::flag(None, "use-bitmap-index", &use_bitmap, msg::H_USE_BITMAP_INDEX),
            Opt::callback(
                None,
                "write-bitmap-index",
                None,
                msg::H_WRITE_BITMAP_INDEX,
                reject("write-bitmap-index"),
            )
            .noarg(),
            Opt::callback(None, "filter", Some("args"), msg::H_FILTER, reject("filter")),
            Opt::callback(None, "missing", Some("action"), msg::H_MISSING, reject("missing"))
                .noneg(),
            Opt::callback(
                None,
                "exclude-promisor-objects",
                None,
                msg::H_EXCLUDE_PROMISOR_OBJECTS,
                reject("exclude-promisor-objects"),
            )
            .noarg(),
            Opt::callback(
                None,
                "exclude-promisor-objects-best-effort",
                None,
                msg::H_EXCLUDE_PROMISOR_OBJECTS_BEST_EFFORT,
                reject("exclude-promisor-objects-best-effort"),
            )
            .noarg(),
            Opt::callback(
                None,
                "delta-islands",
                None,
                msg::H_DELTA_ISLANDS,
                reject("delta-islands"),
            )
            .noarg(),
            Opt::callback(
                None,
                "uri-protocol",
                Some("protocol"),
                msg::H_URI_PROTOCOL,
                reject("uri-protocol"),
            ),
            Opt::integer(
                None,
                "name-hash-version",
                &name_hash_version,
                msg::H_NAME_HASH_VERSION,
            ),
        ],
    );
    let args = parser.parse(argv)?.args;
    if all.get() {
        revs.set(true);
    }
    if stdout.get() != args.is_empty() || args.len() > 1 {
        return Err(parser.usage_error().into());
    }
    if let Some(opt) = unsupported.borrow().as_deref() {
        return Err(Die::Fatal(msg::not_supported(opt)));
    }
    if !stdout.get() {
        return Err(Die::Fatal(msg::not_supported("writing <base-name> pack files")));
    }
    let mut max_depth = depth.get().max(0) as u32;
    if max_depth > pw::MAX_DEPTH {
        eprintln!("warning: {}", msg::depth_too_large(depth.get(), pw::MAX_DEPTH));
        max_depth = pw::MAX_DEPTH;
    }

    let setup = repo::setup(ctx, Mode::Required)?;
    let hash = setup.format.object_format;
    let config = load_config(ctx, &setup);
    let level = match compression.get() {
        i32::MIN => config_compression(&config)?,
        l => check_level(l as i64)?,
    };

    let objects_dir = rgit_odb::objects_dir(&setup.common_dir);
    let set = PackSet::open(objects_dir.join("pack"), hash);
    let repo = Repo {
        source: Objects::new(&set, &objects_dir, hash),
        refs: Refs::new(&setup),
        hash,
    };

    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let listed: Vec<ListedObject> = if revs.get() {
        let mut tips = Vec::new();
        if all.get() {
            for id in repo.refs.all() {
                tips.push(Tip { id, exclude: false });
            }
        }
        read_revs(&mut input, &repo, &mut tips)?;
        pw::enumerate(&repo.source, hash, &tips).map_err(|e| Die::Fatal(e.to_string()))?
    } else {
        let ids = read_object_list(&mut input, hash)?;
        pw::describe(&repo.source, hash, &ids).map_err(|e| Die::Fatal(e.to_string()))?
    };
    let ordered = pw::pack_order(&listed);
    if ordered.is_empty() && non_empty.get() {
        return Ok(0);
    }

    let opts = WriteOptions {
        ofs_delta: ofs_delta.get(),
        reuse_delta: reuse_delta.get() && reuse_object.get(),
        reuse_object: reuse_object.get(),
        compression: level,
        max_depth,
    };
    let out = BufWriter::with_capacity(1 << 16, std::io::stdout().lock());
    let packs = set.packs();
    let (_summary, mut out) = pw::write_pack(out, hash, &ordered, &repo.source, &packs, &opts)
        .map_err(|e| Die::Fatal(e.to_string()))?;
    out.flush().map_err(|e| Die::Fatal(e.to_string()))?;
    Ok(0)
}

/// The configuration (all scopes), or an empty set if it cannot be read.
fn load_config(ctx: &Context, setup: &rgit_repo::Setup) -> ConfigSet {
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

/// `pack.compression`, else `core.compression`; `-1` is zlib's default.
fn config_compression(config: &ConfigSet) -> Result<Option<u32>, Die> {
    for key in ["pack.compression", "core.compression"] {
        if let Ok(Some(v)) = config.int(key) {
            return check_level(v);
        }
    }
    Ok(None)
}

fn check_level(level: i64) -> Result<Option<u32>, Die> {
    match level {
        -1 => Ok(None),
        0..=9 => Ok(Some(level as u32)),
        _ => Err(Die::Fatal(msg::bad_compression(level))),
    }
}

fn read_line(input: &mut dyn BufRead) -> Result<Option<String>, Die> {
    let mut buf = Vec::new();
    match input.read_until(b'\n', &mut buf) {
        Ok(0) => Ok(None),
        Ok(_) => Ok(Some(String::from_utf8_lossy(&buf).into_owned())),
        Err(e) => Err(Die::Fatal(e.to_string())),
    }
}

/// The object list: one id per line, optionally followed by a space and a
/// path (a hint for delta search, unused here). Lines starting with `-`
/// name objects the receiver has (used for thin packs); they are skipped.
fn read_object_list(input: &mut dyn BufRead, hash: HashKind) -> Result<Vec<ObjectId>, Die> {
    let mut ids = Vec::new();
    let hex_len = hash.hex_len();
    while let Some(line) = read_line(input)? {
        if line.starts_with('-') {
            continue;
        }
        let hex = line.as_bytes().get(..hex_len);
        let rest = line.as_bytes().get(hex_len..).unwrap_or(b"");
        let id = hex.and_then(|h| ObjectId::from_hex(hash, h).ok());
        match id {
            Some(id) if rest.is_empty() || rest[0] == b' ' || rest[0] == b'\n' => ids.push(id),
            _ => return Err(Die::Fatal(msg::expected_object_id(&line))),
        }
    }
    Ok(ids)
}

/// What resolving revisions needs.
struct Repo<'a> {
    source: Objects<'a>,
    refs: Refs,
    hash: HashKind,
}

/// Revision arguments, one per line: `<rev>`, `^<rev>`, `--not`. An empty
/// line ends the revisions.
fn read_revs(input: &mut dyn BufRead, repo: &Repo<'_>, tips: &mut Vec<Tip>) -> Result<(), Die> {
    let mut not = false;
    while let Some(line) = read_line(input)? {
        let line = line.strip_suffix('\n').unwrap_or(&line);
        if line.is_empty() {
            break;
        }
        if line == "--not" {
            not = !not;
            continue;
        }
        if line.starts_with('-') {
            return Err(Die::Fatal(msg::not_a_rev(line)));
        }
        let (exclude, name) = match line.strip_prefix('^') {
            Some(rest) => (!not, rest),
            None => (not, line),
        };
        let id = resolve(repo, name)?;
        tips.push(Tip { id, exclude });
    }
    Ok(())
}

/// Resolve a revision: `<name>` followed by any number of `~<n>`, `^<n>`,
/// `^{}`, `^{commit}` or `^{tree}` (`git-rev-parse(1)`, "SPECIFYING
/// REVISIONS").
fn resolve(repo: &Repo<'_>, rev: &str) -> Result<ObjectId, Die> {
    let split = rev.find(['~', '^']).unwrap_or(rev.len());
    let (name, mut suffix) = rev.split_at(split);
    let mut id = resolve_name(repo, name, rev)?;
    let bad = || Die::Fatal(msg::bad_revision(rev));
    while !suffix.is_empty() {
        let op = suffix.as_bytes()[0];
        let rest = &suffix[1..];
        if op == b'^' && rest.starts_with('{') {
            let end = rest.find('}').ok_or_else(bad)?;
            let want = match &rest[1..end] {
                "" => None,
                "commit" => Some(ObjectKind::Commit),
                "tree" => Some(ObjectKind::Tree),
                _ => return Err(Die::Fatal(msg::rev_not_supported(rev))),
            };
            id = peel(repo, id, want).ok_or_else(bad)?;
            suffix = &rest[end + 1..];
            continue;
        }
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let n: usize = if digits == 0 {
            1
        } else {
            rest[..digits].parse().map_err(|_| bad())?
        };
        suffix = &rest[digits..];
        id = peel(repo, id, Some(ObjectKind::Commit)).ok_or_else(bad)?;
        if op == b'^' {
            if n > 0 {
                let parents = parents(repo, &id).ok_or_else(bad)?;
                id = *parents.get(n - 1).ok_or_else(bad)?;
            }
        } else {
            for _ in 0..n {
                id = *parents(repo, &id).ok_or_else(bad)?.first().ok_or_else(bad)?;
            }
        }
    }
    Ok(id)
}

/// A full id, a ref name, or an unambiguous id prefix.
fn resolve_name(repo: &Repo<'_>, name: &str, rev: &str) -> Result<ObjectId, Die> {
    let hash = repo.hash;
    let is_hex = !name.is_empty() && name.bytes().all(|b| b.is_ascii_hexdigit());
    if is_hex && name.len() == hash.hex_len() {
        let id = ObjectId::from_hex(hash, name.as_bytes()).expect("hex checked");
        return match pw::ObjectSource::kind(&repo.source, &id) {
            Ok(Some(_)) => Ok(id),
            _ => Err(Die::Fatal(msg::bad_object(name))),
        };
    }
    if let Some(id) = repo.refs.dwim(name) {
        return Ok(id);
    }
    if is_hex && name.len() >= 4 && name.len() < hash.hex_len() {
        if let Some(id) = repo.source.unique_prefix(name) {
            return Ok(id);
        }
    }
    if rev.contains([':', '@', '{']) {
        return Err(Die::Fatal(msg::rev_not_supported(rev)));
    }
    Err(Die::Fatal(msg::bad_revision(rev)))
}

/// Follow tags (and from a commit to its tree when `want` is a tree)
/// until an object of type `want` (any non-tag for `None`).
fn peel(repo: &Repo<'_>, mut id: ObjectId, want: Option<ObjectKind>) -> Option<ObjectId> {
    for _ in 0..64 {
        let o = pw::ObjectSource::read(&repo.source, &id).ok()??;
        if Some(o.kind) == want || (want.is_none() && o.kind != ObjectKind::Tag) {
            return Some(id);
        }
        id = match o.kind {
            ObjectKind::Tag => rgit_object::Tag::parse(repo.hash, &o.data)
                .ok()?
                .object()
                .ok()?,
            ObjectKind::Commit if want == Some(ObjectKind::Tree) => {
                rgit_object::Commit::parse(repo.hash, &o.data)
                    .ok()?
                    .tree()
                    .ok()?
            }
            _ => return None,
        };
    }
    None
}

fn parents(repo: &Repo<'_>, id: &ObjectId) -> Option<Vec<ObjectId>> {
    let o = pw::ObjectSource::read(&repo.source, id).ok()??;
    rgit_object::Commit::parse(repo.hash, &o.data)
        .ok()?
        .parents()
        .ok()
}

/// Packed objects (through `rgit_pack`) plus loose ones (through
/// `rgit_odb`). Alternates are not read yet.
struct Objects<'a> {
    packed: PackSetSource<'a>,
    loose: LooseStore,
}

impl<'a> Objects<'a> {
    fn new(set: &'a PackSet, objects_dir: &Path, hash: HashKind) -> Objects<'a> {
        Objects {
            packed: PackSetSource::new(set),
            loose: LooseStore::new(objects_dir, hash, LooseOptions::default()),
        }
    }

    /// The only object whose id starts with `hex`, packed or loose.
    fn unique_prefix(&self, hex: &str) -> Option<ObjectId> {
        let prefix = Prefix::from_hex(self.loose.hash_kind(), hex.as_bytes()).ok()?;
        let mut found = self.packed.set().lookup_prefix(&prefix, 2);
        let _ = self.loose.for_each_prefix_match(&prefix, &mut |id| {
            if !found.contains(&id) {
                found.push(id);
            }
        });
        (found.len() == 1).then(|| found[0])
    }
}

fn pack_kind(kind: rgit_object::ObjectKind) -> ObjectKind {
    match kind {
        rgit_object::ObjectKind::Commit => ObjectKind::Commit,
        rgit_object::ObjectKind::Tree => ObjectKind::Tree,
        rgit_object::ObjectKind::Blob => ObjectKind::Blob,
        rgit_object::ObjectKind::Tag => ObjectKind::Tag,
    }
}

impl pw::ObjectSource for Objects<'_> {
    fn read(&self, id: &ObjectId) -> Result<Option<Object>, SourceError> {
        let packed = self.packed.read(id);
        if let Ok(Some(o)) = packed {
            return Ok(Some(o));
        }
        match self.loose.read(id) {
            Ok(Some(o)) => Ok(Some(Object {
                kind: pack_kind(o.kind),
                data: o.data,
            })),
            Ok(None) => packed,
            Err(e) => Err(e.into()),
        }
    }

    fn kind(&self, id: &ObjectId) -> Result<Option<ObjectKind>, SourceError> {
        if let Ok(Some(k)) = self.packed.kind(id) {
            return Ok(Some(k));
        }
        Ok(self.loose.header(id)?.map(|h| pack_kind(h.kind)))
    }
}

/// References through `rgit_refs`: per-worktree names (`HEAD`, ...) from
/// the git directory, the rest from the common directory.
struct Refs {
    worktree: FilesStore,
    common: FilesStore,
}

impl Refs {
    fn new(setup: &rgit_repo::Setup) -> Refs {
        let kind = setup.format.object_format;
        Refs {
            worktree: FilesStore::new(setup.absolute_git_dir(), kind),
            common: FilesStore::new(setup.cwd.join(&setup.common_dir), kind),
        }
    }

    fn store(&self, name: &str) -> &FilesStore {
        let per_worktree = !name.starts_with("refs/")
            || name.starts_with("refs/bisect/")
            || name.starts_with("refs/worktree/")
            || name.starts_with("refs/rewritten/");
        if per_worktree {
            &self.worktree
        } else {
            &self.common
        }
    }

    /// Follow `name` (and symbolic refs) to an object id.
    fn resolve(&self, name: &str) -> Option<ObjectId> {
        if !is_valid_refname(name.as_bytes(), RefNameFlags::ONELEVEL) {
            return None;
        }
        let mut name = name.to_string();
        for _ in 0..MAX_SYMREF_DEPTH {
            match self.store(&name).read_raw(&name).ok()?? {
                Target::Object(id) => return Some(id),
                Target::Symbolic(next) => name = next,
            }
        }
        None
    }

    /// `<refname>` lookup order of `git-rev-parse(1)`.
    fn dwim(&self, name: &str) -> Option<ObjectId> {
        let root = !name.is_empty() && name.bytes().all(|b| b.is_ascii_uppercase() || b == b'_');
        let candidates = [
            name.to_string(),
            format!("refs/{name}"),
            format!("refs/tags/{name}"),
            format!("refs/heads/{name}"),
            format!("refs/remotes/{name}"),
            format!("refs/remotes/{name}/HEAD"),
        ];
        candidates
            .iter()
            .filter(|c| c.starts_with("refs/") || root)
            .find_map(|c| self.resolve(c))
    }

    /// `HEAD` and every reference under `refs/` (`--all`).
    fn all(&self) -> Vec<ObjectId> {
        let mut out: Vec<ObjectId> = self.resolve("HEAD").into_iter().collect();
        if let Ok(items) = self.common.iter(&IterOptions::default()) {
            for item in items {
                if let IterItem::Ref(r) = item {
                    out.extend(r.oid);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_list_lines() {
        let mut input: &[u8] = b"0123456789012345678901234567890123456789\n\
            1123456789012345678901234567890123456789 some/path\n\
            -2123456789012345678901234567890123456789\n";
        let ids = read_object_list(&mut input, HashKind::Sha1).unwrap();
        assert_eq!(ids.len(), 2);
        let mut bad: &[u8] = b"HEAD\n";
        assert!(read_object_list(&mut bad, HashKind::Sha1).is_err());
    }

    #[test]
    fn levels() {
        assert_eq!(check_level(-1).unwrap(), None);
        assert_eq!(check_level(9).unwrap(), Some(9));
        assert!(check_level(10).is_err());
    }

}
