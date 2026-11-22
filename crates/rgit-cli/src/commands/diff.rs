// SPDX-License-Identifier: MIT
//! `git diff` (Documentation/git-diff.adoc): work tree against index,
//! `--cached`/`--staged [<commit>]`, `<commit>` against the work tree,
//! `<commit> <commit>`, `A..B`, `A...B`, `--merge-base`, `<blob> <blob>`
//! and `--no-index <path> <path>`. No combined diffs.

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
    let mut merge_base = false;
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
            "--merge-base" => {
                merge_base = true;
                Ok(true)
            }
            _ => Ok(false),
        },
        |a| usage_exit(format!("{}{}", msg::invalid_option(a), usage())),
    )?;
    let mut opts = parsed.opts;
    let formats = opts.formats(true)?;

    let repo = Repo::open_gently(ctx)?;
    if repo.is_none() && !no_index {
        let all_exist = parsed.args.len() == 2
            && parsed.args.iter().all(|p| Path::new(p).symlink_metadata().is_ok());
        if !all_exist {
            // The usual error outside a repository.
            Repo::open(ctx, Mode::RequiredQuiet)?;
        }
        no_index = true;
    }
    if no_index {
        return run_no_index(ctx, repo.as_ref(), &opts, &parsed.args);
    }
    let mut repo = repo.expect("repository");

    // Revisions first, then paths.
    let split = parsed.dashdash_at.unwrap_or(parsed.args.len());
    let mut revs: Vec<ObjectId> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut ranges = false;
    let mut paths: Vec<String> = Vec::new();
    for (i, arg) in parsed.args[..split].iter().enumerate() {
        // A range is the only revision argument.
        if ranges && is_range(&mut repo, arg) {
            return Err(usage_exit(usage()));
        }
        if let Some(r) = range(ctx, &mut repo, arg)? {
            if !revs.is_empty() {
                return Err(usage_exit(usage()));
            }
            ranges = true;
            revs.push(r.0);
            revs.push(r.1);
            names.extend(range_names(arg));
            continue;
        }
        match repo.resolve(arg) {
            Some(_) if ranges && paths.is_empty() => return Err(usage_exit(usage())),
            Some(id) if paths.is_empty() => {
                revs.push(id);
                names.push(arg.clone());
            }
            _ => {
                if parsed.dashdash_at.is_none() {
                    let exists = |a: &str| {
                        (repo.setup.work_tree.is_some()
                            && repo.setup.original_cwd.join(a).symlink_metadata().is_ok())
                            || diff::looks_like_pathspec(a)
                    };
                    if !exists(arg) {
                        if let Some(d) = repo.resolve_diagnosis(arg) {
                            return Err(Die::Fatal(d));
                        }
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

    // Blobs: exactly two of them and nothing else.
    let blobs: Vec<usize> = (0..revs.len())
        .filter(|&i| {
            repo.peel_to(revs[i], ObjectKind::Tree).is_none()
                && repo.peel_to(revs[i], ObjectKind::Blob).is_some()
        })
        .collect();
    if blobs.len() > 2 {
        return Err(Die::Fatal(msg::more_than_two_blobs(&names[blobs[2]])));
    }
    if !blobs.is_empty() {
        let blob_and_file = blobs.len() == 1
            && revs.len() == 1
            && paths.len() == 1
            && parsed.dashdash_at.is_none();
        if blob_and_file && !cached && !merge_base {
            let file = repo.setup.original_cwd.join(&paths[0]);
            return diff_blobs(ctx, &mut repo, &mut opts, formats, &names[0], Err((&paths[0], &file)));
        }
        if blobs.len() != 2 || revs.len() != 2 || !paths.is_empty() || cached || merge_base {
            return Err(usage_exit(usage()));
        }
        return diff_blobs(ctx, &mut repo, &mut opts, formats, &names[0], Ok(&names[1]));
    }
    if merge_base {
        if ranges {
            return Err(Die::Fatal(msg::MERGE_BASE_WITH_RANGES.to_owned()));
        }
        match revs.len() {
            0 => return Err(usage_exit(format!("{}{}", msg::invalid_option("--merge-base"), usage()))),
            1 => {
                let head = repo
                    .resolve("HEAD")
                    .ok_or_else(|| Die::Fatal(msg::NO_MERGE_BASE_FOUND.to_owned()))?;
                revs[0] = diff::single_merge_base(ctx, &mut repo, revs[0], head)?;
            }
            2 => revs[0] = diff::single_merge_base(ctx, &mut repo, revs[0], revs[1])?,
            _ => return Err(usage_exit(usage())),
        }
    }

    let pathspec = diff::pathspec(ctx, &repo, &paths)?;
    let mut trees = Vec::new();
    for id in &revs {
        let t = repo
            .peel_to(*id, ObjectKind::Tree)
            .ok_or_else(|| usage_exit(usage()))?;
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
                porcelain: true,
                hash: repo.hash,
                submodules: submodules.as_ref(),
                dirty: Default::default(),
            };
            (collect::files(&index, &pathspec, &wt, false)?, Some(("i", "w")))
        }
        (0 | 1, true) | (1, false) => {
            let tree = match trees.first() {
                Some(t) => *t,
                None => head,
            };
            let (index, mtime) = repo.read_index()?;
            if cached {
                let pairs = collect::index(&repo, &tree, &index, &pathspec, None, true)?;
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
                    porcelain: true,
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
    if !cached && trees.is_empty() {
        auto_refresh_index(&repo);
    }
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

/// `diff.autoRefreshIndex` (on by default): before comparing the index
/// with the work tree, the cached stat data of files whose contents did not change is
/// updated and the index written back, if it can be locked.
fn auto_refresh_index(repo: &Repo) {
    use rgit_worktree::stat::Status;
    let on = repo
        .config
        .bool("diff.autorefreshindex")
        .ok()
        .flatten()
        .unwrap_or(true);
    if !on || repo.setup.work_tree.is_none() {
        return;
    }
    let index_path = repo.setup.cwd.join(repo.index_path());
    let Ok(lock) = rgit_fs::LockFile::acquire(&index_path, &repo.index_lock_options()) else {
        return;
    };
    let Ok((mut index, mtime)) = repo.read_index() else {
        return;
    };
    let Ok(checker) = repo.checker(mtime) else {
        return;
    };
    let mut changed = false;
    for pos in 0..index.len() {
        let e = &index.entries()[pos];
        if e.stage().is_conflict()
            || e.flags.skip_worktree
            || e.flags.assume_valid
            || e.flags.intent_to_add
            || e.mode.is_gitlink()
        {
            continue;
        }
        match checker.check(e) {
            Ok(Status::StatOnly) => {
                if let (Ok(Some(meta)), Some(entry)) = (checker.lstat(e.path()), index.entry_mut(pos)) {
                    checker.refresh(entry, &meta);
                    changed = true;
                }
            }
            Ok(Status::Unchanged) if checker.is_racy(e) => changed = true,
            _ => {}
        }
    }
    if !changed {
        return;
    }
    if let Some(reference) = mtime {
        let _ = rgit_worktree::stat::smudge_racy_entries(&mut index, reference, &checker);
    }
    let _ = crate::switch_branch::write_index(repo, &index, lock);
}

/// Whether `arg` is `A..B` or `A...B` with two existing ends.
fn is_range(repo: &mut Repo, arg: &str) -> bool {
    let Some((a, b)) = arg.split_once("..") else {
        return false;
    };
    let b = b.strip_prefix('.').unwrap_or(b);
    if a.is_empty() && b.is_empty() {
        return false;
    }
    let a = if a.is_empty() { "HEAD" } else { a };
    let b = if b.is_empty() { "HEAD" } else { b };
    repo.resolve(a).is_some() && repo.resolve(b).is_some()
}

/// The two ends of `A..B` (an empty end is `HEAD`).
fn range_names(arg: &str) -> [String; 2] {
    let (a, b) = arg.split_once("..").unwrap_or((arg, ""));
    let b = b.strip_prefix('.').unwrap_or(b);
    let end = |s: &str| if s.is_empty() { "HEAD".to_owned() } else { s.to_owned() };
    [end(a), end(b)]
}

/// `<blob> <blob>` and `<blob> <file>`: the blobs under the paths they
/// were named by (`<rev>:<path>`, `:<path>`), else under their names as
/// given; a file under its name, which a blob without a path takes too.
fn diff_blobs(
    ctx: &Context,
    repo: &mut Repo,
    opts: &mut DiffOpts,
    formats: crate::diff::options::Formats,
    old_name: &str,
    new: Result<&str, (&String, &PathBuf)>,
) -> CmdResult {
    let blob = |repo: &mut Repo, name: &str| -> Result<(Option<String>, Side), Die> {
        let (id, mode, path) = repo
            .resolve_entry(name)
            .ok_or_else(|| Die::Fatal(messages::ambiguous_argument(name)))?;
        let id = repo.peel_to(id, ObjectKind::Blob).unwrap_or(id);
        let side = Side {
            mode: mode.unwrap_or(0o100644),
            oid: id,
            file: None,
        };
        Ok((path, side))
    };
    let (old_path, old) = blob(repo, old_name)?;
    let (old_path, new_path, new) = match new {
        Ok(name) => {
            let (path, side) = blob(repo, name)?;
            (
                old_path.unwrap_or_else(|| old_name.to_owned()),
                path.unwrap_or_else(|| name.to_owned()),
                side,
            )
        }
        Err((name, file)) => {
            let side = file_side(repo.hash, file)
                .ok_or_else(|| Die::Fatal(messages::ambiguous_argument(name)))?;
            (old_path.unwrap_or_else(|| name.clone()), name.clone(), side)
        }
    };
    let mut pair = Pair::new(new_path.into_bytes(), Some(old), Some(new));
    pair.old_path = old_path.into_bytes();
    opts.resolve_objects(repo)?;
    let repo: &Repo = repo;
    page(ctx, Some(repo), opts)?;
    let mut out = Output::new(Some(repo), repo.hash, opts, formats, true)?;
    out.set_color(opts)?;
    let (bytes, changed) = out.render(vec![pair])?;
    Ok(out.finish(&bytes, changed, opts))
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

/// `--no-index`: two files or directories outside of git's control.
fn run_no_index(ctx: &Context, repo: Option<&Repo>, opts: &DiffOpts, args: &[String]) -> CmdResult {
    if args.len() != 2 {
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
    let meta = |p: &Path| {
        std::fs::symlink_metadata(p)
            .map_err(|_| Die::Error(msg::could_not_access(&p.to_string_lossy()), 1))
    };
    let (ma, mb) = (meta(&a)?, meta(&b)?);
    let mut pairs = Vec::new();
    let (mut a, mut b) = (a, b);
    // A file against a directory compares with the file of the same name in it.
    if ma.is_dir() != mb.is_dir() {
        if ma.is_dir() {
            a = a.join(b.file_name().unwrap_or_default());
            meta(&a)?;
        } else {
            b = b.join(a.file_name().unwrap_or_default());
            meta(&b)?;
        }
    }
    no_index_walk(hash, &a, &b, &mut pairs)?;
    page(ctx, repo, opts)?;
    let mut out = Output::new(repo, hash, opts, formats, true)?;
    out.convert = false;
    out.set_color(opts)?;
    if repo.is_some() {
        out.mnemonic(opts, "1", "2");
    }
    let (bytes, changed) = out.render(pairs)?;
    let code = out.finish(&bytes, changed, opts);
    Ok(i32::from(changed) | code)
}

fn path_bytes(p: &Path) -> Vec<u8> {
    p.as_os_str().as_encoded_bytes().to_vec()
}

fn file_side(hash: HashKind, p: &Path) -> Option<Side> {
    let m = std::fs::symlink_metadata(p).ok()?;
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

fn no_index_walk(hash: HashKind, a: &Path, b: &Path, pairs: &mut Vec<Pair>) -> Result<(), Die> {
    let is_dir = |p: &Path| std::fs::metadata(p).is_ok_and(|m| m.is_dir());
    let (da, db) = (is_dir(a), is_dir(b));
    if da || db {
        let names = |p: &Path, d: bool| -> Vec<std::ffi::OsString> {
            if !d {
                return Vec::new();
            }
            let mut v: Vec<_> = std::fs::read_dir(p)
                .map(|rd| rd.flatten().map(|e| e.file_name()).collect())
                .unwrap_or_default();
            v.sort();
            v
        };
        let mut all = names(a, da);
        all.extend(names(b, db));
        all.sort();
        all.dedup();
        for n in all {
            no_index_walk(hash, &a.join(&n), &b.join(&n), pairs)?;
        }
        return Ok(());
    }
    let mut old = file_side(hash, a);
    let mut new = file_side(hash, b);
    // A file on one side only is hashed (git looks for renames then).
    if old.is_none() || new.is_none() {
        for s in old.iter_mut().chain(new.iter_mut()) {
            let data = if s.mode == 0o120000 {
                std::fs::read_link(s.file.as_ref().expect("file"))
                    .map(|t| t.into_os_string().into_encoded_bytes())
            } else {
                std::fs::read(s.file.as_ref().expect("file"))
            };
            if let Ok(d) = data {
                s.oid = rgit_hash::hash_object(hash, "blob", &d).map_err(|e| Die::Fatal(e.to_string()))?;
            }
        }
    }
    let dev_null = b"/dev/null".to_vec();
    let pa = if old.is_some() { path_bytes(a) } else { dev_null.clone() };
    let pb = if new.is_some() { path_bytes(b) } else { dev_null };
    let mut p = Pair::new(pb, old, new);
    p.old_path = pa;
    pairs.push(p);
    Ok(())
}
