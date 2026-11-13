// SPDX-License-Identifier: MIT
//! `git add` (Documentation/git-add.adoc): new and modified files and
//! removals under the given pathspecs, `-A`, `-u`, `-n`, `-v`, `-f`,
//! `-N`/`--intent-to-add`, `--chmod=(+|-)x`, `--ignore-errors`
//! (`add.ignoreErrors`), `--ignore-missing` (with `--dry-run`),
//! `--pathspec-from-file`/`--pathspec-file-nul`; `--sparse` is accepted.
//! Ignore rules come from rgit-worktree, the index is written with racy
//! entries smudged.
//!
//! Behaviour determined against the reference git (spec gaps, issue #41):
//!
//! - With `-N`, matched tracked files are listed (`add '<path>'`) but not
//!   updated, deleted ones are removed; new files get an empty
//!   intent-to-add entry without being read.
//! - `--chmod` applies to every index entry matched by the pathspecs after
//!   adding, also to unchanged ones; without pathspecs it does nothing.
//!   Symbolic links and submodules fail with `cannot chmod` (exit 255).
//! - A file that cannot be read fails the tracked phase ("updating files
//!   failed") or the new-file phase ("adding files failed"), unless
//!   `--ignore-errors` is given; then the others are added and the exit
//!   code is 1.
//! - An ignored directory is reported (instead of its files) when a
//!   pathspec names it or a path below it. A pathspec that matched nothing
//!   is accepted when it names an existing ignored path; with
//!   `--ignore-missing` a missing path that would be ignored is reported
//!   as ignored and other missing paths are accepted.
//!
//! Not done yet: eol/filter attributes (a warning is printed instead, see
//! issue #107), `--renormalize`, `--refresh`, interactive modes, untracked
//! nested repositories.

use std::path::{Path, PathBuf};

use rgit_index::{AddOptions, Entry, Index, Mode, Stage, WriteOptions};
use rgit_object::ObjectKind;
use rgit_pathspec::{DirInterest, MatchKind, ParseOptions, Pathspec};
use rgit_worktree::ignore::{Excludes, default_excludes_file};
use rgit_worktree::stat::{
    Checker, Status, mode_from_metadata, smudge_racy_entries, stat_from_metadata,
};

use crate::messages::add as msg;
use crate::prelude::*;
use crate::repo::Mode as RepoMode;
use crate::repository::{self, Repo};

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

/// Whether eol or filter attributes may apply (core.autocrlf, or a
/// top-level `.gitattributes` mentioning them). Decision on #107: warn,
/// never convert silently.
pub(crate) fn attributes_may_apply(repo: &Repo, root: &Path) -> bool {
    if let Ok(Some(v)) = repo.config.string("core.autocrlf") {
        let v = String::from_utf8_lossy(v).to_ascii_lowercase();
        if ["true", "input", "yes", "on", "1"].contains(&v.as_str()) {
            return true;
        }
    }
    let Ok(text) = std::fs::read(root.join(".gitattributes")) else {
        return false;
    };
    let text = String::from_utf8_lossy(&text);
    text.lines().any(|l| {
        let l = l.trim();
        !l.starts_with('#')
            && l.split_whitespace().skip(1).any(|a| {
                let a = a.trim_start_matches(['-', '!']);
                a.starts_with("text")
                    || a.starts_with("eol")
                    || a.starts_with("crlf")
                    || a.starts_with("filter")
                    || a.starts_with("ident")
                    || a.starts_with("working-tree-encoding")
            })
    })
}

struct Walk<'a> {
    root: PathBuf,
    index: &'a Index,
    pathspec: &'a Pathspec,
    excludes: Excludes,
    force: bool,
    seen: Vec<bool>,
    found: Vec<Vec<u8>>,
    ignored: Vec<Vec<u8>>,
}

