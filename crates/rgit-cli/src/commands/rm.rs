// SPDX-License-Identifier: MIT
//! `git rm` (Documentation/git-rm.adoc), basic slice: remove index entries
//! matching the pathspecs and (without `--cached`) their work tree files,
//! with the up-to-date checks against HEAD and the work tree. Options:
//! `--cached`, `-r`, `-f`, `-n`, `-q`, `--ignore-unmatch`.
//!
//! Not done yet: `--pathspec-from-file`, `--sparse`, submodules (gitlinks
//! are removed from the index only), `.gitmodules` updates.

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
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag('n', "dry-run", &dry_run, msg::HELP_DRY_RUN),
            Opt::flag('q', "quiet", &quiet, msg::HELP_QUIET),
            Opt::flag(None, "cached", &cached, msg::HELP_CACHED),
            Opt::flag('f', "force", &force, msg::HELP_FORCE),
            Opt::flag('r', None, &recursive, msg::HELP_R),
            Opt::flag(None, "ignore-unmatch", &ignore_unmatch, msg::HELP_IGNORE_UNMATCH),
        ],
    );
    let args = parser.parse(argv)?.args;
    drop(parser);
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
        let Some(kind) = pathspec.match_path_seen(e.path(), &mut one) else {
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
                continue; // unmerged paths are removed without checks
            };
            if e.mode.is_gitlink() {
                continue;
            }
            let staged = have_head && head.get(path.as_slice()) != Some(&e.id);
            let local = match checker
                .check(e)
                .map_err(|err| Die::Fatal(format!("{}: {err}", lossy(path))))?
            {
                Status::Unchanged | Status::StatOnly | Status::Deleted => false,
                Status::Modified | Status::TypeChanged => true,
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
        index.remove_path(p);
    }
    let cwd = std::fs::canonicalize(os_path(&root, &repo.prefix())).ok();
    if !cached {
        for p in &chosen {
            let full = os_path(&root, p);
            if std::fs::remove_file(&full).is_ok() {
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
