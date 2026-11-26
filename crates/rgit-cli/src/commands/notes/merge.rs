// SPDX-License-Identifier: MIT
//! `git notes merge` (Documentation/git-notes.adoc, "NOTES MERGE
//! STRATEGIES"; `notes.mergeStrategy`, `notes.<name>.mergeStrategy`).
//!
//! Observed behaviour of the `git` binary that the documentation leaves
//! open:
//!
//! - The verbosity starts at 2; `-q` lowers and `-v` raises it. `CONFLICT`
//!   lines need 1, the per-note lines, `Fast-forward` and `Already up to
//!   date.` 2, the abort and commit messages 3, the merge base and the
//!   merge result 4. All of them go to standard output.
//! - Every note changed on both sides (differently) is a conflict, even
//!   when its lines merge cleanly; `manual` writes the merged text (with
//!   conflict markers labelled with the ref names) or the surviving
//!   version to `NOTES_MERGE_WORKTREE/<object>`.
//! - A merge with conflicts records a partial merge commit (the local
//!   notes without the conflicting ones, message listing them under
//!   `Conflicts:`) in `NOTES_MERGE_PARTIAL` and the local ref in the
//!   symbolic ref `NOTES_MERGE_REF`; `--commit` writes a commit with the
//!   same parents and message from it and the files of the work tree.
//! - `--abort` empties `NOTES_MERGE_WORKTREE` (the directory stays) and
//!   removes the two refs.

use std::io::Write;
use std::path::{Path, PathBuf};

use rgit_diff::merge::{self as text_merge, Favor, MergeOptions};
use rgit_hash::ObjectId;
use rgit_notes::{Combine, Notes, refname};
use rgit_object::{Object, ObjectKind};
use rgit_refs::name::{RefNameFlags, check_refname_format};
use rgit_refs::{RefStore, RefUpdate, Transaction};

use crate::messages::notes as msg;
use crate::notes as support;
use crate::prelude::*;
use crate::refs_support::{self, Objects, UpdateHooks};
use crate::repository::Repo;

const WORKTREE: &str = "NOTES_MERGE_WORKTREE";
const PARTIAL: &str = "NOTES_MERGE_PARTIAL";
const MERGE_REF: &str = "NOTES_MERGE_REF";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Strategy {
    Manual,
    Ours,
    Theirs,
    Union,
    CatSortUniq,
}

impl Strategy {
    fn parse(s: &str) -> Option<Strategy> {
        Some(match s {
            "manual" => Strategy::Manual,
            "ours" => Strategy::Ours,
            "theirs" => Strategy::Theirs,
            "union" => Strategy::Union,
            "cat_sort_uniq" => Strategy::CatSortUniq,
            _ => return None,
        })
    }
}

fn fatal(s: impl Into<String>) -> Die {
    Die::Fatal(s.into())
}

/// Prints `line` to standard output when `verbosity` reaches `level`.
fn say(verbosity: i32, level: i32, line: &str) {
    if verbosity >= level {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }
}

pub fn run(ctx: &Context, repo: &mut Repo, local: &str, args: &[String]) -> CmdResult {
    let verbose = Cell::new(0);
    let quiet = Cell::new(0);
    let strategy: RefCell<Option<String>> = RefCell::new(None);
    let commit = Cell::new(false);
    let abort = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE_MERGE,
        vec![
            Opt::group(msg::GROUP_GENERAL),
            Opt::verbose(&verbose, msg::HELP_MERGE_VERBOSE),
            Opt::quiet(&quiet, msg::HELP_MERGE_QUIET),
            Opt::group(msg::GROUP_MERGE),
            Opt::string('s', "strategy", &strategy, "strategy", msg::HELP_STRATEGY),
            Opt::group(msg::GROUP_COMMIT),
            Opt::flag(None, "commit", &commit, msg::HELP_COMMIT).noneg(),
            Opt::group(msg::GROUP_ABORT),
            Opt::flag(None, "abort", &abort, msg::HELP_ABORT).noneg(),
        ],
    );
    let rest = parser.parse(args)?.args;
    let strategy = strategy.borrow().clone();
    let (commit, abort) = (commit.get(), abort.get());
    if (commit && abort) || ((commit || abort) && strategy.is_some()) {
        return Err(parser.error_with_usage(msg::CANNOT_MIX).into());
    }
    if !commit && !abort && rest.len() != 1 {
        return Err(parser.error_with_usage(msg::MUST_SPECIFY_REF).into());
    }
    if (commit || abort) && !rest.is_empty() {
        return Err(parser.error_with_usage(msg::TOO_MANY_ARGUMENTS).into());
    }
    let explicit = match &strategy {
        Some(s) => match Strategy::parse(s) {
            Some(s) => Some(s),
            None => {
                return Err(parser.error_with_usage(&msg::unknown_strategy_option(s)).into());
            }
        },
        None => None,
    };
    drop(parser);
    let verbosity = 2 + verbose.get() - quiet.get();
    let mut m = Merge { ctx, repo, verbosity };
    if abort {
        return m.abort();
    }
    if commit {
        return m.commit();
    }
    m.merge(local, &rest[0], explicit)
}

