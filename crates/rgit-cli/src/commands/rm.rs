// SPDX-License-Identifier: MIT
//! `git rm` (Documentation/git-rm.adoc), basic slice: remove index entries
//! matching the pathspecs and (without `--cached`) their work tree files,
//! with the up-to-date checks against HEAD and the work tree. Options:
//! `--cached`, `-r`, `-f`, `-n`, `-q`, `--ignore-unmatch`.
//!
//! Submodules: a populated submodule counts as locally modified when it
//! has new commits, modified or untracked content. Without `--cached` its
//! directory is removed (an embedded `.git` directory is first moved to
//! `$GIT_DIR/modules/<name>`), and its section in `.gitmodules` is removed
//! and staged.
//!
//! `--pathspec-from-file`/`--pathspec-file-nul`; `--sparse` is accepted
//! (sparse checkouts are not supported yet).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rgit_hash::ObjectId;
use rgit_index::{Stage, WriteOptions};
use rgit_object::{Object, ObjectKind};
use rgit_pathspec::{MatchKind, ParseOptions, Pathspec};
use rgit_worktree::stat::{Status, smudge_racy_entries};

use crate::messages::rm as msg;
use crate::messages::rm::Problem;
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

/// `ENOENT` or `ENOTDIR`: the path does not exist in the work tree.
fn is_missing(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

fn flatten_tree(repo: &Repo, tree: &ObjectId, base: &[u8], out: &mut HashMap<Vec<u8>, ObjectId>) {
    let Some(Object::Tree(t)) = repo.read_object(tree) else {
        return;
    };
    for e in t.entries {
        let mut path = base.to_vec();
        path.extend_from_slice(&e.name);
        if e.mode.canonical().is_tree() {
            path.push(b'/');
            flatten_tree(repo, &e.oid, &path, out);
        } else {
            out.insert(path, e.oid);
        }
    }
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let dry_run = Cell::new(false);
    let quiet = Cell::new(false);
    let cached = Cell::new(false);
    let force = Cell::new(false);
    let recursive = Cell::new(false);
    let ignore_unmatch = Cell::new(false);
    let sparse = Cell::new(false);
    let from_file = RefCell::new(None::<String>);
    let file_nul = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag('n', "dry-run", &dry_run, msg::HELP_DRY_RUN),
            Opt::flag('q', "quiet", &quiet, msg::HELP_QUIET),
            Opt::flag(None, "cached", &cached, msg::HELP_CACHED),
            Opt::flag('f', "force", &force, msg::HELP_FORCE),
            Opt::flag('r', None, &recursive, msg::HELP_R),
            Opt::flag(None, "ignore-unmatch", &ignore_unmatch, msg::HELP_IGNORE_UNMATCH),
            Opt::flag(None, "sparse", &sparse, msg::HELP_SPARSE),
            Opt::string(
                None,
                "pathspec-from-file",
                &from_file,
                "file",
                crate::messages::reset::HELP_PATHSPEC_FROM_FILE,
            ),
            Opt::flag(
                None,
                "pathspec-file-nul",
                &file_nul,
                crate::messages::reset::HELP_PATHSPEC_FILE_NUL,
            ),
        ],
    );
    let mut args = parser.parse(argv)?.args;
    drop(parser);
    let from_file = from_file.into_inner();
    if from_file.is_some() && !args.is_empty() {
        return Err(Die::Fatal(crate::messages::reset::FROM_FILE_WITH_ARGS.to_owned()));
    }
    if file_nul.get() && from_file.is_none() {
        return Err(Die::Fatal(
            crate::messages::reset::FILE_NUL_WITHOUT_FROM_FILE.to_owned(),
        ));
    }
    let (dry_run, quiet, cached, force, recursive, ignore_unmatch) = (
        dry_run.get(),
        quiet.get(),
        cached.get(),
        force.get(),
        recursive.get(),
        ignore_unmatch.get(),
    );

    let mut repo = Repo::open(ctx, Mode::Required)?;
    let advisor = ctx.advisor(&repo.config)?;
    if let Some(name) = &from_file {
        args = super::restore::read_pathspec_file(&repo, name, file_nul.get())?
            .into_iter()
            .map(|p| String::from_utf8_lossy(&p).into_owned())
            .collect();
    }
    if args.is_empty() {
        return Err(Die::Fatal(msg::NO_PATHSPEC.to_string()));
    }
    let root = repo.work_tree()?;
    let opts = ParseOptions {
        prefix: repo.prefix(),
        root: Some(root.as_os_str().as_encoded_bytes().to_vec()),
        globals: repository::pathspec_globals(ctx),
        ..Default::default()
    };
    let pathspec = Pathspec::parse(&args, &opts).map_err(|e| Die::Fatal(e.to_string()))?;
    let (mut index, index_mtime) = repo.read_index()?;
    let checker = repo.checker(index_mtime)?;

    let mut seen = vec![false; pathspec.items().len()];
    let mut chosen: Vec<Vec<u8>> = Vec::new();
    for e in index.entries() {
        let mut one = vec![false; seen.len()];
        let mut kind = pathspec.match_path_seen(e.path(), &mut one);
        if kind.is_none() && e.mode.is_gitlink() {
            // `submod/` names the submodule itself.
            let mut dir = e.path().to_vec();
            dir.push(b'/');
            kind = pathspec.match_path_seen(&dir, &mut one).map(|_| MatchKind::Exact);
        }
        let Some(kind) = kind else {
            continue;
        };
        if kind == MatchKind::Recursive && !recursive {
            let item = one
                .iter()
                .position(|s| *s)
                .map(|i| pathspec.items()[i].original().to_vec())
                .unwrap_or_default();
            return Err(Die::Fatal(msg::not_recursive(&lossy(&item))));
        }
        for (s, o) in seen.iter_mut().zip(one) {
            *s |= o;
        }
        if chosen.last().map(Vec::as_slice) != Some(e.path()) {
            chosen.push(e.path().to_vec());
        }
    }
    if !ignore_unmatch {
        if let Some(item) = pathspec.unmatched(&seen).next() {
            return Err(Die::Fatal(rgit_pathspec::messages::did_not_match_any_files(
                item.original(),
            )));
        }
    }

    // Submodules to remove from the work tree: an embedded repository
    // needs its name from `.gitmodules` to be moved away first.
    let gitlinks: Vec<Vec<u8>> = chosen
        .iter()
        .filter(|p| index.entries_for_path(p).iter().any(|e| e.mode.is_gitlink()))
        .cloned()
        .collect();
    // Removing a submodule edits `.gitmodules`; unstaged changes there
    // would be staged with it.
    if !gitlinks.is_empty() {
        if let Some(e) = index.get(rgit_submodule::GITMODULES.as_bytes(), Stage::MERGED) {
            if std::fs::symlink_metadata(root.join(rgit_submodule::GITMODULES)).is_ok()
                && matches!(
                    checker.check(e),
                    Ok(Status::Modified | Status::TypeChanged)
                )
            {
                return Err(Die::Fatal(msg::GITMODULES_UNSTAGED.to_owned()));
            }
        }
    }
    let gitmodules = if gitlinks.is_empty() || cached {
        None
    } else {
        Some(crate::submodules::load_gitmodules(&repo, Some(&index))?)
    };
    if let Some(gm) = &gitmodules {
        for p in &gitlinks {
            let dot_git = os_path(&root, p).join(".git");
            if std::fs::symlink_metadata(&dot_git).is_ok_and(|m| m.is_dir())
                && gm.by_path(p).is_none()
            {
                return Err(Die::Fatal(crate::messages::submodule::no_name_for(&lossy(p))));
            }
        }
        // Embedded git directories move to `.git/modules` first (also when
        // a check below fails).
        for p in &gitlinks {
            if let Some(sub) = gm.by_path(p) {
                crate::submodules::absorb_git_dir(&repo, &os_path(&root, p), &sub.name, Some(p))?;
            }
        }
    }
    if !force {
        let mut head = HashMap::new();
        if let Some(id) = repo.resolve("HEAD") {
            if let Some(tree) = repo.peel_to(id, ObjectKind::Tree) {
                flatten_tree(&repo, &tree, b"", &mut head);
            }
        }
        let have_head = repo.resolve("HEAD").is_some();
        let mut problems: [(Problem, Vec<String>); 3] = [
            (Problem::Both, Vec::new()),
            (Problem::Staged, Vec::new()),
            (Problem::Local, Vec::new()),
        ];
        for path in &chosen {
            let entries = index.entries_for_path(path);
            let Some(e) = entries.iter().find(|e| e.stage() == Stage::MERGED) else {
                // Unmerged paths are removed without checks, except that a
                // populated submodule must match our side.
                let ours = entries
                    .iter()
                    .find(|e| e.stage() == Stage::OURS)
                    .or_else(|| entries.first());
                if let Some(e) = ours.filter(|e| e.mode.is_gitlink()) {
                    if !cached
                        && crate::submodules::worktree_state(
                            &os_path(&root, path),
                            &e.id,
                            repo.hash,
                            rgit_submodule::Ignore::None,
                            true,
                        )
                        .changed()
                    {
                        problems[2].1.push(lossy(path));
                    }
                }
                continue;
            };
            // Nothing is staged for an intent-to-add entry.
            if cached && e.flags.intent_to_add {
                continue;
            }
            // A file already gone from the work tree (also when a leading
            // directory became a file or a dangling symlink) is removed
            // without checks.
            if !e.mode.is_gitlink() {
                if let Err(err) = std::fs::symlink_metadata(os_path(&root, path)) {
                    if is_missing(&err) {
                        continue;
                    }
                }
            }
            let staged = have_head && head.get(path.as_slice()) != Some(&e.id);
            let local = if e.mode.is_gitlink() {
                crate::submodules::worktree_state(
                    &os_path(&root, path),
                    &e.id,
                    repo.hash,
                    rgit_submodule::Ignore::None,
                    true,
                )
                .changed()
            } else {
                match checker
                .check(e)
                .map_err(|err| Die::Fatal(format!("{}: {err}", lossy(path))))?
            {
                Status::Unchanged | Status::StatOnly | Status::Deleted => false,
                Status::Modified | Status::TypeChanged => true,
                }
            };
            let kind = if staged && local {
                Some(Problem::Both)
            } else if cached {
                None
            } else if staged {
                Some(Problem::Staged)
            } else if local {
                Some(Problem::Local)
            } else {
                None
            };
            if let Some(k) = kind {
                let slot = problems.iter_mut().find(|(p, _)| *p == k).expect("all kinds");
                slot.1.push(lossy(path));
            }
        }
        let mut failed = false;
        for (k, paths) in &problems {
            if !paths.is_empty() {
                let hints = advisor.enabled(Advice::RmHints).map_err(Die::Fatal)?;
                eprintln!("error: {}", msg::problem(*k, paths, cached, hints));
                failed = true;
            }
        }
        if failed {
            return Ok(1);
        }
    }

    if !quiet {
        let mut out = String::new();
        for p in &chosen {
            out.push_str(&msg::rm(&lossy(p)));
            out.push('\n');
        }
        print!("{out}");
    }
    if dry_run || chosen.is_empty() {
        return Ok(0);
    }
    for p in &chosen {
        // Removing a conflict keeps its stages for `restore --merge`.
        let entries = index.entries_for_path(p);
        if entries.iter().any(|e| e.stage().is_conflict()) {
            let mut stages = [None; 3];
            for e in entries {
                if e.stage().is_conflict() {
                    stages[usize::from(e.stage().get()) - 1] = Some((e.mode.0, e.id));
                }
            }
            index.record_resolve_undo(p, stages);
        }
        index.remove_path(p);
    }
    let cwd = std::fs::canonicalize(os_path(&root, &repo.prefix())).ok();
    // `.gitmodules` removed in the same call stays removed: its sections are
    // not edited and it is not staged again.
    let gitmodules_removed = chosen
        .iter()
        .any(|p| p.as_slice() == rgit_submodule::GITMODULES.as_bytes());
    if let Some(gm) = &gitmodules {
        let mut changed = false;
        for p in &gitlinks {
            let dir = os_path(&root, p);
            if let Some(sub) = gm.by_path(p) {
                crate::submodules::absorb_git_dir(&repo, &dir, &sub.name, None)?;
                if !gitmodules_removed {
                    changed |= crate::submodules::remove_section(&repo, &sub.name)?;
                }
            } else if root.join(rgit_submodule::GITMODULES).exists() {
                eprintln!("warning: {}", msg::no_gitmodules_section(&lossy(p)));
            }
            if dir.join(".git").exists() {
                let _ = std::fs::remove_dir_all(&dir);
            } else {
                let _ = std::fs::remove_dir(&dir);
            }
        }
        if changed {
            crate::submodules::stage_gitmodules(&repo, &mut index)?;
        }
    }
    if !cached {
        for p in &chosen {
            let full = os_path(&root, p);
            let removed = match std::fs::remove_file(&full) {
                Ok(()) => true,
                Err(err) if is_missing(&err) => false,
                // A gitlink's directory was handled above.
                Err(_) if std::fs::symlink_metadata(&full).is_ok_and(|m| m.is_dir()) => false,
                // git stops at the first failure and keeps the index.
                Err(err) => {
                    return Err(Die::Fatal(msg::unlink_failed(
                        &lossy(p),
                        &crate::global::strerror(&err),
                    )));
                }
            };
            if removed {
                // Remove now-empty leading directories.
                let mut dir = full.parent().map(Path::to_path_buf);
                while let Some(d) = dir {
                    if d == root || std::fs::canonicalize(&d).ok() == cwd || std::fs::remove_dir(&d).is_err() {
                        break;
                    }
                    dir = d.parent().map(Path::to_path_buf);
                }
            }
        }
    }
    if let Some(reference) = index_mtime {
        let _ = smudge_racy_entries(&mut index, reference, &checker);
    }
    let path = repo.setup.cwd.join(repo.index_path());
    let skip_hash = repo.config.bool("index.skiphash").ok().flatten().unwrap_or(false);
    index
        .write_file(&path, &repo.index_lock_options(), &WriteOptions { skip_hash })
        .map_err(|e| Die::Fatal(e.to_string()))?;
    Ok(0)
}
