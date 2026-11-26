// SPDX-License-Identifier: MIT
//! `git mv` (Documentation/git-mv.adoc): move or rename files, symbolic
//! links and directories in the work tree and the index. Options `-f`,
//! `-k`, `-n`, `-v`; `--sparse` is accepted.
//!
//! Behaviour determined against the reference git (spec gaps of the
//! documentation, see issue #41):
//!
//! - All sources are checked before anything moves. A failed check is
//!   fatal (`<reason>, source=<src>, destination=<dst>`), or with `-k` the
//!   source is skipped silently.
//! - A tracked directory moves as a whole (untracked files included); its
//!   index entries are appended to the list of moves and checked after the
//!   given sources. Empty leading directories of a source stay.
//! - With `-n`, every checked move prints `Checking rename of ...`; with
//!   `-n` or `-v`, every move prints `Renaming ...`.
//! - With `-k`, a failing `rename()` is ignored and the index is updated
//!   as if it had worked. Deliberate difference: when a directory cannot
//!   be renamed (for example because the destination's parent is missing),
//!   the reference git (2.56) also deletes the source directory, untracked
//!   files included; rgit leaves the work tree alone.
//! - Index entries keep their object, mode and flags; the cached stat data
//!   is refreshed when the moved file still matches the entry.
//!
//! - A submodule (gitlink) moves like a file; its `.git` file and the
//!   `core.worktree` of an absorbed git directory are rewritten for the new
//!   place, and its `path` in `.gitmodules` is changed and staged.
//!
//! Sparse checkouts (git-mv(1) `--sparse`): without `--sparse`, a source
//! outside the sparse checkout (skip-worktree, or outside the patterns)
//! and a destination outside the patterns are refused and listed with the
//! `updateSparsePath` advice (with `-k` the move is just skipped). With
//! `--sparse`, a skip-worktree source moving into the checkout gets its
//! file written, and a clean file moving out of it is removed and marked
//! skip-worktree; a modified one stays and is reported.

use std::path::{Path, PathBuf};

use rgit_index::{AddOptions, Entry, Stage};
use rgit_worktree::stat::{Status, smudge_racy_entries};

use crate::messages::mv as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;

fn lossy(p: &[u8]) -> String {
    String::from_utf8_lossy(p).into_owned()
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

fn lstat(root: &Path, rel: &[u8]) -> Option<std::fs::Metadata> {
    std::fs::symlink_metadata(os_path(root, rel)).ok()
}

fn trim_slashes(p: &[u8]) -> &[u8] {
    let mut p = p;
    while let Some(rest) = p.strip_suffix(b"/") {
        p = rest;
    }
    p
}

fn join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    let dir = trim_slashes(dir);
    let mut out = dir.to_vec();
    if !out.is_empty() {
        out.push(b'/');
    }
    out.extend_from_slice(name);
    out
}

fn basename(p: &[u8]) -> &[u8] {
    p.iter()
        .rposition(|&b| b == b'/')
        .map_or(p, |i| &p[i + 1..])
}

/// `path` lies below directory `dir` (or is `dir` itself).
fn is_within(path: &[u8], dir: &[u8]) -> bool {
    path == dir || (path.len() > dir.len() && path.starts_with(dir) && path[dir.len()] == b'/')
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A file or symbolic link given on the command line.
    File,
    /// A directory given on the command line.
    Dir,
    /// A tracked file below a moved directory.
    Inside,
}

struct Move {
    src: Vec<u8>,
    dst: Vec<u8>,
    kind: Kind,
}

