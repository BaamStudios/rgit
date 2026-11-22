// SPDX-License-Identifier: MIT
//! `git diff` (Documentation/git-diff.adoc): work tree against index,
//! `--cached`/`--staged [<commit>]`, `<commit>` against the work tree,
//! `<commit> <commit>`, `A..B`, `A...B` and `--no-index <path> <path>`.
//! No blob or combined diffs.

use std::path::{Path, PathBuf};

use rgit_hash::{HashKind, ObjectId};
use rgit_object::ObjectKind;

use crate::diff::collect::{self, Pair, Side, TreeDiff, WorkTree};
use crate::diff::options::{DiffOpts, usage_exit};
use crate::diff::output::Output;
use crate::diff::{self};
use crate::messages::{self as messages, diff as msg};
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;

fn usage() -> String {
    format!("{}{}", msg::DIFF_USAGE, msg::COMMON_OPTIONS)
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    if let Some(code) = diff::help(argv, &usage()) {
        return Ok(code);
    }
    let mut cached = false;
    let mut no_index = false;
    let parsed = diff::parse_args(
        &argv[1..],
        |a| match a {
            "--cached" | "--staged" => {
                cached = true;
                Ok(true)
            }
            "--no-index" => {
                no_index = true;
                Ok(true)
            }
            "--merge-base" => Err(Die::Fatal(msg::not_supported(a))),
            _ => Ok(false),
        },
        |a| usage_exit(format!("{}{}", msg::invalid_option(a), usage())),
    )?;
    let mut opts = parsed.opts;
    let formats = opts.formats(true)?;

    let repo = Repo::open_gently(ctx)?;
    if repo.is_none() && !opts.find_object.is_empty() {
        return Err(usage_exit(msg::FIND_OBJECT_NEEDS_REPO.to_string()));
    }
    if repo.is_none() && !no_index {
        let all_exist = parsed.args.len() == 2
            && parsed.args.iter().all(|p| Path::new(p).symlink_metadata().is_ok());
        if !all_exist {
            // The usual error outside a repository.
            Repo::open(ctx, Mode::RequiredQuiet)?;
        }
        no_index = true;
    }
    // Inside a repository two existing paths of which one lies outside
    // the work tree are compared as with --no-index.
    if let (Some(r), false, 2) = (&repo, no_index, parsed.args.len()) {
        if let Some(top) = r.setup.work_tree.as_ref().and_then(|w| w.canonicalize().ok()) {
            let cwd = &r.setup.original_cwd;
            let outside = |p: &String| {
                cwd.join(p)
                    .canonicalize()
                    .ok()
                    .map(|c| !c.starts_with(&top))
            };
            let states: Vec<Option<bool>> = parsed.args.iter().map(outside).collect();
            if states.iter().all(Option::is_some) && states.contains(&Some(true)) {
                no_index = true;
            }
        }
    }
    if no_index {
        return run_no_index(ctx, repo.as_ref(), &opts, &parsed.args);
    }
    let mut repo = repo.expect("repository");

    // Revisions first, then paths.
    let split = parsed.dashdash_at.unwrap_or(parsed.args.len());
    let mut revs: Vec<ObjectId> = Vec::new();
    let mut symmetric = false;
    let mut paths: Vec<String> = Vec::new();
    for (i, arg) in parsed.args[..split].iter().enumerate() {
        if let Some(r) = range(ctx, &mut repo, arg)? {
            symmetric |= r.2;
            revs.push(r.0);
            revs.push(r.1);
            continue;
        }
        match repo.resolve(arg) {
            Some(id) if paths.is_empty() => revs.push(id),
            _ => {
                if parsed.dashdash_at.is_none() {
                    let exists = |a: &str| {
                        repo.setup.work_tree.is_some()
                            && Path::new(a).symlink_metadata().is_ok()
                    };
                    if !exists(arg) {
                        return Err(Die::Fatal(if paths.is_empty() && i == revs.len() {
                            messages::ambiguous_argument(arg)
                        } else {
                            messages::no_such_path(arg)
                        }));
                    }
                }
                paths.push(arg.clone());
            }
        }
    }
    paths.extend(parsed.args[split..].iter().cloned());
    let _ = symmetric;

    let pathspec = diff::pathspec(ctx, &repo, &paths)?;
    diff::check_max_depth(&opts, &pathspec)?;
    let mut trees = Vec::new();
    for id in &revs {
        let t = repo
            .peel_to(*id, ObjectKind::Tree)
            .ok_or_else(|| Die::Fatal(msg::not_supported("diffs of blobs")))?;
        trees.push(t);
    }
    let head = if trees.is_empty() && cached {
        match repo.resolve("HEAD").and_then(|h| repo.peel_to(h, ObjectKind::Tree)) {
            Some(t) => t,
            None => rgit_hash::hash_object(repo.hash, "tree", b"")
                .map_err(|e| Die::Fatal(e.to_string()))?,
        }
    } else {
        ObjectId::null(repo.hash)
    };
    opts.resolve_objects(&mut repo)?;
    let repo = repo;
    let (mut pairs, mnemonic) = match (trees.len(), cached) {
        (0, false) => {
            let root = repo.work_tree()?;
            let (index, mtime) = repo.read_index()?;
            let checker = repo.checker(mtime)?;
            let submodules =
                crate::submodules::IgnoreRules::for_index(&repo, &index, opts.ignore_submodules()?, true)?;
            let wt = WorkTree {
                root: &root,
                checker: &checker,
                hash: repo.hash,
                submodules: submodules.as_ref(),
                dirty: Default::default(),
            };
            let mut pairs = collect::files(&index, &pathspec, &wt, false)?;
            // --ita-visible-in-index: intent-to-add entries are empty files
            // in the index, so the work tree file is a modification.
            if opts.ita_invisible == Some(false) {
                for p in &mut pairs {
                    let ita = index
                        .entries()
                        .iter()
                        .any(|e| e.path() == &p.path[..] && e.flags.intent_to_add);
                    if ita && p.old.is_none() {
                        let empty = rgit_hash::hash_object(repo.hash, "blob", b"")
                            .map_err(|e| Die::Fatal(e.to_string()))?;
                        let mode = p.new.as_ref().map_or(0o100644, |s| s.mode);
                        p.old = Some(collect::Side {
                            mode,
                            oid: empty,
                            file: None,
                        });
                    }
                }
            }
            (pairs, Some(("i", "w")))
        }
        (0 | 1, true) | (1, false) => {
            let tree = match trees.first() {
                Some(t) => *t,
                None => head,
            };
            let (index, mtime) = repo.read_index()?;
            if cached {
                let ita_invisible = opts.ita_invisible.unwrap_or(true);
                let pairs = collect::index(&repo, &tree, &index, &pathspec, None, ita_invisible)?;
                let pairs = if opts.copies_harder {
                    collect::with_unmodified(pairs, collect::tree_files(&repo, &tree, &pathspec)?)
                } else {
                    pairs
                };
                (
                    pairs,
                    Some(("c", "i")),
                )
            } else {
                let root = repo.work_tree()?;
                let checker = repo.checker(mtime)?;
                let submodules = crate::submodules::IgnoreRules::for_index(
                    &repo,
                    &index,
                    opts.ignore_submodules()?,
                    true,
                )?;
                let wt = WorkTree {
                    root: &root,
                    checker: &checker,
                    hash: repo.hash,
                    submodules: submodules.as_ref(),
                    dirty: Default::default(),
                };
                (
                    collect::index(&repo, &tree, &index, &pathspec, Some(&wt), true)?,
                    Some(("c", "w")),
                )
            }
        }
        (2, false) => {
            let mut w = TreeDiff {
                repo: &repo,
                pathspec: &pathspec,
                recursive: true,
                show_trees: false,
                max_depth: opts.max_depth,
                pairs: Vec::new(),
            };
            w.walk(Some(&trees[0]), Some(&trees[1]), b"")?;
            let pairs = if opts.copies_harder {
                collect::with_unmodified(w.pairs, collect::tree_files(&repo, &trees[0], &pathspec)?)
            } else {
                w.pairs
            };
            (pairs, None)
        }
        _ => return Err(Die::Fatal(msg::not_supported("this form of diff"))),
    };
    crate::submodules::IgnoreRules::drop_ignored(&repo, &mut pairs, opts.ignore_submodules()?, true)?;
    page(ctx, Some(&repo), &opts)?;
    let mut out = Output::new(Some(&repo), repo.hash, &opts, formats, true)?;
    out.set_color(&opts)?;
    out.combined_unmerged = trees.is_empty() && !cached;
    if let Some((s, d)) = mnemonic {
        out.mnemonic(&opts, s, d);
    }
    let (bytes, changed) = out.render(pairs)?;
    Ok(out.finish(&bytes, changed, &opts))
}

