// SPDX-License-Identifier: MIT
//! `git sparse-checkout` (Documentation/git-sparse-checkout.adoc,
//! config/core.adoc `core.sparseCheckout*`, config/sparse.adoc): the
//! subcommands `init`, `list`, `set`, `add`, `reapply`, `disable`,
//! `check-rules` and `clean`, in cone and non-cone mode.
//!
//! The settings go to the worktree's `config.worktree`;
//! `extensions.worktreeConfig` is turned on first. The patterns are
//! written to `$GIT_DIR/info/sparse-checkout` of the worktree through a
//! lock file, after the work tree was brought in line with them
//! ([`rgit_unpack::reapply_sparse`]): paths leaving the checkout are
//! removed when up to date, paths entering it are written, everything
//! else is reported as a warning. In cone mode, directories outside the
//! cone that only hold ignored files are removed as well.
//!
//! `--sparse-index` only sets `index.sparse`; rgit always writes a full
//! index (the sparse index is #119).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use rgit_config::{SetRequest, ValuePattern};
use rgit_fs::{LockFile, LockOptions};
use rgit_index::{Index, Stage};
use rgit_unpack::Worktree;
use rgit_worktree::ignore::Excludes;
use rgit_worktree::sparse::{self, DirMatch, Sparse};
use rgit_worktree::stat::smudge_racy_entries;

use crate::messages::sparse_checkout as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;
use crate::sparse as shared;

const SUBCOMMANDS: &[&str] = &[
    "init",
    "list",
    "set",
    "add",
    "reapply",
    "disable",
    "check-rules",
    "clean",
];

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mut parser = Parser::new(
        msg::USAGE,
        SUBCOMMANDS.iter().map(|s| Opt::subcommand(s)).collect(),
    );
    let parsed = parser.parse(argv)?;
    drop(parser);
    let args = parsed.args;
    match parsed.subcommand.as_deref() {
        Some("init") => init(ctx, &args),
        Some("list") => list(ctx, &args),
        Some("set") => set_or_add(ctx, &args, false),
        Some("add") => set_or_add(ctx, &args, true),
        Some("reapply") => reapply(ctx, &args),
        Some("disable") => disable(ctx, &args),
        Some("check-rules") => check_rules(ctx, &args),
        Some("clean") => clean(ctx, &args),
        _ => Err(Die::Output(Parser::new(msg::USAGE, Vec::new()).usage_error())),
    }
}

/// The repository with a work tree (`this operation must be run in a work
/// tree` otherwise); outside a repository the worktree counts as not
/// sparse and `not_sparse` is the error.
fn open_worktree(ctx: &Context, not_sparse: &str) -> Result<Repo, Die> {
    match Repo::open_gently(ctx)? {
        Some(repo) => {
            repo.work_tree()?;
            Ok(repo)
        }
        None => Err(Die::Fatal(not_sparse.to_owned())),
    }
}

fn root(repo: &Repo) -> Result<PathBuf, Die> {
    Ok(repo.setup.cwd.join(repo.work_tree()?))
}

