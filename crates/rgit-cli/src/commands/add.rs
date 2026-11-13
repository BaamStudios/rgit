// SPDX-License-Identifier: MIT
//! `git add` (Documentation/git-add.adoc), first slice: new and modified
//! files and removals under the given pathspecs, `-A`, `-u`, `-n`, `-v`,
//! `-f`. Ignore rules come from rgit-worktree, the index is written with
//! racy entries smudged.
//!
//! Not done yet: eol/filter attributes (a warning is printed instead, see
//! issue #107), `--intent-to-add`, `--chmod`, `--renormalize`,
//! `--ignore-errors`, interactive modes, untracked nested repositories.

use std::path::{Path, PathBuf};

use rgit_index::{AddOptions, Entry, Index, Stage, WriteOptions};
use rgit_object::ObjectKind;
use rgit_pathspec::{DirInterest, MatchKind, ParseOptions, Pathspec};
use rgit_worktree::ignore::{Excludes, default_excludes_file};
use rgit_worktree::stat::{
    Checker, Status, mode_from_metadata, smudge_racy_entries, stat_from_metadata,
};

use crate::messages::add as msg;
use crate::prelude::*;
use crate::repo::Mode;
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
/// its stage-0 entry.
pub(crate) fn entry_for(
    repo: &Repo,
    checker: &Checker,
    root: &Path,
    path: &[u8],
    old: Option<&Entry>,
    dry_run: bool,
) -> Result<Option<Entry>, Die> {
    let shown = lossy(path);
    let Some(meta) = checker
        .lstat(path)
        .map_err(|e| Die::Fatal(format!("lstat(\"{shown}\"): {e}")))?
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
            .map_err(|e| Die::Fatal(format!("readlink(\"{shown}\"): {e}")))?
    } else {
        std::fs::read(&full).map_err(|e| Die::Fatal(format!("open(\"{shown}\"): {e}")))?
    };
    let id = if dry_run {
        rgit_hash::hash_object(repo.hash, "blob", &data).map_err(|e| Die::Fatal(e.to_string()))?
    } else {
        repo.odb()
            .write(ObjectKind::Blob, &data)
            .map_err(|e| Die::Fatal(e.to_string()))?
    };
    let mut entry = Entry::new(path.to_vec(), Stage::MERGED, mode, id);
    entry.stat = stat_from_metadata(&meta);
    if let Some(o) = old {
        entry.flags = o.flags;
        entry.flags.intent_to_add = false;
    }
    Ok(Some(entry))
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let dry_run = Cell::new(false);
    let verbose = Cell::new(false);
    let force = Cell::new(false);
    let update = Cell::new(false);
    let all = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag('n', "dry-run", &dry_run, msg::HELP_DRY_RUN),
            Opt::flag('v', "verbose", &verbose, msg::HELP_VERBOSE),
            Opt::group(""),
            Opt::flag('f', "force", &force, msg::HELP_FORCE),
            Opt::flag('u', "update", &update, msg::HELP_UPDATE),
            Opt::flag('A', "all", &all, msg::HELP_ALL),
        ],
    );
    let args = parser.parse(argv)?.args;
    drop(parser);
    let (dry_run, verbose, force, update, all) =
        (dry_run.get(), verbose.get(), force.get(), update.get(), all.get());
    if update && all {
        return Err(Die::Fatal(msg::UPDATE_AND_ALL.to_string()));
    }

    let repo = Repo::open(ctx, Mode::Required)?;
    let root = repo.work_tree()?;
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

    // Tracked files first, in index order.
    let mut lines: Vec<String> = Vec::new();
    let mut updates: Vec<Entry> = Vec::new();
    let mut removals: Vec<Vec<u8>> = Vec::new();
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
        let status = checker
            .check(e)
            .map_err(|err| Die::Fatal(format!("{}: {err}", lossy(e.path()))))?;
        match status {
            Status::Unchanged => {}
            Status::StatOnly => {
                if let Some(n) = entry_for(&repo, &checker, &root, e.path(), Some(e), true)? {
                    updates.push(n);
                }
            }
            Status::Modified | Status::TypeChanged => {
                lines.push(msg::add(&lossy(e.path())));
                if let Some(n) = entry_for(&repo, &checker, &root, e.path(), Some(e), dry_run)? {
                    if warn_attrs {
                        eprintln!("warning: {}", msg::attributes_not_applied(&lossy(e.path())));
                    }
                    updates.push(n);
                }
            }
            Status::Deleted => {
                lines.push(msg::remove(&lossy(e.path())));
                removals.push(e.path().to_vec());
            }
        }
    }

    // Then untracked files in directory order.
    let mut found = Vec::new();
    let mut ignored = Vec::new();
    if !update {
        let mut walk = Walk {
            root: root.clone(),
            index: &index,
            pathspec: &pathspec,
            excludes: build_excludes(&repo, &root),
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
    }
    for item in pathspec.unmatched(&seen) {
        if ignored.iter().any(|p| p.as_slice() == item.original()) {
            continue;
        }
        return Err(Die::Fatal(rgit_pathspec::messages::did_not_match_any_files(
            item.original(),
        )));
    }

    for path in &found {
        lines.push(msg::add(&lossy(path)));
        if warn_attrs {
            eprintln!("warning: {}", msg::attributes_not_applied(&lossy(path)));
        }
        if let Some(n) = entry_for(&repo, &checker, &root, path, None, dry_run)? {
            updates.push(n);
        }
    }
    if verbose || dry_run {
        let mut out = String::new();
        for l in &lines {
            out.push_str(l);
            out.push('\n');
        }
        print!("{out}");
    }
    let code = if ignored.is_empty() {
        0
    } else {
        eprintln!("{}", msg::IGNORED_HEADER);
        for p in &ignored {
            eprintln!("{}", lossy(p));
        }
        eprintln!("{}", msg::IGNORED_HINT);
        1
    };
    if dry_run {
        return Ok(code);
    }

    let changed = !updates.is_empty() || !removals.is_empty();
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