/// Start the pager (`pager.diff`, on by default) unless the exit code is
/// asked for.
fn page(ctx: &Context, repo: Option<&Repo>, opts: &DiffOpts) -> Result<(), Die> {
    if opts.wants_exit_code() {
        return Ok(());
    }
    match repo {
        Some(r) => crate::pager::setup_for(ctx, &r.config, "diff", true),
        None => crate::pager::setup(ctx, "diff", true),
    }
}

/// `A..B` or `A...B` (an empty side is `HEAD`): (old, new, symmetric).
fn range(ctx: &Context, repo: &mut Repo, arg: &str) -> Result<Option<(ObjectId, ObjectId, bool)>, Die> {
    let (a, b, sym) = if let Some(i) = arg.find("...") {
        (&arg[..i], &arg[i + 3..], true)
    } else if let Some(i) = arg.find("..") {
        (&arg[..i], &arg[i + 2..], false)
    } else {
        return Ok(None);
    };
    if a.is_empty() && b.is_empty() {
        return Ok(None);
    }
    let a = if a.is_empty() { "HEAD" } else { a };
    let b = if b.is_empty() { "HEAD" } else { b };
    let (Some(ia), Some(ib)) = (repo.resolve(a), repo.resolve(b)) else {
        return Ok(None);
    };
    if !sym {
        return Ok(Some((ia, ib, false)));
    }
    let ca = repo.peel_to(ia, ObjectKind::Commit);
    let cb = repo.peel_to(ib, ObjectKind::Commit);
    let (Some(ca), Some(cb)) = (ca, cb) else {
        return Ok(None);
    };
    let bases = diff::merge_bases(ctx, &ca, &cb)?;
    let Some(base) = bases.first() else {
        return Err(Die::Fatal(msg::no_merge_base(a, b)));
    };
    if bases.len() > 1 {
        eprintln!("{}", msg::multiple_merge_bases(a, b, &base.to_hex()));
    }
    Ok(Some((*base, ib, true)))
}