/// Why the leading directories of `dst` cannot take a new file.
fn check_leading(root: &Path, dst: &[u8]) -> Option<&'static str> {
    if dst.ends_with(b"/") {
        return Some(msg::DEST_DIR_MISSING);
    }
    for (i, _) in dst.iter().enumerate().filter(|(_, b)| **b == b'/') {
        match lstat(root, &dst[..i]) {
            None => return Some(msg::DEST_DIR_MISSING),
            Some(m) if m.file_type().is_symlink() => return Some(msg::DEST_BEYOND_SYMLINK),
            Some(m) if !m.is_dir() => return Some(msg::DEST_NOT_DIR),
            Some(_) => {}
        }
    }
    None
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let verbose = Cell::new(false);
    let dry_run = Cell::new(false);
    let force = Cell::new(false);
    let keep_going = Cell::new(false);
    let sparse = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag('v', "verbose", &verbose, msg::HELP_VERBOSE),
            Opt::flag('n', "dry-run", &dry_run, msg::HELP_DRY_RUN),
            Opt::flag('f', "force", &force, msg::HELP_FORCE),
            Opt::flag('k', None, &keep_going, msg::HELP_K),
            Opt::flag(None, "sparse", &sparse, msg::HELP_SPARSE),
        ],
    );
    let args = parser.parse(argv)?.args;
    if args.len() < 2 {
        return Err(Die::Output(parser.usage_error()));
    }
    drop(parser);
    let (verbose, dry_run, force, keep_going) =
        (verbose.get(), dry_run.get(), force.get(), keep_going.get());

    let repo = Repo::open(ctx, Mode::Required)?;
    let root = repo.work_tree()?;
    let index_path = repo.setup.cwd.join(repo.index_path());
    let mut lock = rgit_fs::LockFile::acquire(&index_path, &repo.index_lock_options())
        .map_err(|e| {
            if let Some(w) = e.warning() {
                eprintln!("warning: {w}");
            }
            Die::Fatal(e.to_string())
        })?;
    let (mut index, index_mtime) = repo.read_index()?;
    let checker = repo.checker(index_mtime)?;
    let patterns = crate::sparse::load(&repo);
    let allow_sparse = sparse.get();
    let out_of_patterns = |p: &[u8]| patterns.as_ref().is_some_and(|s| !s.includes(p));
    // Moving clean files out of the checkout makes them sparse (cone mode).
    let cone = patterns.as_ref().is_some_and(rgit_worktree::sparse::Sparse::is_cone);
    let advisor = ctx.advisor(&repo.config)?;

    let root_shown = lossy(root.as_os_str().as_encoded_bytes());
    let normalize = |arg: &String| -> Result<Vec<u8>, Die> {
        repo.prefix_path(arg.as_bytes())
            .map_err(|()| Die::Fatal(msg::outside_repository(arg, &root_shown)))
    };
    let (dest_arg, src_args) = args.split_last().expect("two arguments");
    let mut sources = Vec::with_capacity(src_args.len());
    for a in src_args {
        sources.push(trim_slashes(&normalize(a)?).to_vec());
    }
    let dest = normalize(dest_arg)?;

    let dest_is_dir = dest.is_empty()
        || lstat(&root, trim_slashes(&dest)).is_some_and(|m| m.is_dir())
        || (lstat(&root, trim_slashes(&dest)).is_none()
            && !index.dir_range(trim_slashes(&dest)).is_empty());
    let mut moves: Vec<Move> = Vec::new();
    for src in sources.iter() {
        let dst = if dest_is_dir {
            join(&dest, basename(src))
        } else if sources.len() == 1 {
            dest.clone()
        } else {
            return Err(Die::Fatal(msg::not_a_directory(&lossy(&dest))));
        };
        moves.push(Move { src: src.clone(), dst, kind: Kind::File });
    }

    // Check every move; directories add their tracked files at the end.
    // Without --sparse, paths outside the sparse checkout are refused.
    let mut refused_paths: Vec<Vec<u8>> = Vec::new();
    let mut refused: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < moves.len() {
        if dry_run {
            println!("{}", msg::checking(&lossy(&moves[i].src), &lossy(&moves[i].dst)));
        }
        if !allow_sparse && !source_is_dir(&root, &index, &moves[i].src) {
            let (src, dst) = (&moves[i].src, &moves[i].dst);
            let src_out = index
                .get(src, Stage::MERGED)
                .is_some_and(|e| e.flags.skip_worktree)
                || out_of_patterns(src);
            let dst_out = out_of_patterns(dst);
            if src_out {
                refused_paths.push(src.clone());
            }
            if dst_out {
                refused_paths.push(dst.clone());
            }
            if src_out || dst_out {
                refused.push(i);
                i += 1;
                continue;
            }
        }
        let dst_out = out_of_patterns(&moves[i].dst);
        let src_out = index
            .get(&moves[i].src, Stage::MERGED)
            .is_some_and(|e| e.flags.skip_worktree)
            || out_of_patterns(&moves[i].src);
        let leaving = cone && dst_out && !src_out;
        let problem = check_one(&root, &index, &mut moves, i, force, verbose, dst_out, leaving);
        match problem {
            None => i += 1,
            Some(_) if keep_going => {
                moves.remove(i);
            }
            Some(what) => {
                return Err(Die::Fatal(msg::bad_move(
                    what,
                    &lossy(&moves[i].src),
                    &lossy(&moves[i].dst),
                )));
            }
        }
    }

    if !refused_paths.is_empty() {
        crate::sparse::report_outside(&advisor, &refused_paths)?;
        if !keep_going {
            return Ok(1);
        }
        // A directory with a refused file inside stays as a whole.
        let mut drop: Vec<bool> = vec![false; moves.len()];
        for &r in &refused {
            drop[r] = true;
            if moves[r].kind == Kind::Inside {
                for (j, m) in moves.iter().enumerate() {
                    if m.kind == Kind::Dir && is_within(&moves[r].src, &m.src) {
                        drop[j] = true;
                        for (k, n) in moves.iter().enumerate() {
                            if n.kind == Kind::Inside && is_within(&n.src, &m.src) {
                                drop[k] = true;
                            }
                        }
                    }
                }
            }
        }
        let mut k = drop.into_iter();
        moves.retain(|_| !k.next().unwrap_or(false));
    }

    // A file and one of its leading directories cannot both move.
    for m in moves.iter().filter(|m| m.kind != Kind::Inside) {
        if let Some(parent) = moves
            .iter()
            .find(|p| p.kind == Kind::Dir && p.src != m.src && is_within(&m.src, &p.src))
        {
            return Err(Die::Fatal(msg::both_and_parent(&lossy(&m.src), &lossy(&parent.src))));
        }
    }

    let mut out = std::io::stdout().lock();
    let mut moved_links: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    // Directories moved file by file (out of the sparse checkout, or from
    // a sparse directory without files); their sources are pruned after.
    let mut per_file_dirs: Vec<Vec<u8>> = Vec::new();
    let mut dirty: Vec<Vec<u8>> = Vec::new();
    let mut to_write: Vec<Vec<u8>> = Vec::new();
    for m in &moves {
        if verbose || dry_run {
            use std::io::Write;
            let _ = writeln!(out, "{}", msg::renaming(&lossy(&m.src), &lossy(&m.dst)));
        }
        if dry_run {
            continue;
        }
        let in_per_file =
            m.kind == Kind::Inside && per_file_dirs.iter().any(|d| is_within(&m.src, d));
        let old_entry = index.get(&m.src, Stage::MERGED).cloned();
        let src_skip = old_entry.as_ref().is_some_and(|e| e.flags.skip_worktree);
        let dst_out = out_of_patterns(&m.dst);
        let src_out = src_skip || out_of_patterns(&m.src);
        // A clean file leaving the sparse checkout is removed instead.
        let leaving = cone && dst_out && !src_out;
        if m.kind == Kind::Dir && (lstat(&root, &m.src).is_none() || cone && dst_out) {
            per_file_dirs.push(m.src.clone());
            continue;
        }
        let mut was_dirty = false;
        if m.kind != Kind::Inside || in_per_file {
            let mut skip_rename = m.kind == Kind::File && src_skip || in_per_file && src_skip;
            if leaving && m.kind != Kind::Dir {
                let clean = old_entry.as_ref().is_some_and(|e| {
                    matches!(checker.check(e), Ok(Status::Unchanged | Status::StatOnly))
                });
                if clean {
                    let _ = std::fs::remove_file(os_path(&root, &m.src));
                    skip_rename = true;
                } else {
                    was_dirty = true;
                    dirty.push(m.dst.clone());
                    let _ = rgit_unpack::create_leading_dirs(&root, &m.dst, true);
                }
            }
            if !skip_rename {
                if in_per_file {
                    let _ = rgit_unpack::create_leading_dirs(&root, &m.dst, true);
                }
                // With -k a failed rename is ignored; the index is updated anyway.
                let renamed = std::fs::rename(os_path(&root, &m.src), os_path(&root, &m.dst));
                if let (Err(e), false) = (renamed, keep_going) {
                    return Err(Die::Fatal(msg::rename_failed(
                        &lossy(&m.src),
                        &lossy(&m.dst),
                        &crate::global::strerror(&e),
                    )));
                }
            }
        }
        if m.kind == Kind::Dir {
            continue;
        }
        let Some(old) = index.remove(&m.src, Stage::MERGED) else {
            continue;
        };
        let new_skip = if src_skip && !dst_out {
            // Entering the sparse checkout: the file is written.
            to_write.push(m.dst.clone());
            false
        } else if leaving && (m.kind == Kind::File || in_per_file) {
            !was_dirty
        } else {
            old.flags.skip_worktree
        };
        if old.mode.is_gitlink() {
            moved_links.push((m.src.clone(), m.dst.clone()));
        }
        index.remove_path(&m.dst);
        let mut entry = Entry::new(m.dst.clone(), Stage::MERGED, old.mode, old.id);
        entry.flags = old.flags;
        entry.flags.skip_worktree = new_skip;
        entry.stat = old.stat;
        if let Ok(Some(meta)) = checker.lstat(&m.dst) {
            if matches!(checker.check_with(&entry, &meta), Ok(Status::Unchanged | Status::StatOnly)) {
                checker.refresh(&mut entry, &meta);
            }
        }
        match index.add(entry, AddOptions { ok_to_replace: true, ..Default::default() }) {
            Ok(()) => {}
            Err(rgit_index::AddError::InvalidPath(p)) => {
                eprintln!("error: {}", msg::invalid_path(&lossy(&p)));
            }
            Err(e) => return Err(Die::Fatal(e.to_string())),
        }
    }
    drop(out);
    if dry_run {
        lock.rollback();
        return Ok(0);
    }
    for d in &per_file_dirs {
        remove_empty_dirs(&os_path(&root, d));
    }
    if !to_write.is_empty() {
        let conv = crate::convert::Converter::checkout(&repo, &index)?;
        let symlinks = checker.options().symlinks;
        for path in &to_write {
            let Ok(pos) = index.position(path, Stage::MERGED) else {
                continue;
            };
            let entry = index.entries()[pos].clone();
            let Ok(data) = rgit_unpack::read_blob(repo.odb(), &entry.id) else {
                continue;
            };
            let data = if entry.mode.is_file() {
                rgit_unpack::Filter::to_worktree(&conv, path, &entry.id, data)
            } else {
                data
            };
            match rgit_unpack::write_entry(&root, path, &entry, &data, symlinks) {
                Ok(Some(meta)) => {
                    if let Some(e) = index.entry_mut(pos) {
                        e.stat = rgit_worktree::stat::stat_from_metadata(&meta);
                    }
                }
                Ok(None) => {}
                Err(e) => eprintln!("error: {e}"),
            }
        }
    }
    if !dirty.is_empty() {
        let mut text = String::from(msg::DIRTY_OUTSIDE_HEADER);
        text.push('\n');
        for d in &dirty {
            text.push_str(&lossy(d));
            text.push('\n');
        }
        eprint!("{text}");
        advisor
            .advise(Advice::UpdateSparsePath, msg::DIRTY_OUTSIDE_HINT)
            .map_err(Die::Fatal)?;
    }
    for (src, dst) in &moved_links {
        crate::submodules::moved(&repo, &mut index, src, dst)?;
    }
    if let Some(reference) = index_mtime {
        let _ = smudge_racy_entries(&mut index, reference, &checker);
    }
    let opts = crate::index_config::write_options(&repo.config, &index)?;
    index
        .write_locked(&mut lock, &opts)
        .and_then(|()| lock.commit())
        .map_err(|e| Die::Fatal(e.to_string()))?;
    Ok(0)
}