impl Walk<'_> {
    /// Whether a pathspec names the directory `dir` or a path below it.
    fn names_inside(&self, dir: &[u8]) -> bool {
        self.pathspec.items().iter().any(|it| {
            if it.is_exclude() {
                return false;
            }
            let p = it.pattern();
            let p = p.strip_suffix(b"/").unwrap_or(p);
            p == dir || (p.len() > dir.len() && p.starts_with(dir) && p[dir.len()] == b'/')
        })
    }

    fn walk(&mut self, dir: &[u8], parent_excluded: bool) {
        let Ok(rd) = std::fs::read_dir(os_path(&self.root, dir)) else {
            return;
        };
        let mut entries: Vec<(Vec<u8>, std::fs::FileType)> = rd
            .flatten()
            .filter_map(|e| Some((e.file_name().as_encoded_bytes().to_vec(), e.file_type().ok()?)))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, ft) in entries {
            if name == b".git" {
                continue;
            }
            let mut path = dir.to_vec();
            path.extend_from_slice(&name);
            if ft.is_dir() {
                if !self.index.path_range(&path).is_empty() {
                    continue; // submodule or tracked file turned directory
                }
                if !self.pathspec.is_empty()
                    && self.pathspec.dir_interest(&path) == DirInterest::None
                {
                    continue;
                }
                if self.index.dir_range(&path).is_empty()
                    && std::fs::symlink_metadata(os_path(&self.root, &path).join(".git")).is_ok()
                {
                    continue; // untracked nested repository: not handled yet
                }
                let excluded = parent_excluded || self.excludes.is_excluded(&path, true);
                if excluded && !self.force {
                    // Nothing below is added; the directory itself is
                    // reported when a pathspec reaches into it.
                    if self.names_inside(&path) {
                        self.ignored.push(path);
                    }
                    continue;
                }
                path.push(b'/');
                self.walk(&path, excluded);
            } else {
                if !self.index.path_range(&path).is_empty() {
                    continue;
                }
                let kind = if self.pathspec.is_empty() {
                    Some(MatchKind::Recursive)
                } else {
                    self.pathspec.match_path(&path)
                };
                let Some(kind) = kind else { continue };
                let excluded = parent_excluded || self.excludes.is_excluded(&path, false);
                if excluded && !self.force {
                    if kind == MatchKind::Exact {
                        self.ignored.push(path);
                    }
                    continue;
                }
                if !self.pathspec.is_empty() {
                    let _ = self.pathspec.match_path_seen(&path, &mut self.seen);
                }
                self.found.push(path);
            }
        }
    }
}

pub(crate) fn build_excludes(repo: &Repo, root: &Path) -> Excludes {
    let mut ex = Excludes::new(root.to_path_buf());
    ex.set_ignore_case(repo.config.bool("core.ignorecase").ok().flatten().unwrap_or(false));
    ex.set_per_directory(b".gitignore".to_vec());
    let env = rgit_config::SystemEnv::default();
    let file = match repo.config.path("core.excludesfile", &env) {
        Ok(Some(p)) => Some(p),
        _ => default_excludes_file(|k| std::env::var_os(k)),
    };
    let info = repo.setup.common_dir.join("info").join("exclude");
    ex.add_standard(
        file.as_deref()
            .map(|p| (p, p.as_os_str().as_encoded_bytes().to_vec())),
        (&info, info.as_os_str().as_encoded_bytes().to_vec()),
    );
    ex
}

/// Hash (and unless `dry_run`, store) the work tree file `path` and build
/// its stage-0 entry. `Err` is git's error text for an unreadable file.
pub(crate) fn entry_for(
    repo: &Repo,
    checker: &Checker,
    root: &Path,
    path: &[u8],
    old: Option<&Entry>,
    dry_run: bool,
) -> Result<Option<Entry>, String> {
    let shown = lossy(path);
    let strerror = |e: &std::io::Error| crate::global::strerror(e);
    let Some(meta) = checker
        .lstat(path)
        .map_err(|e| format!("lstat(\"{shown}\"): {}", strerror(&e)))?
    else {
        return Ok(None);
    };
    let Some(mode) = mode_from_metadata(&meta, checker.options(), old.map(|e| e.mode)) else {
        return Ok(None);
    };
    let full = os_path(root, path);
    let data = if meta.file_type().is_symlink() {
        std::fs::read_link(&full)
            .map(|t| t.as_os_str().as_encoded_bytes().to_vec())
            .map_err(|e| format!("readlink(\"{shown}\"): {}", strerror(&e)))?
    } else {
        std::fs::read(&full).map_err(|e| format!("open(\"{shown}\"): {}", strerror(&e)))?
    };
    let id = if dry_run {
        rgit_hash::hash_object(repo.hash, "blob", &data).map_err(|e| e.to_string())?
    } else {
        repo.odb()
            .write(ObjectKind::Blob, &data)
            .map_err(|e| e.to_string())?
    };
    let mut entry = Entry::new(path.to_vec(), Stage::MERGED, mode, id);
    entry.stat = stat_from_metadata(&meta);
    if let Some(o) = old {
        entry.flags = o.flags;
        entry.flags.intent_to_add = false;
    }
    Ok(Some(entry))
}