/// `--no-index`: two files or directories outside of git's control,
/// `-` for standard input, further arguments are pathspecs (only for
/// two directories).
fn run_no_index(ctx: &Context, repo: Option<&Repo>, opts: &DiffOpts, args: &[String]) -> CmdResult {
    if args.len() < 2 {
        return Err(usage_exit(msg::NO_INDEX_USAGE.to_string()));
    }
    let hash = repo.map_or(HashKind::Sha1, |r| r.hash);
    let formats = opts.formats(true)?;
    // The paths are relative to the directory the command started in,
    // not to the top of the work tree.
    if let Some(r) = repo {
        let _ = std::env::set_current_dir(&r.setup.original_cwd);
    }
    let (a, b) = (PathBuf::from(&args[0]), PathBuf::from(&args[1]));
    let stdin_path = Path::new("-");
    let kind = |p: &Path| -> Result<NoIndexKind, Die> {
        if p == stdin_path {
            return Ok(NoIndexKind::Stdin);
        }
        let m = std::fs::symlink_metadata(p)
            .map_err(|_| Die::Error(msg::could_not_access(&p.to_string_lossy()), 1))?;
        Ok(if m.is_dir() {
            NoIndexKind::Dir
        } else if is_fifo(p) {
            NoIndexKind::Pipe
        } else {
            NoIndexKind::File
        })
    };
    let (ka, kb) = (kind(&a)?, kind(&b)?);
    let dirs = ka == NoIndexKind::Dir || kb == NoIndexKind::Dir;
    for (k, other) in [(ka, kb), (kb, ka)] {
        if other == NoIndexKind::Dir {
            match k {
                NoIndexKind::Stdin => return Err(Die::Fatal(msg::NO_INDEX_STDIN_DIR.to_string())),
                NoIndexKind::Pipe => return Err(Die::Fatal(msg::NO_INDEX_PIPE_DIR.to_string())),
                _ => {}
            }
        }
    }
    // Pathspecs only for two directories.
    let specs = &args[2..];
    if !specs.is_empty() && (ka != NoIndexKind::Dir || kb != NoIndexKind::Dir) {
        return Err(usage_exit(format!(
            "{}{}",
            msg::NO_INDEX_PATHSPEC_NEEDS_DIRS,
            msg::NO_INDEX_USAGE
        )));
    }
    let pathspec = if specs.is_empty() {
        None
    } else {
        if specs.iter().any(|s| Path::new(s).is_absolute()) {
            return Err(usage_exit(msg::NO_INDEX_USAGE.to_string()));
        }
        let parsed = rgit_pathspec::Pathspec::parse(specs, &rgit_pathspec::ParseOptions::default())
            .map_err(|e| Die::Fatal(e.to_string()))?;
        Some(parsed)
    };
    // Standard input and named pipes can be read only once.
    let mut streams = Vec::new();
    for (k, p) in [(ka, &a), (kb, &b)] {
        let mut data = Vec::new();
        match k {
            NoIndexKind::Stdin => {
                std::io::Read::read_to_end(&mut std::io::stdin(), &mut data)
                    .map_err(|e| Die::Fatal(e.to_string()))?;
            }
            NoIndexKind::Pipe => {
                data = std::fs::read(p)
                    .map_err(|_| Die::Error(msg::read_failed(&p.to_string_lossy()), 1))?;
            }
            _ => continue,
        }
        streams.push((p.clone(), data));
    }
    let mut pairs = Vec::new();
    let (mut a, mut b) = (a, b);
    // A file against a directory compares with the file of the same name in it.
    if dirs && ka != kb {
        if ka == NoIndexKind::Dir {
            a = a.join(b.file_name().unwrap_or_default());
        } else {
            b = b.join(a.file_name().unwrap_or_default());
        }
        std::fs::symlink_metadata(if ka == NoIndexKind::Dir { &a } else { &b })
            .map_err(|_| {
                let p = if ka == NoIndexKind::Dir { &a } else { &b };
                Die::Error(msg::could_not_access(&p.to_string_lossy()), 1)
            })?;
    }
    let walk = NoIndexWalk {
        hash,
        pathspec: pathspec.as_ref(),
    };
    walk.walk(&a, &b, b"", &mut pairs)?;
    page(ctx, repo, opts)?;
    let mut out = Output::new(repo, hash, opts, formats, true)?;
    out.convert = false;
    out.streams = streams;
    out.set_color(opts)?;
    if repo.is_some() {
        out.mnemonic(opts, "1", "2");
    }
    let (bytes, changed) = out.render(pairs)?;
    let code = out.finish(&bytes, changed, opts);
    Ok(i32::from(changed) | code)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoIndexKind {
    Stdin,
    Pipe,
    Dir,
    File,
}

/// A named pipe (also behind a symbolic link).
fn is_fifo(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        std::fs::metadata(p).is_ok_and(|m| m.file_type().is_fifo())
    }
    #[cfg(not(unix))]
    {
        let _ = p;
        false
    }
}