/// Reads the sparse-checkout file; `None` when it does not exist.
fn read_file(repo: &Repo) -> Option<Vec<u8>> {
    std::fs::read(shared::file_path(repo)).ok()
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// `path` quoted like `ls-tree` (with `core.quotePath`).
fn quoted(repo: &Repo, path: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    crate::quote::write_quoted(&mut out, path, repo.quote_path());
    out
}

// ----- configuration -------------------------------------------------

/// The configuration file of the current worktree, after turning on
/// `extensions.worktreeConfig` when needed.
fn worktree_config(repo: &Repo) -> Result<PathBuf, Die> {
    let common = repo.setup.cwd.join(&repo.setup.common_dir).join("config");
    let on = repo
        .config
        .bool("extensions.worktreeconfig")
        .ok()
        .flatten()
        .unwrap_or(false);
    if !on {
        config_set(&common, "extensions.worktreeConfig", Some("true"))?;
    }
    Ok(repo.setup.cwd.join(&repo.setup.git_dir).join("config.worktree"))
}

fn config_set(path: &Path, key: &str, value: Option<&str>) -> Result<(), Die> {
    let req = SetRequest {
        key: key.as_bytes(),
        value: value.map(str::as_bytes),
        pattern: &ValuePattern::Any,
        all: false,
        comment: None,
    };
    match rgit_config::write::set_in_file(path, &LockOptions::default(), &req) {
        Ok(()) => Ok(()),
        Err(e) if matches!(e.edit_error(), Some(rgit_config::EditError::NothingToUnset)) => Ok(()),
        Err(e) => Err(Die::Fatal(e.to_string())),
    }
}

/// Writes `core.sparseCheckout`, `core.sparseCheckoutCone` and (when given)
/// `index.sparse` to the worktree configuration.
fn write_config(repo: &Repo, enabled: bool, cone: bool, sparse_index: Option<bool>) -> Result<(), Die> {
    let path = worktree_config(repo)?;
    let b = |v: bool| if v { "true" } else { "false" };
    config_set(&path, "core.sparseCheckout", Some(b(enabled)))?;
    config_set(&path, "core.sparseCheckoutCone", Some(b(cone)))?;
    if let Some(si) = sparse_index {
        config_set(&path, "index.sparse", Some(b(si)))?;
    }
    Ok(())
}

/// The `--[no-]sparse-index` value (`-1` when not given).
fn tri(v: i32) -> Option<bool> {
    (v >= 0).then_some(v > 0)
}

// ----- work tree update ------------------------------------------------

/// Brings the index and work tree in line with `sparse`. Nothing happens
/// without an index file (an unborn checkout). Returns the exit code.
fn update_worktree(repo: &Repo, sparse: &Sparse) -> Result<(), Die> {
    let index_path = repo.setup.cwd.join(repo.index_path());
    if !index_path.exists() {
        return Ok(());
    }
    let root = root(repo)?;
    let lock = LockFile::acquire(&index_path, &LockOptions::default())
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let (mut index, mtime) = repo.read_index()?;
    // The settings may have just been turned on.
    if !repo
        .config
        .bool("sparse.expectfilesoutsideofpatterns")
        .ok()
        .flatten()
        .unwrap_or(false)
    {
        shared::clear_present_files(&root, &mut index);
    }
    let checker = repo.checker(mtime)?;
    let wt = Worktree {
        root: &root,
        checker: &checker,
    };
    let plan = rgit_unpack::reapply_sparse(repo.odb(), &index, &wt, sparse)
        .map_err(crate::commands::read_tree::unpack_failed)?;
    let conv = crate::convert::Converter::checkout(repo, plan.index())?;
    let applied = plan.apply(repo.odb(), &wt, &conv);
    shared::print_warnings(&applied.sparse_warnings, true);
    let mut index = applied.index;
    if let Some(reference) = mtime {
        let _ = smudge_racy_entries(&mut index, reference, &checker);
    }
    crate::switch_branch::write_index(repo, &index, lock)?;
    for e in &applied.errors {
        eprintln!("error: {e}");
    }
    if sparse.is_cone() {
        remove_ignored_dirs(repo, &root, &index, sparse);
    }
    Ok(())
}

/// The topmost directories outside the cone that hold index entries, all
/// of them skip-worktree (and none conflicted), in index order.
fn sparse_dirs(index: &Index, sparse: &Sparse) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut blocked: Vec<Vec<u8>> = Vec::new();
    for e in index.entries() {
        let path = e.path();
        let mut dir = None;
        for (i, &b) in path.iter().enumerate() {
            if b == b'/' && sparse.dir_match(&path[..i]) == Some(DirMatch::Excluded) {
                dir = Some(&path[..i]);
                break;
            }
        }
        let Some(dir) = dir else {
            continue;
        };
        let visible = !e.flags.skip_worktree || e.stage() != Stage::MERGED;
        if visible {
            if !blocked.iter().any(|b| b == dir) {
                blocked.push(dir.to_vec());
            }
        } else if out.last().map(Vec::as_slice) != Some(dir) {
            out.push(dir.to_vec());
        }
    }
    out.dedup();
    out.retain(|d| !blocked.contains(d));
    out
}