/// The empty intent-to-add entry for the new file `path` (`-N`).
fn intent_entry(repo: &Repo, checker: &Checker, path: &[u8]) -> Result<Option<Entry>, Die> {
    let Ok(Some(meta)) = checker.lstat(path) else {
        return Ok(None);
    };
    let Some(mode) = mode_from_metadata(&meta, checker.options(), None) else {
        return Ok(None);
    };
    let id = rgit_hash::hash_object(repo.hash, "blob", b"").map_err(|e| Die::Fatal(e.to_string()))?;
    let mut entry = Entry::new(path.to_vec(), Stage::MERGED, mode, id);
    entry.flags.intent_to_add = true;
    Ok(Some(entry))
}

fn report_failure(path: &[u8], err: &str) {
    eprintln!("error: {err}");
    eprintln!("error: {}", msg::unable_to_index(&lossy(path)));
}

/// Reads the `--pathspec-from-file` input.
fn read_pathspec_file(name: &str, full: &Path, nul: bool) -> Result<Vec<Vec<u8>>, Die> {
    use std::io::Read;
    let mut data = Vec::new();
    let read = if name == "-" {
        std::io::stdin().read_to_end(&mut data).map(|_| ())
    } else {
        std::fs::File::open(full).and_then(|mut f| f.read_to_end(&mut data).map(|_| ()))
    };
    read.map_err(|e| Die::Fatal(msg::could_not_open(name, &crate::global::strerror(&e))))?;
    rgit_pathspec::parse_pathspec_file(&data, nul).map_err(|e| Die::Fatal(e.to_string()))
}

