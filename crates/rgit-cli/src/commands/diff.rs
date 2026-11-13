// SPDX-License-Identifier: MIT
//! `git diff` (Documentation/git-diff.adoc): work tree against index,
//! `--cached`/`--staged [<commit>]`, `<commit>` against the work tree,
//! `<commit> <commit>`, `A..B`, `A...B` and `--no-index <path> <path>`.
//! No rename detection, no blob or combined diffs.

use std::path::{Path, PathBuf};

use rgit_hash::{HashKind, ObjectId};
use rgit_object::ObjectKind;

use crate::diff::collect::{self, Pair, Side, TreeDiff, WorkTree};
use crate::diff::options::{DiffOpts, usage_exit};
use crate::diff::output::{Output, finish};
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
    let opts = parsed.opts;
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
    let repo = repo;
    let (pairs, mnemonic) = match (trees.len(), cached) {
        (0, false) => {
            let root = repo.work_tree()?;
            let (index, mtime) = repo.read_index()?;
            let checker = repo.checker(mtime)?;
            let wt = WorkTree {
                root: &root,
                checker: &checker,
                porcelain: true,
                hash: repo.hash,
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
                (
                    collect::index(&repo, &tree, &index, &pathspec, None, true)?,
                    Some(("c", "i")),
                )
            } else {
                let root = repo.work_tree()?;
                let checker = repo.checker(mtime)?;
                let wt = WorkTree {
                    root: &root,
                    checker: &checker,
                    porcelain: true,
                    hash: repo.hash,
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
            (w.pairs, None)
        }
        _ => return Err(Die::Fatal(msg::not_supported("this form of diff"))),
    };
    page(ctx, Some(&repo), &opts)?;
    let mut out = Output::new(Some(&repo), repo.hash, &opts, formats, true);
    out.set_color(&opts)?;
    out.combined_unmerged = trees.is_empty() && !cached;
    if let Some((s, d)) = mnemonic {
        out.mnemonic(&opts, s, d);
    }
    let (bytes, changed) = out.render(pairs)?;
    Ok(finish(&bytes, changed, &opts))
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
    let mut out = Output::new(repo, hash, opts, formats, true);
    out.set_color(opts)?;
    if repo.is_some() {
        out.mnemonic(opts, "1", "2");
    }
    let (bytes, changed) = out.render(pairs)?;
    finish(&bytes, changed, opts);
    Ok(i32::from(changed))
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