fn excludes(repo: &Repo, root: &Path) -> Excludes {
    crate::commands::add::build_excludes(repo, root)
}

/// Whether `dir` (relative to `root`) holds a file that is not ignored.
fn has_untracked(root: &Path, dir: &[u8], ex: &mut Excludes) -> bool {
    let Ok(rd) = std::fs::read_dir(os_path(root, dir)) else {
        return false;
    };
    let mut names: Vec<(Vec<u8>, bool)> = rd
        .flatten()
        .map(|e| {
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            (e.file_name().as_encoded_bytes().to_vec(), is_dir)
        })
        .collect();
    names.sort();
    for (name, is_dir) in names {
        let mut rel = dir.to_vec();
        rel.push(b'/');
        rel.extend_from_slice(&name);
        if ex.match_with_leading_dirs(&rel, is_dir).is_some_and(|m| m.is_excluded()) {
            continue;
        }
        if !is_dir || has_untracked(root, &rel, ex) {
            return true;
        }
    }
    false
}

/// Cone mode: removes the directories outside the cone that only hold
/// ignored files, and warns about those with other untracked files.
fn remove_ignored_dirs(repo: &Repo, root: &Path, index: &Index, sparse: &Sparse) {
    let mut ex = excludes(repo, root);
    for dir in sparse_dirs(index, sparse) {
        let full = os_path(root, &dir);
        if !std::fs::symlink_metadata(&full).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        if has_untracked(root, &dir, &mut ex) {
            let mut shown = dir.clone();
            shown.push(b'/');
            eprintln!("warning: {}", msg::untracked_outside_cone(&lossy(&shown)));
            continue;
        }
        let _ = std::fs::remove_dir_all(&full);
        rgit_unpack::remove_empty_parents(root, &dir);
    }
}

fn os_path(root: &Path, rel: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        root.join(std::ffi::OsStr::from_bytes(rel))
    }
    #[cfg(not(unix))]
    {
        root.join(String::from_utf8_lossy(rel).as_ref())
    }
}

/// Takes the lock on the sparse-checkout file (creating `info/`).
fn lock_file(repo: &Repo) -> Result<LockFile, Die> {
    let path = shared::file_path(repo);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    LockFile::acquire(&path, &LockOptions::default()).map_err(|e| Die::Fatal(e.to_string()))
}

/// Updates the work tree for `data` and writes it as the new
/// sparse-checkout file.
fn write_patterns(repo: &Repo, data: &[u8], cone: bool) -> Result<(), Die> {
    let mut lock = lock_file(repo)?;
    let sparse = shared::parse(data, cone, shared::ignore_case(&repo.config));
    update_worktree(repo, &sparse)?;
    lock.write_all(data).map_err(|e| Die::Fatal(e.to_string()))?;
    lock.commit().map_err(|e| Die::Fatal(e.to_string()))?;
    Ok(())
}

/// `clone --sparse`: turns on a cone-mode sparse checkout of the top
/// level files before the initial checkout and returns its patterns.
pub fn init_for_clone(repo: &Repo) -> Result<Sparse, Die> {
    write_config(repo, true, true, None)?;
    let data = b"/*\n!/*/\n";
    let mut lock = lock_file(repo)?;
    lock.write_all(data).map_err(|e| Die::Fatal(e.to_string()))?;
    lock.commit().map_err(|e| Die::Fatal(e.to_string()))?;
    Ok(shared::parse(data, true, shared::ignore_case(&repo.config)))
}

// ----- subcommands -------------------------------------------------------

fn init(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let cone = Cell::new(-1);
    let sparse_index = Cell::new(-1);
    let mut parser = Parser::new(
        msg::USAGE_INIT,
        vec![
            Opt::bool(None, "cone", &cone, msg::HELP_CONE),
            Opt::bool(None, "sparse-index", &sparse_index, msg::HELP_SPARSE_INDEX),
        ],
    );
    let args = parser.parse(argv)?.args;
    if !args.is_empty() {
        return Err(Die::Output(parser.usage_error()));
    }
    drop(parser);
    let repo = Repo::open(ctx, Mode::Required)?;
    repo.work_tree()?;
    let cone = decide_cone(&repo, cone.get());
    write_config(&repo, true, cone, tri(sparse_index.get()))?;
    let data = match read_file(&repo) {
        Some(d) => d,
        None => b"/*\n!/*/\n".to_vec(),
    };
    write_patterns(&repo, &data, cone)?;
    Ok(0)
}

