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
//! Not done yet: submodules (gitlinks move like files, `.gitmodules` is not
//! updated), sparse-checkout rules (#43).

use std::path::{Path, PathBuf};

use rgit_index::{AddOptions, Entry, Stage, WriteOptions};
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
    let mut lock = rgit_fs::LockFile::acquire(&index_path, &rgit_fs::LockOptions::default())
        .map_err(|e| {
            if let Some(w) = e.warning() {
                eprintln!("warning: {w}");
            }
            Die::Fatal(e.to_string())
        })?;
    let (mut index, index_mtime) = repo.read_index()?;
    let checker = repo.checker(index_mtime)?;

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

    let dest_is_dir = dest.is_empty() || lstat(&root, trim_slashes(&dest)).is_some_and(|m| m.is_dir());
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
    let mut i = 0;
    while i < moves.len() {
        if dry_run {
            println!("{}", msg::checking(&lossy(&moves[i].src), &lossy(&moves[i].dst)));
        }
        let problem = check_one(&root, &index, &mut moves, i, force, verbose);
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
    for m in &moves {
        if verbose || dry_run {
            use std::io::Write;
            let _ = writeln!(out, "{}", msg::renaming(&lossy(&m.src), &lossy(&m.dst)));
        }
        if dry_run {
            continue;
        }
        if m.kind != Kind::Inside {
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
        if m.kind == Kind::Dir {
            continue;
        }
        let Some(old) = index.remove(&m.src, Stage::MERGED) else {
            continue;
        };
        index.remove_path(&m.dst);
        let mut entry = Entry::new(m.dst.clone(), Stage::MERGED, old.mode, old.id);
        entry.flags = old.flags;
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
    if let Some(reference) = index_mtime {
        let _ = smudge_racy_entries(&mut index, reference, &checker);
    }
    let skip_hash = repo.config.bool("index.skiphash").ok().flatten().unwrap_or(false);
    index
        .write_locked(&mut lock, &WriteOptions { skip_hash })
        .and_then(|()| lock.commit())
        .map_err(|e| Die::Fatal(e.to_string()))?;
    Ok(0)
}

/// Checks `moves[i]`; a directory appends its tracked files to `moves`.
fn check_one(
    root: &Path,
    index: &rgit_index::Index,
    moves: &mut Vec<Move>,
    i: usize,
    force: bool,
    verbose: bool,
) -> Option<&'static str> {
    let (src, dst, kind) = (moves[i].src.clone(), moves[i].dst.clone(), moves[i].kind);
    let meta = if src.is_empty() { None } else { lstat(root, &src) };
    let Some(meta) = meta else {
        return Some(msg::BAD_SOURCE);
    };
    if is_within(&dst, &src) {
        return Some(msg::INTO_ITSELF);
    }
    if meta.is_dir() && kind == Kind::File {
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
        None if kind == Kind::File => {
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