#[allow(clippy::too_many_lines)]
pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let dry_run = Cell::new(false);
    let verbose = Cell::new(false);
    let force = Cell::new(false);
    let update = Cell::new(false);
    let intent = Cell::new(false);
    let all = Cell::new(false);
    let ignore_errors = Cell::new(-1);
    let ignore_missing = Cell::new(false);
    let sparse = Cell::new(false);
    let chmod: RefCell<Option<String>> = RefCell::new(None);
    let from_file: RefCell<Option<String>> = RefCell::new(None);
    let file_nul = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag('n', "dry-run", &dry_run, msg::HELP_DRY_RUN),
            Opt::flag('v', "verbose", &verbose, msg::HELP_VERBOSE),
            Opt::group(""),
            Opt::flag('f', "force", &force, msg::HELP_FORCE),
            Opt::flag('u', "update", &update, msg::HELP_UPDATE),
            Opt::flag('N', "intent-to-add", &intent, msg::HELP_INTENT_TO_ADD),
            Opt::flag('A', "all", &all, msg::HELP_ALL),
            Opt::bool(None, "ignore-errors", &ignore_errors, msg::HELP_IGNORE_ERRORS),
            Opt::flag(None, "ignore-missing", &ignore_missing, msg::HELP_IGNORE_MISSING),
            Opt::flag(None, "sparse", &sparse, msg::HELP_SPARSE),
            Opt::string(None, "chmod", &chmod, "(+|-)x", msg::HELP_CHMOD).literal_argh(),
            Opt::string(None, "pathspec-from-file", &from_file, "file", msg::HELP_PATHSPEC_FROM_FILE),
            Opt::flag(None, "pathspec-file-nul", &file_nul, msg::HELP_PATHSPEC_FILE_NUL),
        ],
    );
    let mut args: Vec<Vec<u8>> = parser
        .parse(argv)?
        .args
        .into_iter()
        .map(String::into_bytes)
        .collect();
    drop(parser);
    let (dry_run, verbose, force, update, intent, all) = (
        dry_run.get(),
        verbose.get(),
        force.get(),
        update.get(),
        intent.get(),
        all.get(),
    );
    let chmod = chmod.into_inner();
    let from_file = from_file.into_inner();
    if update && all {
        return Err(Die::Fatal(msg::UPDATE_AND_ALL.to_string()));
    }
    let flip = match chmod.as_deref() {
        None => None,
        Some("+x") => Some('+'),
        Some("-x") => Some('-'),
        Some(other) => return Err(Die::Fatal(msg::bad_chmod(other))),
    };
    if ignore_missing.get() && !dry_run {
        return Err(Die::Fatal(msg::option_requires("--ignore-missing", "--dry-run")));
    }
    if from_file.is_some() && !args.is_empty() {
        return Err(Die::Fatal(msg::FROM_FILE_WITH_ARGS.to_string()));
    }
    if file_nul.get() && from_file.is_none() {
        return Err(Die::Fatal(msg::option_requires(
            "--pathspec-file-nul",
            "--pathspec-from-file",
        )));
    }

    let repo = Repo::open(ctx, RepoMode::Required)?;
    let root = repo.work_tree()?;
    if let Some(name) = &from_file {
        // A relative file name is relative to the original directory.
        let full = if name == "-" || Path::new(name).is_absolute() {
            PathBuf::from(name)
        } else {
            os_path(&root, &repo.prefix()).join(name)
        };
        args = read_pathspec_file(name, &full, file_nul.get())?;
    }
    let ignore_errors = match ignore_errors.get() {
        -1 => ["add.ignore-errors", "add.ignoreerrors"]
            .iter()
            .find_map(|k| repo.config.bool(k).ok().flatten())
            .unwrap_or(false),
        v => v != 0,
    };
    if args.is_empty() && !update && !all {
        eprintln!("{}", msg::NOTHING_SPECIFIED);
        eprintln!("{}", msg::HINT_ADD_DOT);
        return Ok(0);
    }
    let prefix = repo.prefix();
    let opts = ParseOptions {
        prefix: prefix.clone(),
        root: Some(root.as_os_str().as_encoded_bytes().to_vec()),
        globals: repository::pathspec_globals(ctx),
        ..Default::default()
    };
    let pathspec = Pathspec::parse(&args, &opts).map_err(|e| Die::Fatal(e.to_string()))?;
    let (mut index, index_mtime) = repo.read_index()?;
    let checker = repo.checker(index_mtime)?;
    let mut seen = vec![false; pathspec.items().len()];
    let warn_attrs = attributes_may_apply(&repo, &root);
    let show = verbose || dry_run;
    // With -N, git stores the empty blob for every file it lists as added
    // (tracked or new), with --dry-run too.
    let write_empty_blob = || {
        repo.odb()
            .write(ObjectKind::Blob, b"")
            .map_err(|e| Die::Fatal(e.to_string()))
    };

    // Tracked files first, in index order.
    let mut updates: Vec<Entry> = Vec::new();
    let mut removals: Vec<Vec<u8>> = Vec::new();
    let mut failed = false;
    for e in index.entries() {
        if !pathspec.is_empty() && pathspec.match_path_seen(e.path(), &mut seen).is_none() {
            continue;
        }
        if pathspec.is_empty() && !prefix.is_empty() && !update && !all {
            continue;
        }
        if e.stage() != Stage::MERGED || e.mode.is_gitlink() {
            continue;
        }
        if intent && e.flags.intent_to_add {
            continue;
        }
        let status = checker
            .check(e)
            .map_err(|err| Die::Fatal(format!("{}: {err}", lossy(e.path()))))?;
        match status {
            Status::Deleted => {
                if show {
                    println!("{}", msg::remove(&lossy(e.path())));
                }
                removals.push(e.path().to_vec());
            }
            _ if intent => {
                write_empty_blob()?;
                if show {
                    println!("{}", msg::add(&lossy(e.path())));
                }
            }
            Status::Unchanged => {}
            Status::StatOnly => {
                if let Ok(Some(n)) = entry_for(&repo, &checker, &root, e.path(), Some(e), true) {
                    updates.push(n);
                }
            }
            Status::Modified | Status::TypeChanged => {
                match entry_for(&repo, &checker, &root, e.path(), Some(e), dry_run) {
                    Ok(n) => {
                        if show {
                            println!("{}", msg::add(&lossy(e.path())));
                        }
                        if let Some(n) = n {
                            if warn_attrs {
                                eprintln!("warning: {}", msg::attributes_not_applied(&lossy(e.path())));
                            }
                            updates.push(n);
                        }
                    }
                    Err(err) => {
                        report_failure(e.path(), &err);
                        failed = true;
                    }
                }
            }
        }
    }
    if failed && !ignore_errors {
        return Err(Die::Fatal(msg::UPDATING_FAILED.to_string()));
    }

    // Then untracked files in directory order.
    let mut found = Vec::new();
    let mut ignored = Vec::new();
    let mut excludes = build_excludes(&repo, &root);
    if !update {
        let mut walk = Walk {
            root: root.clone(),
            index: &index,
            pathspec: &pathspec,
            excludes,
            force,
            seen,
            found: Vec::new(),
            ignored: Vec::new(),
        };
        walk.walk(b"", false);
        for w in walk.excludes.take_warnings() {
            eprintln!("warning: {w}");
        }
        seen = walk.seen;
        found = walk.found;
        ignored = walk.ignored;
        excludes = walk.excludes;
    }
    for item in pathspec.unmatched(&seen) {
        let pattern = item.pattern();
        let path = pattern.strip_suffix(b"/").unwrap_or(pattern);
        let meta = std::fs::symlink_metadata(os_path(&root, pattern)).ok();
        let exists = meta.is_some();
        let is_dir = pattern.len() != path.len() || meta.is_some_and(|m| m.is_dir());
        let is_ignored = !force
            && excludes
                .match_with_leading_dirs(path, is_dir)
                .is_some_and(|m| m.is_excluded());
        if exists {
            continue; // ignored ones were reported by the walk
        }
        if ignore_missing.get() {
            if !exists && is_ignored {
                ignored.push(pattern.to_vec());
            }
            continue;
        }
        return Err(Die::Fatal(rgit_pathspec::messages::did_not_match_any_files(
            item.original(),
        )));
    }
    if update {
        // With -u only tracked files count.
        let mut any = false;
        for item in pathspec.unmatched(&seen) {
            eprintln!(
                "error: {}",
                rgit_pathspec::messages::did_not_match_known_to_git(item.original())
            );
            any = true;
        }
        if any {
            return Err(Die::Exit(128));
        }
    }

    let mut add_failed = false;
    for path in &found {
        if intent {
            write_empty_blob()?;
            if let Some(n) = intent_entry(&repo, &checker, path)? {
                updates.push(n);
            }
            if show {
                println!("{}", msg::add(&lossy(path)));
            }
            continue;
        }
        match entry_for(&repo, &checker, &root, path, None, dry_run) {
            Ok(n) => {
                if show {
                    println!("{}", msg::add(&lossy(path)));
                }
                if warn_attrs {
                    eprintln!("warning: {}", msg::attributes_not_applied(&lossy(path)));
                }
                if let Some(n) = n {
                    updates.push(n);
                }
            }
            Err(err) => {
                report_failure(path, &err);
                add_failed = true;
            }
        }
    }
    if add_failed && !ignore_errors {
        return Err(Die::Fatal(msg::ADDING_FAILED.to_string()));
    }
    let mut code = if failed || add_failed { 1 } else { 0 };
    if !ignored.is_empty() {
        eprintln!("{}", msg::IGNORED_HEADER);
        for p in &ignored {
            eprintln!("{}", lossy(p));
        }
        eprintln!("{}", msg::IGNORED_HINT);
        code = 1;
    }

    let mut changed = !updates.is_empty() || !removals.is_empty();
    if !dry_run {
        for p in removals {
            index.remove_path(&p);
        }
        for e in updates {
            let path = e.path().to_vec();
            index
                .add(e, AddOptions { ok_to_replace: true, ..Default::default() })
                .map_err(|err| Die::Fatal(err.to_string()))?;
            index.invalidate_path(&path);
        }
    }

    // --chmod: every index entry the pathspecs match.
    if let (Some(flip), false) = (flip, pathspec.is_empty()) {
        let mode = if flip == '+' { Mode::EXECUTABLE } else { Mode::FILE };
        let mut flipped = Vec::new();
        for (pos, e) in index.entries().iter().enumerate() {
            if !pathspec.matches(e.path()) {
                continue;
            }
            if !e.mode.is_file() {
                eprintln!("error: {}", msg::cannot_chmod(flip, &lossy(e.path())));
                code = 255;
            } else if e.mode != mode {
                flipped.push(pos);
            }
        }
        if !dry_run {
            for pos in flipped {
                if let Some(e) = index.entry_mut(pos) {
                    e.mode = mode;
                    let path = e.path().to_vec();
                    index.invalidate_path(&path);
                    changed = true;
                }
            }
        }
    }
    if dry_run {
        return Ok(code);
    }
    if changed {
        if let Some(reference) = index_mtime {
            let _ = smudge_racy_entries(&mut index, reference, &checker);
        }
        let path = repo.setup.cwd.join(repo.index_path());
        let skip_hash = repo.config.bool("index.skiphash").ok().flatten().unwrap_or(false);
        index
            .write_file(&path, &rgit_fs::LockOptions::default(), &WriteOptions { skip_hash })
            .map_err(|e| Die::Fatal(e.to_string()))?;
    }
    Ok(code)
}