/// The mode of `set`/`init`/`reapply`: the flag, else the current setting
/// while sparse, else cone mode.
fn decide_cone(repo: &Repo, flag: i32) -> bool {
    match flag {
        0 => false,
        1 => true,
        _ => {
            if shared::enabled(&repo.config) {
                shared::cone_config(&repo.config).unwrap_or(true)
            } else {
                true
            }
        }
    }
}

fn list(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let repo = open_worktree(ctx, msg::NOT_SPARSE)?;
    if !shared::enabled(&repo.config) {
        return Err(Die::Fatal(msg::NOT_SPARSE.into()));
    }
    let mut parser = Parser::new(msg::USAGE_LIST, Vec::new());
    let args = parser.parse(argv)?.args;
    if !args.is_empty() {
        return Err(Die::Output(parser.usage_error()));
    }
    let Some(data) = read_file(&repo) else {
        eprintln!("warning: {}", msg::NOT_SPARSE_NO_FILE);
        return Ok(0);
    };
    let cone = shared::cone_config(&repo.config).unwrap_or(false);
    let sparse = shared::parse(&data, cone, false);
    let mut out = Vec::new();
    match sparse.cone_dirs() {
        Some(dirs) => {
            for d in dirs {
                out.extend_from_slice(&quoted(&repo, &d));
                out.push(b'\n');
            }
        }
        None => {
            // Every pattern line as written (no comments or blank lines,
            // trailing spaces dropped).
            for line in data.split(|&b| b == b'\n') {
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                let mut end = line.len();
                while end > 0 && line[end - 1] == b' ' && !(end > 1 && line[end - 2] == b'\\') {
                    end -= 1;
                }
                let line = &line[..end];
                if line.is_empty() || line[0] == b'#' {
                    continue;
                }
                out.extend_from_slice(line);
                out.push(b'\n');
            }
        }
    }
    let _ = std::io::stdout().write_all(&out);
    Ok(0)
}

/// Reads newline-separated arguments from stdin (C-style quoted ones
/// unquoted when `unquote`).
fn stdin_lines(unquote: bool) -> Vec<Vec<u8>> {
    let mut data = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut data);
    let mut out = Vec::new();
    for line in data.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        if unquote && line.first() == Some(&b'"') {
            if let Some(u) = crate::quote::c_unquote(line) {
                out.push(u);
                continue;
            }
        }
        out.push(line.to_vec());
    }
    out
}