fn path_bytes(p: &Path) -> Vec<u8> {
    p.as_os_str().as_encoded_bytes().to_vec()
}

fn file_side(hash: HashKind, p: &Path) -> Option<Side> {
    // `/dev/null` stands for a missing file.
    if p == Path::new("/dev/null") {
        return None;
    }
    if p == Path::new("-") || is_fifo(p) {
        return Some(Side {
            mode: 0o100644,
            oid: ObjectId::null(hash),
            file: Some(p.to_path_buf()),
        });
    }
    let m = std::fs::symlink_metadata(p).ok()?;
    if m.is_dir() {
        return None;
    }
    let mode = if m.file_type().is_symlink() {
        0o120000
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if m.permissions().mode() & 0o100 != 0 { 0o100755 } else { 0o100644 }
        }
        #[cfg(not(unix))]
        {
            0o100644
        }
    };
    Some(Side {
        mode,
        oid: ObjectId::null(hash),
        file: Some(p.to_path_buf()),
    })
}

/// Joins a directory and a name without doubling a trailing slash.
fn join(dir: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let mut s = dir.as_os_str().to_os_string();
    if !s.as_encoded_bytes().ends_with(b"/") {
        s.push("/");
    }
    s.push(name);
    PathBuf::from(s)
}

struct NoIndexWalk<'a> {
    hash: HashKind,
    pathspec: Option<&'a rgit_pathspec::Pathspec>,
}