struct Merge<'a> {
    ctx: &'a Context,
    repo: &'a mut Repo,
    verbosity: i32,
}

impl Merge<'_> {
    /// `.git/NOTES_MERGE_WORKTREE` as git shows it, and its real path.
    fn worktree(&self) -> (String, PathBuf) {
        let shown = self.repo.setup.git_dir.join(WORKTREE);
        let real = self.repo.setup.cwd.join(&shown);
        (shown.display().to_string(), real)
    }

    fn store(&self) -> rgit_refs::files::FilesStore {
        refs_support::ref_store(&self.repo.setup, &self.repo.config)
    }

    fn update(&self, update: RefUpdate) -> Result<(), Die> {
        let store = self.store();
        let objects = Objects::open(&self.repo.setup);
        let mut hooks = UpdateHooks::new(&self.repo.setup, &self.repo.config, &objects);
        let name = update.name.clone();
        let mut tx = Transaction::new(&store);
        tx.push(update)
            .and_then(|()| tx.commit(&mut hooks))
            .map_err(|e| fatal(format!("update_ref failed for ref '{name}': {e}")))
    }

    /// Removes the files of the notes merge work tree (the directory
    /// stays); `false` when it does not exist.
    fn clear_worktree(&self) -> bool {
        let (_, real) = self.worktree();
        let Ok(entries) = std::fs::read_dir(&real) else {
            return false;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                let _ = std::fs::remove_dir_all(&p);
            } else {
                let _ = std::fs::remove_file(&p);
            }
        }
        true
    }

    fn delete_state_refs(&self) -> Result<(), Die> {
        let store = self.store();
        let hash = self.repo.hash;
        if store.read_raw(PARTIAL).ok().flatten().is_some() {
            self.update(RefUpdate::delete(PARTIAL, hash, None).no_deref(true))?;
        }
        if store.read_raw(MERGE_REF).ok().flatten().is_some() {
            self.update(RefUpdate::symref_delete(MERGE_REF, hash, None))?;
        }
        Ok(())
    }

    fn abort(&self) -> CmdResult {
        let (shown, _) = self.worktree();
        say(self.verbosity, 3, &msg::removing_worktree(&shown));
        let removed = self.clear_worktree();
        self.delete_state_refs()?;
        if !removed {
            eprintln!("error: {}", msg::REMOVE_WORKTREE_FAILED);
            return Ok(1);
        }
        Ok(0)
    }

    fn commit(&self) -> CmdResult {
        let store = self.store();
        let partial = match store.resolve_oid(PARTIAL).ok().flatten() {
            Some(id) => id,
            None => return Err(fatal(msg::READ_PARTIAL_FAILED)),
        };
        let Some(Object::Commit(c)) = self.repo.read_object(&partial) else {
            return Err(fatal(msg::READ_PARTIAL_FAILED));
        };
        let local = match store.read_raw(MERGE_REF).ok().flatten() {
            Some(rgit_refs::Target::Symbolic(name)) => name,
            _ => return Err(fatal("failed to resolve NOTES_MERGE_REF")),
        };
        let (shown, real) = self.worktree();
        say(self.verbosity, 3, &msg::committing_worktree(&shown));
        let odb = self.repo.odb();
        let tree = c.tree().ok();
        let mut notes = Notes::load(odb, tree.as_ref()).map_err(|e| fatal(e.to_string()))?;
        let hex_len = self.repo.hash.hex_len();
        if let Ok(entries) = std::fs::read_dir(&real) {
            let mut files: Vec<_> = entries.flatten().collect();
            files.sort_by_key(|e| e.file_name());
            for e in files {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.len() != hex_len {
                    continue;
                }
                let Ok(key) = ObjectId::from_hex(self.repo.hash, name.as_bytes()) else {
                    continue;
                };
                let data = std::fs::read(e.path()).map_err(|e| fatal(e.to_string()))?;
                let blob = odb
                    .write(ObjectKind::Blob, &data)
                    .map_err(|e| fatal(e.to_string()))?;
                notes.set(key, blob);
            }
        }
        let parents = c.parents().map_err(|_| fatal(msg::READ_PARTIAL_FAILED))?;
        let message = c.message().to_vec();
        let new = support::write_commit(odb, &self.repo.config, &notes, &parents, &message)?;
        let first_line = String::from_utf8_lossy(rgit_commit::message::first_line(&message)).into_owned();
        let old = parents.first().copied();
        self.update(
            RefUpdate::update(local.as_str(), new, old).message(format!("notes: {first_line}")),
        )?;
        say(self.verbosity, 3, &msg::removing_worktree(&shown));
        self.clear_worktree();
        self.delete_state_refs()?;
        Ok(0)
    }

    /// The strategy from the configuration (`notes.<name>.mergeStrategy`,
    /// then `notes.mergeStrategy`).
    fn configured_strategy(&self, local: &str) -> Result<Strategy, Die> {
        let short = local.strip_prefix(refname::PREFIX).unwrap_or(local);
        let specific = format!("notes.{short}.mergestrategy").to_ascii_lowercase();
        let mut general = None;
        let mut per_ref = None;
        for e in self.repo.config.entries() {
            let name = e.name.to_string().to_ascii_lowercase();
            let is_general = name == "notes.mergestrategy";
            let is_specific = name == specific;
            if !is_general && !is_specific {
                continue;
            }
            let value = e
                .value
                .as_ref()
                .map(|v| String::from_utf8_lossy(v).into_owned())
                .unwrap_or_default();
            let Some(s) = Strategy::parse(&value) else {
                // git names the variable in its documented spelling.
                let shown = if is_specific {
                    format!("notes.{short}.mergeStrategy")
                } else {
                    "notes.mergeStrategy".to_owned()
                };
                eprintln!("error: {}", msg::unknown_strategy(&value));
                return Err(fatal(match &e.source.origin {
                    rgit_config::Origin::File(p) => crate::messages::bad_config_variable(
                        &shown,
                        &p.display().to_string(),
                        e.line,
                    ),
                    rgit_config::Origin::CommandLine => {
                        crate::messages::unable_to_parse_config(&shown, "command-line config")
                    }
                    other => crate::messages::unable_to_parse_config(&shown, &other.describe()),
                }));
            };
            if is_specific {
                per_ref = Some(s);
            } else {
                general = Some(s);
            }
        }
        Ok(per_ref.or(general).unwrap_or(Strategy::Manual))
    }

    fn merge(&mut self, local: &str, remote_arg: &str, explicit: Option<Strategy>) -> CmdResult {
        if !local.starts_with(refname::PREFIX) {
            return Err(fatal(msg::refusing_merge(local)));
        }
        if check_refname_format(local.as_bytes(), RefNameFlags::default()).is_err() {
            return Err(fatal(msg::failed_local_ref(local)));
        }
        let configured = self.configured_strategy(local)?;
        let strategy = explicit.unwrap_or(configured);
        self.check_other_worktrees(local)?;

        let store = self.store();
        let local_oid = store.resolve_oid(local).ok().flatten();
        let remote = if self.repo.resolve(remote_arg).is_some() {
            remote_arg.to_owned()
        } else {
            refname::expand(remote_arg)
        };
        let remote_oid = self.repo.resolve(&remote);
        let message = msg::merged_notes(&remote, local);
        let reflog = format!("notes: {message}");
        let (local_oid, remote_oid) = match (local_oid, remote_oid) {
            (None, None) => return Err(fatal(msg::cannot_merge_empty(&remote, local))),
            (_, None) => return Ok(0),
            (None, Some(r)) => {
                self.update(RefUpdate::update(local, r, None).message(reflog))?;
                return Ok(0);
            }
            (Some(l), Some(r)) => (l, r),
        };
        let bases = crate::diff::merge_bases(self.ctx, &local_oid, &remote_oid)?;
        let short = |id: &ObjectId| id.to_hex()[..7].to_owned();
        let base = bases.first().copied();
        match bases.len() {
            0 => say(self.verbosity, 4, msg::NO_MERGE_BASE),
            1 => say(self.verbosity, 4, &msg::one_merge_base(&short(&bases[0]))),
            _ => say(self.verbosity, 4, &msg::multiple_merge_bases(&short(&bases[0]))),
        }
        let null = ObjectId::null(self.repo.hash);
        say(
            self.verbosity,
            4,
            &msg::merging_remote(&short(&remote_oid), &short(&local_oid), &short(&base.unwrap_or(null))),
        );
        if base == Some(remote_oid) {
            say(self.verbosity, 2, msg::UP_TO_DATE);
            return Ok(0);
        }
        if base == Some(local_oid) {
            say(self.verbosity, 2, msg::FAST_FORWARD);
            self.update(RefUpdate::update(local, remote_oid, Some(local_oid)).message(reflog))?;
            return Ok(0);
        }

        let odb = self.repo.odb();
        let load = |id: Option<ObjectId>| -> Result<Notes, Die> {
            let tree = id.and_then(|id| support::notes_tree(odb, &id));
            Notes::load(odb, tree.as_ref()).map_err(|e| fatal(e.to_string()))
        };
        let base_notes = load(base)?;
        let mut result = load(Some(local_oid))?;
        let remote_notes = load(Some(remote_oid))?;

        // The notes changed on the remote side since the base.
        let mut changes: Vec<(ObjectId, Option<ObjectId>, Option<ObjectId>)> = Vec::new();
        {
            let b = base_notes.list();
            let r = remote_notes.list();
            let (mut i, mut j) = (0, 0);
            while i < b.len() || j < r.len() {
                let next = match (b.get(i), r.get(j)) {
                    (Some(x), Some(y)) if x.0 == y.0 => {
                        i += 1;
                        j += 1;
                        (x.0, Some(x.1), Some(y.1))
                    }
                    (Some(x), Some(y)) if x.0 < y.0 => {
                        i += 1;
                        (x.0, Some(x.1), None)
                    }
                    (Some(x), None) => {
                        i += 1;
                        (x.0, Some(x.1), None)
                    }
                    (_, Some(y)) => {
                        j += 1;
                        (y.0, None, Some(y.1))
                    }
                    (None, None) => unreachable!(),
                };
                if next.1 != next.2 {
                    changes.push(next);
                }
            }
        }

        let mut conflicts: Vec<(ObjectId, Vec<u8>)> = Vec::new();
        let mut changed = false;
        let style = crate::commands::merge_file::config_style(&self.repo.config)?;
        for (obj, b, r) in changes {
            let l = result.get(&obj);
            if l == r {
                continue;
            }
            if l == b {
                match r {
                    Some(r) => result.set(obj, r),
                    None => {
                        result.remove(&obj);
                    }
                }
                changed = true;
                continue;
            }
            let hex = obj.to_hex();
            match strategy {
                Strategy::Manual => {
                    say(self.verbosity, 2, &msg::auto_merging(&hex));
                    if l.is_none() && r.is_none() {
                        continue;
                    }
                    // An unfinished merge is found before the first
                    // conflict is reported.
                    if conflicts.is_empty() {
                        self.check_worktree_free()?;
                    }
                    let text = match (l, r) {
                        (Some(l), Some(r)) => {
                            if b.is_none() {
                                say(self.verbosity, 1, &msg::conflict_add_add(&hex));
                            } else {
                                say(self.verbosity, 1, &msg::conflict_content(&hex));
                            }
                            let read = |id: Option<ObjectId>| {
                                id.and_then(|id| support::blob(odb, &id)).unwrap_or_default()
                            };
                            let opts = MergeOptions {
                                style,
                                favor: Favor::None,
                                marker_size: 0,
                                ours_label: local.as_bytes(),
                                base_label: b"",
                                theirs_label: remote.as_bytes(),
                                diff: text_merge::merge_diff_options(),
                            };
                            text_merge::merge(&read(b), &read(Some(l)), &read(Some(r)), &opts)
                                .map(|m| m.content)
                                .unwrap_or_default()
                        }
                        (None, Some(r)) => {
                            say(self.verbosity, 1, &msg::conflict_delete_modify(&hex, local, &remote));
                            support::blob(odb, &r).unwrap_or_default()
                        }
                        (Some(l), None) => {
                            say(self.verbosity, 1, &msg::conflict_delete_modify(&hex, &remote, local));
                            support::blob(odb, &l).unwrap_or_default()
                        }
                        (None, None) => continue,
                    };
                    self.write_conflict(&obj, &text)?;
                    if result.remove(&obj).is_some() {
                        changed = true;
                    }
                    conflicts.push((obj, text));
                }
                Strategy::Ours => {
                    say(self.verbosity, 2, &msg::using_local(&hex));
                }
                Strategy::Theirs => {
                    say(self.verbosity, 2, &msg::using_remote(&hex));
                    match r {
                        Some(r) => result.set(obj, r),
                        None => {
                            result.remove(&obj);
                        }
                    }
                    changed = true;
                }
                Strategy::Union | Strategy::CatSortUniq => {
                    let (line, combine) = if strategy == Strategy::Union {
                        (msg::concatenating(&hex), Combine::Concatenate)
                    } else {
                        (msg::concatenating_unique(&hex), Combine::CatSortUniq)
                    };
                    say(self.verbosity, 2, &line);
                    if let Some(r) = r {
                        result
                            .add(odb, obj, r, combine)
                            .map_err(|e| fatal(e.to_string()))?;
                        changed = true;
                    }
                }
            }
        }
        say(self.verbosity, 4, &msg::merge_result(conflicts.len(), !changed));

        let mut text = message.clone().into_bytes();
        if !conflicts.is_empty() {
            text.extend_from_slice(b"\n\nConflicts:\n");
            for (obj, _) in &conflicts {
                text.extend_from_slice(b"\t");
                text.extend_from_slice(obj.to_hex().as_bytes());
                text.push(b'\n');
            }
        }
        let new = support::write_commit(odb, &self.repo.config, &result, &[local_oid, remote_oid], &text)?;
        if conflicts.is_empty() {
            self.update(RefUpdate::update(local, new, Some(local_oid)).message(reflog))?;
            return Ok(0);
        }
        self.update(RefUpdate::update(PARTIAL, new, None).no_deref(true))?;
        self.update(RefUpdate::symref_update(MERGE_REF, local, None).no_deref(true))?;
        let (shown, _) = self.worktree();
        eprintln!("{}", msg::automatic_merge_failed(&shown));
        Ok(1)
    }

    /// Dies when another worktree is in the middle of a notes merge into
    /// `local`.
    fn check_other_worktrees(&self, local: &str) -> Result<(), Die> {
        for wt in crate::worktrees::list(&self.repo.setup, false) {
            if wt.current {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(wt.git_dir.join(MERGE_REF)) else {
                continue;
            };
            if text.trim_end().strip_prefix("ref: ") == Some(local) {
                return Err(fatal(msg::already_in_progress(local, &wt.path.display().to_string())));
            }
        }
        Ok(())
    }

    /// Dies when an earlier merge left files in the work tree.
    fn check_worktree_free(&self) -> Result<(), Die> {
        let (_, real) = self.worktree();
        let busy = std::fs::read_dir(&real).is_ok_and(|mut d| d.next().is_some());
        if busy {
            let what = self.repo.setup.git_dir.join("NOTES_MERGE_*");
            return Err(fatal(msg::worktree_exists(&what.display().to_string())));
        }
        Ok(())
    }

    fn write_conflict(&self, obj: &ObjectId, text: &[u8]) -> Result<(), Die> {
        let (_, real) = self.worktree();
        std::fs::create_dir_all(&real).map_err(|e| fatal(e.to_string()))?;
        let path: &Path = &real.join(obj.to_hex());
        std::fs::write(path, text).map_err(|e| fatal(e.to_string()))
    }
}