/// A cone-mode argument as a directory relative to the top of the work
/// tree (`prefix` applied, `.`/`..` resolved, slashes trimmed).
fn cone_dir(prefix: &[u8], arg: &[u8]) -> Vec<u8> {
    let arg = if arg.first() == Some(&b'"') {
        crate::quote::c_unquote(arg).unwrap_or_else(|| arg.to_vec())
    } else {
        arg.to_vec()
    };
    let mut full = Vec::new();
    if arg.first() != Some(&b'/') {
        full.extend_from_slice(prefix);
    }
    full.extend_from_slice(&arg);
    let mut parts: Vec<&[u8]> = Vec::new();
    for c in full.split(|&b| b == b'/') {
        match c {
            b"" | b"." => {}
            b".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    parts.join(&b'/')
}

/// With `core.ignoreCase`, the spelling of `dir` the index uses.
fn index_case(index: &Index, dir: &[u8]) -> Vec<u8> {
    let mut below = dir.to_ascii_lowercase();
    below.push(b'/');
    for e in index.entries() {
        let p = e.path();
        if p.len() > below.len() && p[..below.len()].eq_ignore_ascii_case(&below) {
            return p[..dir.len()].to_vec();
        }
    }
    dir.to_vec()
}

fn set_or_add(ctx: &mut Context, argv: &[String], add: bool) -> CmdResult {
    let cone = Cell::new(-1);
    let sparse_index = Cell::new(-1);
    let skip_checks = Cell::new(false);
    let use_stdin = Cell::new(false);
    let repo_for_add = if add {
        let repo = open_worktree(ctx, msg::NO_SPARSE_TO_ADD)?;
        if !shared::enabled(&repo.config) {
            return Err(Die::Fatal(msg::NO_SPARSE_TO_ADD.into()));
        }
        Some(repo)
    } else {
        None
    };
    let mut opts = Vec::new();
    if !add {
        opts.push(Opt::bool(None, "cone", &cone, msg::HELP_CONE));
        opts.push(Opt::bool(None, "sparse-index", &sparse_index, msg::HELP_SPARSE_INDEX));
    }
    opts.push(Opt::flag(None, "skip-checks", &skip_checks, msg::HELP_SKIP_CHECKS).noneg());
    if add {
        opts.push(Opt::flag(None, "stdin", &use_stdin, msg::HELP_STDIN));
    } else {
        opts.push(Opt::flag(None, "stdin", &use_stdin, msg::HELP_STDIN).noneg());
    }
    let mut parser = Parser::new(if add { msg::USAGE_ADD } else { msg::USAGE_SET }, opts);
    let args = parser.parse(argv)?.args;
    drop(parser);
    let repo = match repo_for_add {
        Some(r) => r,
        None => {
            let r = Repo::open(ctx, Mode::Required)?;
            r.work_tree()?;
            r
        }
    };
    let cone = if add {
        shared::cone_config(&repo.config).unwrap_or(true)
    } else {
        decide_cone(&repo, cone.get())
    };
    let icase = shared::ignore_case(&repo.config);
    let raw: Vec<Vec<u8>> = if use_stdin.get() {
        stdin_lines(cone)
    } else {
        args.iter().map(|a| a.as_bytes().to_vec()).collect()
    };
    let prefix = repo.prefix();
    let (index, _) = repo.read_index()?;
    let data = if cone {
        let mut dirs: Vec<Vec<u8>> = Vec::new();
        for a in &raw {
            let mut d = cone_dir(&prefix, a);
            if !skip_checks.get() {
                if index.get(&d, Stage::MERGED).is_some() || !index.entries_for_path(&d).is_empty() {
                    return Err(Die::Fatal(msg::not_a_directory(&lossy(a))));
                }
                if icase {
                    d = index_case(&index, &d);
                }
            }
            dirs.push(d);
        }
        if add {
            let existing = read_file(&repo).unwrap_or_default();
            let (current, warnings) = Sparse::parse(&existing, true, false);
            for w in &warnings {
                eprintln!("warning: {w}");
            }
            match current.cone_dirs() {
                Some(d) if warnings.is_empty() => dirs.extend(d),
                _ => return Err(Die::Fatal(msg::NOT_CONE_PATTERNS.into())),
            }
        }
        sparse::cone_file(&dirs)
    } else {
        if !prefix.is_empty() {
            return Err(Die::Fatal(msg::TOPLEVEL_IN_NON_CONE.into()));
        }
        let mut data = if add {
            let mut d = read_file(&repo).unwrap_or_default();
            if !d.is_empty() && !d.ends_with(b"\n") {
                d.push(b'\n');
            }
            d
        } else if raw.is_empty() {
            b"/*\n!/*/\n".to_vec()
        } else {
            Vec::new()
        };
        for a in &raw {
            if !skip_checks.get()
                && !a.starts_with(b"/")
                && !a.starts_with(b"!")
                && !index.entries_for_path(a).is_empty()
            {
                eprintln!("warning: {}", msg::pass_leading_slash(&lossy(a)));
            }
            data.extend_from_slice(a);
            data.push(b'\n');
        }
        data
    };
    drop(index);
    if !add {
        write_config(&repo, true, cone, tri(sparse_index.get()))?;
    }
    write_patterns(&repo, &data, cone)?;
    Ok(0)
}

fn reapply(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let repo = open_worktree(ctx, msg::MUST_BE_SPARSE_REAPPLY)?;
    if !shared::enabled(&repo.config) {
        return Err(Die::Fatal(msg::MUST_BE_SPARSE_REAPPLY.into()));
    }
    let cone = Cell::new(-1);
    let sparse_index = Cell::new(-1);
    let mut parser = Parser::new(
        msg::USAGE_REAPPLY,
        vec![
            Opt::bool(None, "cone", &cone, msg::HELP_CONE),
            Opt::bool(None, "sparse-index", &sparse_index, msg::HELP_SPARSE_INDEX),
        ],
    );
    let args = parser.parse(argv)?.args;
    if !args.is_empty() {
        return Err(Die::Output(parser.usage_error()));
    }
    drop(parser);
    let cone_mode = decide_cone(&repo, cone.get());
    if cone.get() >= 0 || sparse_index.get() >= 0 {
        write_config(&repo, true, cone_mode, tri(sparse_index.get()))?;
    }
    let data = read_file(&repo).unwrap_or_default();
    let sparse = shared::parse(&data, cone_mode, shared::ignore_case(&repo.config));
    update_worktree(&repo, &sparse)?;
    Ok(0)
}

fn disable(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mut parser = Parser::new(msg::USAGE_DISABLE, Vec::new());
    let args = parser.parse(argv)?.args;
    if !args.is_empty() {
        return Err(Die::Output(parser.usage_error()));
    }
    drop(parser);
    let repo = Repo::open(ctx, Mode::Required)?;
    repo.work_tree()?;
    let (all, _) = Sparse::parse(b"/*\n", false, false);
    let _lock = lock_file(&repo)?;
    update_worktree(&repo, &all)?;
    write_config(&repo, false, false, Some(false))?;
    Ok(0)
}

fn check_rules(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let nul = Cell::new(false);
    let skip_checks = Cell::new(false);
    let cone = Cell::new(-1);
    let rules_file = RefCell::new(None::<String>);
    let mut parser = Parser::new(
        msg::USAGE_CHECK_RULES,
        vec![
            Opt::flag('z', None, &nul, msg::HELP_Z),
            Opt::flag(None, "skip-checks", &skip_checks, msg::HELP_SKIP_CHECKS).hidden(),
            Opt::bool(None, "cone", &cone, msg::HELP_RULES_CONE),
            Opt::string(None, "rules-file", &rules_file, msg::ARGH_FILE, msg::HELP_RULES_FILE),
        ],
    );
    let args = parser.parse(argv)?.args;
    if !args.is_empty() {
        return Err(Die::Output(parser.usage_error()));
    }
    drop(parser);
    let repo = Repo::open(ctx, Mode::Required)?;
    let icase = shared::ignore_case(&repo.config);
    let sparse = match rules_file.into_inner() {
        Some(f) => {
            let path = repo.setup.original_cwd.join(&f);
            let data = std::fs::read(&path)
                .map_err(|_| Die::Fatal(msg::unable_to_load(&path.display().to_string())))?;
            if cone.get() == 0 {
                shared::parse(&data, false, icase)
            } else {
                let dirs: Vec<Vec<u8>> = data
                    .split(|&b| b == b'\n')
                    .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
                    .filter(|l| !l.is_empty())
                    .map(|l| cone_dir(b"", l))
                    .collect();
                Sparse::from_cone_dirs(&dirs, icase)
            }
        }
        None => {
            let data = read_file(&repo).unwrap_or_default();
            shared::parse(&data, decide_cone(&repo, cone.get()), icase)
        }
    };
    let mut input = Vec::new();
    let _ = std::io::stdin().lock().read_to_end(&mut input);
    let sep = if nul.get() { b'\0' } else { b'\n' };
    let mut out = Vec::new();
    let mut items: Vec<&[u8]> = input.split(|&b| b == sep).collect();
    if items.last().is_some_and(|l| l.is_empty()) {
        items.pop();
    }
    for item in items {
        let path = if !nul.get() && item.first() == Some(&b'"') {
            crate::quote::c_unquote(item).unwrap_or_else(|| item.to_vec())
        } else {
            item.to_vec()
        };
        if sparse.includes(&path) {
            if nul.get() {
                out.extend_from_slice(&path);
                out.push(0);
            } else {
                out.extend_from_slice(&quoted(&repo, &path));
                out.push(b'\n');
            }
        }
    }
    let _ = std::io::stdout().write_all(&out);
    Ok(0)
}

fn clean(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let repo = open_worktree(ctx, msg::MUST_BE_SPARSE_CLEAN)?;
    if !shared::enabled(&repo.config) {
        return Err(Die::Fatal(msg::MUST_BE_SPARSE_CLEAN.into()));
    }
    let dry_run = Cell::new(false);
    let force = Cell::new(false);
    let verbose = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE_CLEAN,
        vec![
            Opt::flag('n', "dry-run", &dry_run, msg::HELP_DRY_RUN),
            Opt::flag('f', "force", &force, msg::HELP_FORCE),
            Opt::flag('v', "verbose", &verbose, msg::HELP_VERBOSE),
        ],
    );
    let args = parser.parse(argv)?.args;
    if !args.is_empty() {
        return Err(Die::Output(parser.usage_error()));
    }
    drop(parser);
    if !shared::cone_config(&repo.config).unwrap_or(false) {
        return Err(Die::Fatal(msg::MUST_BE_CONE_CLEAN.into()));
    }
    let require_force = repo
        .config
        .bool("clean.requireforce")
        .ok()
        .flatten()
        .unwrap_or(true);
    if require_force && !force.get() && !dry_run.get() {
        return Err(Die::Fatal(msg::REFUSING_TO_CLEAN.into()));
    }
    let data = read_file(&repo).unwrap_or_default();
    let sparse = shared::parse(&data, true, shared::ignore_case(&repo.config));
    if !sparse.is_cone() {
        return Err(Die::Fatal(msg::MUST_BE_CONE_CLEAN.into()));
    }
    let (index, _) = repo.read_index()?;
    if index.has_conflicts() {
        return Err(Die::Fatal(msg::SPARSE_INDEX_CONFLICTS.into()));
    }
    let root = root(&repo)?;
    let prefix = repo.prefix();
    let mut out = String::new();
    for dir in sparse_dirs(&index, &sparse) {
        let full = os_path(&root, &dir);
        if !std::fs::symlink_metadata(&full).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        let mut shown_dir = dir.clone();
        shown_dir.push(b'/');
        let shown = |p: &[u8]| lossy(&crate::quote::relative_to(p, &prefix));
        if verbose.get() {
            let mut files = Vec::new();
            list_files(&root, &dir, &mut files);
            for f in files {
                let line = if dry_run.get() {
                    msg::would_remove(&shown(&f))
                } else {
                    msg::removing(&shown(&f))
                };
                out.push_str(&line);
                out.push('\n');
            }
        } else {
            let line = if dry_run.get() {
                msg::would_remove(&shown(&shown_dir))
            } else {
                msg::removing(&shown(&shown_dir))
            };
            out.push_str(&line);
            out.push('\n');
        }
        if !dry_run.get() && std::fs::remove_dir_all(&full).is_err() {
            eprintln!("warning: {}", msg::failed_to_remove(&lossy(&dir)));
        }
    }
    print!("{out}");
    Ok(0)
}

/// Every file below `dir`, sorted, depth first.
fn list_files(root: &Path, dir: &[u8], out: &mut Vec<Vec<u8>>) {
    let Ok(rd) = std::fs::read_dir(os_path(root, dir)) else {
        return;
    };
    let mut names: Vec<(Vec<u8>, bool)> = rd
        .flatten()
        .map(|e| {
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            (e.file_name().as_encoded_bytes().to_vec(), is_dir)
        })
        .collect();
    names.sort();
    for (name, is_dir) in names {
        let mut rel = dir.to_vec();
        rel.push(b'/');
        rel.extend_from_slice(&name);
        if is_dir {
            list_files(root, &rel, out);
        } else {
            out.push(rel);
        }
    }
}