/// Whether `src` is a directory: in the work tree, or (gone in a sparse
/// checkout) as the leading directory of index entries.
fn source_is_dir(root: &Path, index: &rgit_index::Index, src: &[u8]) -> bool {
    match lstat(root, src) {
        Some(m) => m.is_dir(),
        None => {
            !src.is_empty()
                && index.get(src, Stage::MERGED).is_none()
                && !index.dir_range(src).is_empty()
        }
    }
}

/// Removes `dir` and the directories below it when they hold no files.
fn remove_empty_dirs(dir: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut empty = true;
    for e in rd.flatten() {
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            empty &= remove_empty_dirs(&e.path());
        } else {
            empty = false;
        }
    }
    empty && std::fs::remove_dir(dir).is_ok()
}

/// Checks `moves[i]`; a directory appends its tracked files to `moves`.
#[allow(clippy::too_many_arguments)]
fn check_one(
    root: &Path,
    index: &rgit_index::Index,
    moves: &mut Vec<Move>,
    i: usize,
    force: bool,
    verbose: bool,
    dst_out: bool,
    leaving: bool,
) -> Option<&'static str> {
    let (src, dst, kind) = (moves[i].src.clone(), moves[i].dst.clone(), moves[i].kind);
    let meta = if src.is_empty() { None } else { lstat(root, &src) };
    // A skip-worktree file or a sparse directory has no file to look at.
    let is_dir = match &meta {
        Some(m) => m.is_dir(),
        None if src.is_empty() => return Some(msg::BAD_SOURCE),
        None if index
            .get(&src, Stage::MERGED)
            .is_some_and(|e| e.flags.skip_worktree) =>
        {
            false
        }
        None if source_is_dir(root, index, &src) => true,
        None => return Some(msg::BAD_SOURCE),
    };
    if is_within(&dst, &src) {
        return Some(msg::INTO_ITSELF);
    }
    let gitlink = index
        .get(&src, Stage::MERGED)
        .is_some_and(|e| e.mode.is_gitlink());
    if is_dir && kind == Kind::File && !gitlink {
        let range = index.dir_range(&src);
        if range.is_empty() {
            return Some(msg::DIR_EMPTY);
        }
        let dst = trim_slashes(&dst).to_vec();
        if lstat(root, &dst).is_some() {
            return Some(msg::DIR_DEST_EXISTS);
        }
        let mut inside: Vec<Move> = Vec::new();
        for e in &index.entries()[range] {
            if inside.last().is_some_and(|m| m.src.as_slice() == e.path()) {
                continue;
            }
            inside.push(Move {
                src: e.path().to_vec(),
                dst: [dst.as_slice(), &e.path()[src.len()..]].concat(),
                kind: Kind::Inside,
            });
        }
        moves[i].dst = dst;
        moves[i].kind = Kind::Dir;
        moves.extend(inside);
        return None;
    }
    let entries = index.entries_for_path(&src);
    if entries.is_empty() {
        return Some(msg::NOT_TRACKED);
    }
    if !entries.iter().any(|e| e.stage() == Stage::MERGED) {
        return Some(msg::CONFLICTED);
    }
    match lstat(root, &dst) {
        Some(d) => {
            if !force {
                return Some(msg::DEST_EXISTS);
            }
            if d.is_dir() {
                return Some(msg::CANNOT_OVERWRITE);
            }
            if verbose {
                eprintln!("warning: {}", msg::overwriting(&lossy(&dst)));
            }
        }
        None if leaving && index.get(&dst, Stage::MERGED).is_some() && !force => {
            return Some(msg::DEST_IN_INDEX);
        }
        None if kind == Kind::File && !dst_out => {
            if let Some(what) = check_leading(root, &dst) {
                return Some(what);
            }
        }
        None => {}
    }
    if moves[..i].iter().any(|m| m.dst == dst) {
        return Some(msg::MULTIPLE_SOURCES);
    }
    None
}