impl NoIndexWalk<'_> {
    fn is_dir(p: &Path) -> bool {
        p != Path::new("-") && std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
    }

    /// Whether the pathspec lets `rel` (relative to the compared
    /// directories) through; directories only need to be able to contain
    /// a match.
    fn wanted(&self, rel: &[u8], dir: bool) -> bool {
        let Some(ps) = self.pathspec else {
            return true;
        };
        if dir {
            ps.dir_interest(rel) != rgit_pathspec::DirInterest::None
        } else {
            ps.match_path(rel).is_some()
        }
    }

    fn names(p: &Path) -> Vec<std::ffi::OsString> {
        let mut v: Vec<_> = std::fs::read_dir(p)
            .map(|rd| rd.flatten().map(|e| e.file_name()).collect())
            .unwrap_or_default();
        v.sort();
        v
    }

    /// Everything below `dir` as additions (`side_b`) or deletions.
    fn one_side(&self, dir: &Path, rel: &[u8], side_b: bool, pairs: &mut Vec<Pair>) -> Result<(), Die> {
        for n in Self::names(dir) {
            let p = join(dir, &n);
            let mut r = rel.to_vec();
            if !r.is_empty() {
                r.push(b'/');
            }
            r.extend_from_slice(n.as_encoded_bytes());
            if Self::is_dir(&p) {
                if self.wanted(&r, true) {
                    self.one_side(&p, &r, side_b, pairs)?;
                }
            } else if self.wanted(&r, false) {
                let s = self.hashed(file_side(self.hash, &p))?;
                pairs.push(if side_b {
                    let mut pair = Pair::new(path_bytes(&p), None, s);
                    pair.old_path = b"/dev/null".to_vec();
                    pair
                } else {
                    let mut pair = Pair::new(b"/dev/null".to_vec(), s, None);
                    pair.old_path = path_bytes(&p);
                    pair
                });
            }
        }
        Ok(())
    }

    /// A file on one side only is hashed (git looks for renames then).
    fn hashed(&self, side: Option<Side>) -> Result<Option<Side>, Die> {
        let Some(mut s) = side else {
            return Ok(None);
        };
        let path = s.file.as_ref().expect("file");
        let data = if s.mode == 0o120000 {
            std::fs::read_link(path).map(|t| t.into_os_string().into_encoded_bytes())
        } else {
            std::fs::read(path)
        };
        if let Ok(d) = data {
            s.oid = rgit_hash::hash_object(self.hash, "blob", &d)
                .map_err(|e| Die::Fatal(e.to_string()))?;
        }
        Ok(Some(s))
    }

    fn walk(&self, a: &Path, b: &Path, rel: &[u8], pairs: &mut Vec<Pair>) -> Result<(), Die> {
        let (da, db) = (Self::is_dir(a), Self::is_dir(b));
        if da && db {
            let mut all = Self::names(a);
            all.extend(Self::names(b));
            all.sort();
            all.dedup();
            for n in all {
                let mut r = rel.to_vec();
                if !r.is_empty() {
                    r.push(b'/');
                }
                r.extend_from_slice(n.as_encoded_bytes());
                let (pa, pb) = (join(a, &n), join(b, &n));
                let dir = Self::is_dir(&pa) || Self::is_dir(&pb);
                if self.wanted(&r, dir) {
                    self.walk(&pa, &pb, &r, pairs)?;
                }
            }
            return Ok(());
        }
        // A directory on one side only, or against a file of the same
        // name: the file goes, everything in the directory comes (or the
        // other way round).
        let exists = |p: &Path| p == Path::new("-") || std::fs::symlink_metadata(p).is_ok();
        if da {
            if exists(b) {
                pairs.push(self.single(None, b)?);
            }
            return self.one_side(a, rel, false, pairs);
        }
        if db {
            if exists(a) {
                pairs.push(self.single(Some(a), b)?);
            }
            return self.one_side(b, rel, true, pairs);
        }
        let mut old = file_side(self.hash, a);
        let mut new = file_side(self.hash, b);
        if old.is_none() || new.is_none() {
            old = self.hashed(old)?;
            new = self.hashed(new)?;
        }
        let dev_null = b"/dev/null".to_vec();
        let mut pa = if old.is_some() { path_bytes(a) } else { dev_null.clone() };
        let mut pb = if new.is_some() { path_bytes(b) } else { dev_null };
        // Against `/dev/null` both sides take the name of the other path.
        if a == Path::new("/dev/null") {
            pa = pb.clone();
        } else if b == Path::new("/dev/null") {
            pb = pa.clone();
        }
        let mut p = Pair::new(pb, old, new);
        p.old_path = pa;
        pairs.push(p);
        Ok(())
    }

    /// A file deleted (`old`) or added (`new`) against nothing.
    fn single(&self, old: Option<&Path>, new: &Path) -> Result<Pair, Die> {
        Ok(match old {
            Some(o) => {
                let s = self.hashed(file_side(self.hash, o))?;
                let mut p = Pair::new(b"/dev/null".to_vec(), s, None);
                p.old_path = path_bytes(o);
                p
            }
            None => {
                let s = self.hashed(file_side(self.hash, new))?;
                let mut p = Pair::new(path_bytes(new), None, s);
                p.old_path = b"/dev/null".to_vec();
                p
            }
        })
    }
}
