// SPDX-License-Identifier: MIT
//! `git apply` (Documentation/git-apply.adoc): apply patches to the work
//! tree and/or the index. Parsing and hunk matching live in `rgit-apply`;
//! this file reads the inputs, checks every patch against the files, and
//! writes the results.
//!
//! Behaviour determined against the reference git (spec gaps, issue #149):
//!
//! - Every input file is parsed, checked and written on its own; the first
//!   input that fails stops the command. The index is written once at the
//!   end, and only when nothing failed.
//! - Within one input, all patches are checked before anything is written.
//!   Writing removes files (deletions, rename sources) first and creates
//!   or updates files afterwards, in patch order.
//! - A patch that modifies or deletes a path changed by an earlier patch
//!   of the same input works on that result; renames and copies always
//!   read the original file. A path renamed away or deleted earlier cannot
//!   be modified again.
//! - A new file may take the place of a file that the input deletes or
//!   renames away.
//! - `-R` reverses each patch and also their order.
//! - From a subdirectory, traditional patches are relative to it, git
//!   patches to the top; git patches outside the subdirectory are skipped.
//! - `--include`/`--exclude` match the whole path with `*` matching `/`;
//!   the first matching pattern decides.
//! - `--reject` implies `--verbose` (unless `--quiet`).
//! - `--build-fake-ancestor` writes the index file and applies nothing.
//!
//! Binary patches (`GIT binary patch`, literal and delta) are applied
//! when the preimage has the object id of the `index` line, and so must
//! the result; without data the new blob must be in the repository.
//!
//! `--3way` merges from the preimage blob of the `index` line (labels
//! `ours`/`theirs`, `merge.conflictStyle`, `--ours`/`--theirs`/`--union`),
//! leaving conflicts in index stages 1-3; without the blob, and for
//! created or deleted files, the patch is applied directly.
//!
//! Whitespace errors of added lines (`core.whitespace`, the `whitespace`
//! attribute) are reported, fixed or fatal as `--whitespace` /
//! `apply.whitespace` say; `--ignore-space-change` /
//! `apply.ignoreWhitespace=change` match context lines regardless of the
//! amount of whitespace.
//!
//! Submodules (gitlinks) are `Subproject commit <id>` texts: the preimage
//! comes from the index (else the submodule's `HEAD`), the result goes
//! into the index; their directories are only removed when empty.
//! `--inaccurate-eof` drops the final line feed of the last lines of the
//! last hunk.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use rgit_apply::{ApplyOptions, FilePatch, HunkResult, LineKind, ParseOptions};
use rgit_index::{AddOptions, Entry, Index, Mode as IndexMode, Stage, WriteOptions};
use rgit_object::ObjectKind;
use rgit_worktree::stat::{Checker, Status, stat_from_metadata};

use crate::messages::apply as msg;
use crate::prelude::*;
use crate::repository::Repo;

const S_IFMT: u32 = 0o170000;
const S_IFLNK: u32 = 0o120000;
const S_IFGITLINK: u32 = 0o160000;

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

/// The mode git records for a patch mode: symbolic link, gitlink, or a
/// regular file that is executable when any owner execute bit is set.
/// `--inaccurate-eof` (behaviour of the `git` binary): the last lines of
/// the last hunk, on both sides, are taken to have no final line feed,
/// whatever the patch says.
fn inaccurate_eof(hunks: &[rgit_apply::Hunk]) -> Vec<rgit_apply::Hunk> {
    let mut hunks = hunks.to_vec();
    if let Some(h) = hunks.last_mut() {
        let pre = h.lines.iter().rposition(|l| l.kind != LineKind::Add);
        let post = h.lines.iter().rposition(|l| l.kind != LineKind::Remove);
        for k in [pre, post].into_iter().flatten() {
            if h.lines[k].text.ends_with(b"\n") {
                h.lines[k].text.pop();
            }
        }
    }
    hunks
}

/// Mode of a submodule (gitlink).
const GITLINK: u32 = 0o160000;

/// The contents of a gitlink in a patch.
fn gitlink_text(id: &rgit_hash::ObjectId) -> Vec<u8> {
    format!("Subproject commit {}\n", id.to_hex()).into_bytes()
}

/// The commit named by gitlink contents.
fn gitlink_id(hash: rgit_hash::HashKind, data: &[u8]) -> Option<rgit_hash::ObjectId> {
    let hex = data.strip_prefix(b"Subproject commit ")?;
    let hex = hex.strip_suffix(b"\n").unwrap_or(hex);
    rgit_hash::ObjectId::from_hex(hash, hex).ok()
}

/// Removes a submodule directory if it is empty (git leaves a populated
/// one in place with a warning).
fn rmdir_submodule(full: &Path, shown: &str) {
    if let Err(e) = std::fs::remove_dir(full) {
        if e.kind() != std::io::ErrorKind::NotFound {
            eprintln!("warning: {}", msg::unable_to_rmdir(shown, &crate::global::strerror(&e)));
        }
    }
}

fn canonical_mode(m: u32) -> u32 {
    match m & S_IFMT {
        S_IFLNK => S_IFLNK,
        S_IFGITLINK => S_IFGITLINK,
        _ if m & 0o100 != 0 => 0o100755,
        _ => 0o100644,
    }
}

/// Normalizes `--directory`: `.` and empty components dropped, `..`
/// resolved; `None` when it climbs above the start.
fn normalize_directory(d: &str) -> Option<Vec<u8>> {
    let mut parts: Vec<&str> = Vec::new();
    for c in d.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            c => parts.push(c),
        }
    }
    let mut out = parts.join("/").into_bytes();
    if !out.is_empty() {
        out.push(b'/');
    }
    Some(out)
}

/// `--whitespace=<action>` / `apply.whitespace`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum WsAction {
    #[default]
    NoWarn,
    Warn,
    Fix,
    Error,
    ErrorAll,
}

impl WsAction {
    fn parse(v: &str) -> Option<WsAction> {
        Some(match v {
            "nowarn" => WsAction::NoWarn,
            "warn" => WsAction::Warn,
            "fix" | "strip" => WsAction::Fix,
            "error" => WsAction::Error,
            "error-all" => WsAction::ErrorAll,
            _ => return None,
        })
    }
}

/// Errors shown before the rest are only counted.
const WS_SHOWN: usize = 5;

#[derive(Default)]
struct Settings {
    stat: bool,
    numstat: bool,
    summary: bool,
    check: bool,
    index: bool,
    cached: bool,
    ita: bool,
    unsafe_paths: bool,
    apply: bool,
    no_add: bool,
    reverse: bool,
    reject: bool,
    verbose: bool,
    quiet: bool,
    allow_empty: bool,
    nul: bool,
    match_opts: ApplyOptions,
    filters: Vec<(bool, String)>,
    fake_ancestor: Option<String>,
    quote_path: bool,
    file_mode: bool,
    symlinks: bool,
    /// `--3way`, with `--ours`/`--theirs`/`--union` and
    /// `merge.conflictStyle`.
    threeway: bool,
    favor: rgit_diff::merge::Favor,
    conflict_style: rgit_diff::merge::ConflictStyle,
    ws_action: WsAction,
    /// `--inaccurate-eof`.
    inaccurate_eof: bool,
    /// `core.whitespace`.
    ws_rule: rgit_diff::ws_rule::WsRule,
}

/// Whitespace errors found so far (over all inputs).
#[derive(Default)]
struct WsCount {
    lines: usize,
    /// Added lines fixed by `--whitespace=fix` in applied patches.
    fixed: usize,
    shown: usize,
    squelched: usize,
}

/// Where the content of a path stands after earlier patches of the same
/// input.
#[derive(Clone)]
enum Slot {
    /// `link`: the patch itself made the path a symbolic link (a mode
    /// taken over from the old file does not count).
    Written { data: Vec<u8>, mode: u32, link: bool },
    Removed,
}

/// The planned result of one patch.
struct Plan {
    patch: usize,
    /// A path to delete (deletion or rename source).
    remove: Option<Vec<u8>>,
    /// The new file: path, content, mode.
    write: Option<(Vec<u8>, Vec<u8>, u32)>,
    /// Whether the file is created (for `-N`).
    created: bool,
    /// 1-based numbers of rejected hunks.
    rejected: Vec<usize>,
    /// A three-way merge with conflicts: the base blob and the contents
    /// of ours and theirs (index stages 1-3).
    conflict: Option<(Option<rgit_hash::ObjectId>, Vec<u8>, Vec<u8>)>,
}

struct State<'a> {
    s: &'a Settings,
    repo: Option<&'a Repo>,
    /// Where work tree paths are rooted.
    root: PathBuf,
    /// The current directory relative to `root` (traditional patches).
    prefix: Vec<u8>,
    index: Option<Index>,
    checker: Option<Checker>,
    index_changed: bool,
    /// A `--3way` merge left conflicts (exit code 1).
    conflicted: bool,
    ws: WsCount,
    /// The summary of fatal whitespace errors was printed.
    ws_summary_printed: bool,
    parse_opts: ParseOptions,
}

/// One check failure (printed as `error: ...`).
struct Failure;

impl State<'_> {
    fn quote(&self, name: &[u8]) -> String {
        let mut out = Vec::new();
        rgit_diff::quote_path(&mut out, name, self.s.quote_path);
        lossy(&out)
    }

    fn display(&self, p: &FilePatch) -> String {
        p.display_name(|n| self.quote(n))
    }

    fn index_entry(&self, path: &[u8]) -> Option<&Entry> {
        self.index.as_ref()?.get(path, Stage::MERGED)
    }

    fn read_blob(&self, entry: &Entry) -> Result<Vec<u8>, String> {
        let repo = self.repo.ok_or_else(String::new)?;
        rgit_unpack::read_blob(repo.odb(), &entry.id).map_err(|e| e.to_string())
    }

    /// Content and mode of a work tree file; `None` when missing (or a
    /// directory).
    fn read_worktree(&self, path: &[u8]) -> Result<Option<(Vec<u8>, u32)>, std::io::Error> {
        let full = os_path(&self.root, path);
        if self.lstat(path).is_none() {
            return Ok(None);
        }
        let meta = std::fs::symlink_metadata(&full)?;
        if meta.is_dir() {
            return Ok(None);
        }
        if meta.file_type().is_symlink() {
            let t = std::fs::read_link(&full)?;
            return Ok(Some((t.as_os_str().as_encoded_bytes().to_vec(), S_IFLNK)));
        }
        let mut data = std::fs::read(&full)?;
        if let Some(conv) = self.repo.and_then(|r| r.converter().ok()) {
            // Patches apply to the blob form; no core.safecrlf check
            // (gitattributes(5)).
            data = conv
                .to_git(path, data, rgit_convert::Options::QUIET)
                .map_err(|_| std::io::Error::other("conversion"))?;
        }
        let mode = if self.s.file_mode {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if meta.permissions().mode() & 0o100 != 0 {
                    0o100755
                } else {
                    0o100644
                }
            }
            #[cfg(not(unix))]
            {
                0o100644
            }
        } else {
            match self.index_entry(path) {
                Some(e) if e.mode.is_file() => e.mode.0,
                _ => 0o100644,
            }
        };
        Ok(Some((data, mode)))
    }

    /// `lstat` of `path`; `None` when it is missing or lies beyond a
    /// symbolic link (or a non-directory).
    fn lstat(&self, path: &[u8]) -> Option<std::fs::Metadata> {
        let real_dirs = path.iter().enumerate().all(|(i, &b)| {
            b != b'/'
                || std::fs::symlink_metadata(os_path(&self.root, &path[..i]))
                    .is_ok_and(|m| m.is_dir())
        });
        if !real_dirs {
            return None;
        }
        std::fs::symlink_metadata(os_path(&self.root, path)).ok()
    }

    /// The original content of `path` (before this input) for the selected
    /// mode, `Ok(None)` when it does not exist there.
    fn original(&self, path: &[u8]) -> Result<Option<(Vec<u8>, u32)>, String> {
        let shown = lossy(path);
        // A submodule is its recorded commit, as `Subproject commit <id>`.
        if let Some(e) = self.index_entry(path).filter(|e| e.mode.is_gitlink()) {
            return Ok(Some((gitlink_text(&e.id), GITLINK)));
        }
        if !(self.s.cached || self.s.index || path.is_empty()) {
            let full = os_path(&self.root, path);
            if let Some(id) = crate::repository::submodule_head(&full, self.hash()) {
                return Ok(Some((gitlink_text(&id), GITLINK)));
            }
        }
        if self.s.cached || self.s.index {
            let Some(entry) = self.index_entry(path) else {
                return Ok(None);
            };
            if self.s.index {
                let meta = self.lstat(path);
                // A missing work tree file is fine; it is written anew.
                let same = match (&meta, &self.checker) {
                    (None, _) => !entry.flags.intent_to_add,
                    (Some(m), Some(c)) if !entry.flags.intent_to_add => {
                        // Stat data must match, as after `update-index --refresh`.
                        matches!(c.check_with(entry, m), Ok(Status::Unchanged))
                    }
                    _ => false,
                };
                if !same {
                    return Err(msg::does_not_match_index(&shown));
                }
            }
            let data = self.read_blob(entry)?;
            return Ok(Some((data, entry.mode.0)));
        }
        self.read_worktree(path)
            .map_err(|e| msg::path_error(&shown, &crate::global::strerror(&e)))
    }

    /// Whether a leading directory of `path` is (or becomes) a symbolic
    /// link.
    /// The old name of a patch is only checked against the files as they
    /// are (`table` is `None`), a new name also against earlier patches.
    fn beyond_symlink(&self, path: &[u8], table: Option<&HashMap<Vec<u8>, Slot>>) -> bool {
        for (i, _) in path.iter().enumerate().filter(|(_, b)| **b == b'/') {
            let lead = &path[..i];
            match table.and_then(|t| t.get(lead)) {
                Some(Slot::Written { link, .. }) => {
                    if *link {
                        return true;
                    }
                    continue;
                }
                Some(Slot::Removed) => continue,
                None => {}
            }
            if !self.s.cached {
                let full = os_path(&self.root, lead);
                if std::fs::symlink_metadata(full).is_ok_and(|m| m.file_type().is_symlink()) {
                    return true;
                }
            }
            if self.s.cached || self.s.index {
                if self.index_entry(lead).is_some_and(|e| e.mode.is_symlink()) {
                    return true;
                }
            }
        }
        false
    }

    /// Checks all patches of one input and computes their results.
    fn check(&mut self, patches: &mut [FilePatch]) -> Result<Vec<Plan>, Die> {
        if let Some(m) = patches.iter().find_map(|p| p.garbage.clone()) {
            return Err(Die::Error(m, 128));
        }
        // Paths this input deletes or renames away.
        let mut going: HashSet<Vec<u8>> = HashSet::new();
        for p in patches.iter() {
            if p.is_delete == Some(true) || p.is_rename {
                if let Some(o) = &p.old_name {
                    going.insert(o.clone());
                }
            }
        }
        // Paths must stay inside the work area.
        let check_paths = !self.s.unsafe_paths || self.s.index || self.s.cached;
        if check_paths {
            for p in patches.iter() {
                for name in [&p.old_name, &p.new_name].into_iter().flatten() {
                    // An empty name fails later, as a missing file.
                    if !name.is_empty() && !rgit_index::verify_path(name) {
                        return Err(Die::Error(msg::invalid_path(&lossy(name)), 128));
                    }
                }
            }
        }
        let mut table: HashMap<Vec<u8>, Slot> = HashMap::new();
        let mut plans = Vec::new();
        let mut failed = false;
        for (i, p) in patches.iter_mut().enumerate() {
            let shown = self.display(p);
            if self.s.verbose {
                eprintln!("{}", msg::checking(&shown));
            }
            match self.check_one(i, p, &mut table, &going) {
                Ok(plan) => plans.push(plan),
                Err(Failure) => failed = true,
            }
        }
        if failed {
            return Err(Die::Exit(1));
        }
        Ok(plans)
    }

    fn check_one(
        &mut self,
        idx: usize,
        p: &mut FilePatch,
        table: &mut HashMap<Vec<u8>, Slot>,
        going: &HashSet<Vec<u8>>,
    ) -> Result<Plan, Failure> {
        let fail = |m: String| {
            eprintln!("error: {m}");
            Failure
        };
        let removed = |path: &[u8], table: &HashMap<Vec<u8>, Slot>| {
            matches!(table.get(path), Some(Slot::Removed)) || going.contains(path)
        };
        // The preimage.
        let mut old: Option<(Vec<u8>, u32)> = None;
        if p.is_new != Some(true) {
            let path = p.old_name.clone().unwrap_or_default();
            let shown = lossy(&path);
            let from_table = if p.is_rename || p.is_copy {
                None
            } else {
                table.get(&path).cloned()
            };
            match from_table {
                Some(Slot::Removed) => return Err(fail(msg::renamed_or_deleted(&shown))),
                Some(Slot::Written { data, mode, .. }) => old = Some((data, mode)),
                None => {
                    // The index is asked first; the work tree only after the
                    // leading directories are known to be real.
                    let use_index = self.s.cached || self.s.index;
                    if !use_index && self.beyond_symlink(&path, None) {
                        return Err(fail(msg::beyond_symlink(&shown)));
                    }
                    let found = self.original(&path);
                    if use_index
                        && matches!(found, Ok(Some(_)))
                        && self.beyond_symlink(&path, None)
                    {
                        return Err(fail(msg::beyond_symlink(&shown)));
                    }
                    match found {
                        Err(m) => return Err(fail(m)),
                        Ok(Some(o)) => old = Some(o),
                        Ok(None) if p.is_new.is_none() => p.is_new = Some(true),
                        Ok(None) => {
                            let m = if self.s.cached || self.s.index {
                                msg::not_in_index(&shown)
                            } else {
                                msg::path_error(&shown, "No such file or directory")
                            };
                            return Err(fail(m));
                        }
                    }
                }
            }
            if p.is_new.is_none() {
                p.is_new = Some(false);
            }
        }
        let created = p.is_new == Some(true);
        // `--3way` turns a creation of a path that is in the index into
        // a merge of both additions.
        let mut add_add: Option<Vec<u8>> = None;
        if created && self.s.threeway {
            let path = p.new_name.clone().unwrap_or_default();
            if !removed(&path, table) {
                if let Some(e) = self.index_entry(&path).filter(|e| !e.flags.intent_to_add) {
                    let e = e.clone();
                    let blob = self.read_blob(&e).map_err(&fail)?;
                    if self.s.index {
                        let wt = self.read_worktree(&path).ok().flatten();
                        if wt.is_none_or(|(d, _)| d != blob) {
                            return Err(fail(msg::does_not_match_index(&lossy(&path))));
                        }
                    }
                    add_add = Some(blob);
                }
            }
        }
        if created && add_add.is_none() {
            let path = p.new_name.clone().unwrap_or_default();
            let shown = lossy(&path);
            let gone = removed(&path, table);
            if (self.s.index || self.s.cached) && !gone {
                if let Some(e) = self.index_entry(&path) {
                    if !e.flags.intent_to_add {
                        return Err(fail(msg::exists_in_index(&shown)));
                    }
                    if self.s.index {
                        return Err(fail(msg::does_not_match_index(&shown)));
                    }
                }
            }
            if !self.s.cached && !gone {
                let exists = matches!(table.get(&path), Some(Slot::Written { .. }))
                    || self.lstat(&path).is_some_and(|m| !m.is_dir());
                if exists {
                    return Err(fail(msg::exists_in_worktree(&shown)));
                }
            }
        }
        // Modes.
        let old_mode = old.as_ref().map(|o| o.1);
        if let (Some(actual), Some(expected)) = (old_mode, p.old_mode) {
            let expected = canonical_mode(expected);
            if actual != expected {
                let name = lossy(p.old_name.as_deref().unwrap_or_default());
                eprintln!("warning: {}", msg::wrong_mode(&name, actual, expected));
            }
        }
        let new_mode = if created {
            canonical_mode(p.new_mode.unwrap_or(0o100644))
        } else {
            match (p.old_mode, p.new_mode) {
                (Some(o), Some(n)) if canonical_mode(o) != canonical_mode(n) => canonical_mode(n),
                (None, Some(n)) => canonical_mode(n),
                _ => old_mode.unwrap_or(0o100644),
            }
        };
        // The content.
        let image = old.map(|o| o.0).unwrap_or_default();
        let name_for_fail = lossy(
            p.old_name
                .as_deref()
                .or(p.new_name.as_deref())
                .unwrap_or_default(),
        );
        let three = if self.s.threeway && !p.binary {
            self.three_way(p, &image, created, add_add.as_deref())
        } else {
            None
        };
        let mut conflict = None;
        let (content, rejected) = if let Some((merged, stages)) = three {
            conflict = stages;
            (merged, Vec::new())
        } else if p.binary {
            (self.binary_content(p, &image, &name_for_fail)?, Vec::new())
        } else {
            let eof_fixed;
            let hunks = if self.s.inaccurate_eof {
                eof_fixed = inaccurate_eof(&p.hunks);
                &eof_fixed
            } else {
                &p.hunks
            };
            let path = p.new_name.as_deref().or(p.old_name.as_deref()).unwrap_or_default();
            let rule = self.ws_rule_for(path);
            let fixer = |t: &[u8]| rgit_diff::ws_rule::fix_line(t, &rule);
            let fix: Option<rgit_apply::Fixer> =
                (self.s.ws_action == WsAction::Fix).then_some(&fixer);
            let applied =
                rgit_apply::apply_hunks_fixing(&image, hunks, &self.s.match_opts, fix);
            let mut rejected = Vec::new();
            for (k, r) in applied.hunks.iter().enumerate() {
                match r {
                    HunkResult::Applied {
                        line,
                        offset,
                        reduced,
                    } => {
                        if self.s.verbose && *offset != 0 {
                            eprintln!("{}", msg::hunk_offset(k + 1, *line, *offset));
                        }
                        if let (true, Some((l, t))) = (self.s.verbose, reduced) {
                            eprintln!("{}", msg::context_reduced(*l, *t, *line));
                        }
                    }
                    HunkResult::Failed { preimage } => {
                        if self.s.verbose {
                            eprintln!("error: {}\n{}", msg::WHILE_SEARCHING, lossy(preimage));
                        }
                        if !self.s.quiet {
                            eprintln!(
                                "error: {}",
                                msg::patch_failed(&name_for_fail, p.hunks[k].old_start)
                            );
                        }
                        if !self.s.reject {
                            if !self.s.quiet {
                                eprintln!("error: {}", msg::does_not_apply(&name_for_fail));
                            }
                            return Err(Failure);
                        }
                        rejected.push(k + 1);
                    }
                }
            }
            (applied.content, rejected)
        };
        if p.is_delete == Some(true) && !content.is_empty() && rejected.is_empty() {
            return Err(fail(msg::REMOVAL_LEAVES_CONTENTS.to_string()));
        }
        // Leading symbolic links of the new path.
        if let Some(n) = &p.new_name {
            if p.is_delete != Some(true) && self.beyond_symlink(n, Some(table)) {
                return Err(fail(msg::beyond_symlink(&lossy(n))));
            }
        }
        // Record the result.
        let mut plan = Plan {
            patch: idx,
            remove: None,
            write: None,
            created,
            rejected,
            conflict,
        };
        if p.is_delete == Some(true) {
            let o = p.old_name.clone().unwrap_or_default();
            table.insert(o.clone(), Slot::Removed);
            plan.remove = Some(o);
        } else {
            let n = p.new_name.clone().unwrap_or_default();
            if p.is_rename {
                if let Some(o) = &p.old_name {
                    if *o != n {
                        table.insert(o.clone(), Slot::Removed);
                        plan.remove = Some(o.clone());
                    }
                }
            }
            table.insert(
                n.clone(),
                Slot::Written {
                    data: content.clone(),
                    mode: new_mode,
                    link: p.new_mode.is_some_and(|m| canonical_mode(m) == S_IFLNK),
                },
            );
            plan.write = Some((n, content, new_mode));
        }
        Ok(plan)
    }

    /// Writes the results of one checked input.
    fn write(&mut self, patches: &[FilePatch], plans: &[Plan]) -> Result<bool, Die> {
        let worktree = !self.s.cached;
        let to_index = self.s.index || self.s.cached;
        for plan in plans {
            let Some(path) = &plan.remove else { continue };
            if worktree {
                let full = os_path(&self.root, path);
                if std::fs::symlink_metadata(&full).is_ok_and(|m| m.is_dir()) {
                    rmdir_submodule(&full, &lossy(path));
                } else {
                    let _ = rgit_unpack::remove_entry(&self.root, path, false);
                }
            }
            if to_index {
                if let Some(index) = &mut self.index {
                    if !index.remove_path(path).is_empty() {
                        self.index_changed = true;
                    }
                }
            }
        }
        for plan in plans {
            let Some((path, data, mode)) = &plan.write else { continue };
            if *mode == GITLINK {
                // The submodule is not checked out: its directory is
                // emptied and kept; the index records the commit.
                if worktree {
                    let full = os_path(&self.root, path);
                    rmdir_submodule(&full, &lossy(path));
                    let _ = std::fs::create_dir_all(&full);
                }
                if to_index {
                    let Some(id) = gitlink_id(self.hash(), data) else {
                        return Err(Die::Error(msg::bad_gitlink(&lossy(path)), 128));
                    };
                    self.add_entry(Entry::new(path.clone(), Stage::MERGED, IndexMode(GITLINK), id))?;
                }
                continue;
            }
            let mut meta = None;
            if worktree {
                // A leading symbolic link may only show up now (a renamed
                // link): never write through it.
                let lead_link = path.iter().enumerate().any(|(i, &b)| {
                    b == b'/'
                        && std::fs::symlink_metadata(os_path(&self.root, &path[..i]))
                            .is_ok_and(|m| m.file_type().is_symlink())
                });
                if lead_link {
                    return Err(Die::Error(msg::beyond_symlink(&lossy(path)), 128));
                }
                // Never remove a directory with content in the way.
                let full = os_path(&self.root, path);
                if std::fs::symlink_metadata(&full).is_ok_and(|m| m.is_dir()) {
                    if let Err(e) = std::fs::remove_dir(&full) {
                        return Err(Die::Error(
                            msg::unable_to_write(&lossy(path), *mode, &crate::global::strerror(&e)),
                            128,
                        ));
                    }
                }
                let entry = Entry::new(
                    path.clone(),
                    Stage::MERGED,
                    IndexMode(*mode),
                    self.hash().null(),
                );
                let converted;
                let data = match self.repo.and_then(|r| r.converter().ok()) {
                    Some(conv) if entry.mode.is_file() => {
                        let id = rgit_hash::hash_object(self.hash(), "blob", data)
                            .unwrap_or_else(|_| self.hash().null());
                        converted = conv.to_worktree(path, &id, data.clone());
                        &converted
                    }
                    _ => data,
                };
                match rgit_unpack::write_entry(&self.root, path, &entry, data, self.s.symlinks) {
                    Ok(m) => meta = m,
                    Err(e) => {
                        let err = match &e {
                            rgit_unpack::CheckoutError::LeadingDirectory { error, .. }
                            | rgit_unpack::CheckoutError::CreateFile { error, .. }
                            | rgit_unpack::CheckoutError::CreateSymlink { error, .. } => {
                                error.clone()
                            }
                        };
                        return Err(Die::Error(
                            msg::unable_to_write(&lossy(path), *mode, &err),
                            128,
                        ));
                    }
                }
            }
            if let (true, Some((base, ours, theirs))) = (to_index, &plan.conflict) {
                if let Some(index) = &mut self.index {
                    index.remove_path(path);
                }
                let ours = self.write_blob(ours)?;
                let theirs = self.write_blob(theirs)?;
                let stages = [(Stage::BASE, *base), (Stage::OURS, Some(ours)), (Stage::THEIRS, Some(theirs))];
                for (stage, id) in stages {
                    let Some(id) = id else { continue };
                    let entry = Entry::new(path.clone(), stage, IndexMode(*mode), id);
                    if let Some(index) = &mut self.index {
                        if let Err(e) = index.add(entry, AddOptions::default()) {
                            eprintln!("error: {e}");
                            return Err(Die::Error(msg::unable_to_add(&lossy(path)), 128));
                        }
                        self.index_changed = true;
                    }
                }
            } else if to_index {
                let id = self.write_blob(data)?;
                let mut entry = Entry::new(path.clone(), Stage::MERGED, IndexMode(*mode), id);
                if let (true, Some(m)) = (self.s.index, &meta) {
                    entry.stat = stat_from_metadata(m);
                }
                self.add_entry(entry)?;
            } else if self.s.ita && plan.created {
                if let Some(index) = &self.index {
                    if index.entries_for_path(path).is_empty() {
                        let id = rgit_hash::hash_object(self.hash(), "blob", b"")
                            .map_err(|e| Die::Fatal(e.to_string()))?;
                        let mut entry =
                            Entry::new(path.clone(), Stage::MERGED, IndexMode(*mode), id);
                        entry.flags.intent_to_add = true;
                        self.add_entry(entry)?;
                    }
                }
            }
        }
        // Rejects and the final messages.
        let mut clean = true;
        for plan in plans {
            if let (Some(_), Some((path, _, _))) = (&plan.conflict, &plan.write) {
                eprintln!("U {}", lossy(path));
                self.conflicted = true;
            }
        }
        for plan in plans {
            let p = &patches[plan.patch];
            let shown = self.display(p);
            if plan.rejected.is_empty() {
                if self.s.verbose {
                    eprintln!("{}", msg::applied_cleanly(&shown));
                }
                continue;
            }
            clean = false;
            if !self.s.quiet {
                eprintln!("{}", msg::applying_with_rejects(&shown, plan.rejected.len()));
            }
            let name = p
                .new_name
                .as_deref()
                .or(p.old_name.as_deref())
                .unwrap_or_default();
            let mut rej = msg::rej_header(&lossy(name)).into_bytes();
            for (k, h) in p.hunks.iter().enumerate() {
                if plan.rejected.contains(&(k + 1)) {
                    rej.extend_from_slice(&h.raw);
                    if !self.s.quiet {
                        eprintln!("{}", msg::hunk_rejected(k + 1));
                    }
                } else if !self.s.quiet {
                    eprintln!("{}", msg::hunk_applied(k + 1));
                }
            }
            let mut rej_name = name.to_vec();
            rej_name.extend_from_slice(b".rej");
            let full = os_path(&self.root, &rej_name);
            let _ = std::fs::remove_file(&full);
            if let Err(e) = std::fs::write(&full, &rej) {
                eprintln!(
                    "error: {}",
                    msg::path_error(&lossy(&rej_name), &crate::global::strerror(&e))
                );
            }
        }
        Ok(clean)
    }

    /// The whitespace rule of `path`: `core.whitespace`, changed by the
    /// `whitespace` attribute.
    fn ws_rule_for(&self, path: &[u8]) -> rgit_diff::ws_rule::WsRule {
        use rgit_diff::ws_rule::{self, WsRule};
        let base = self.s.ws_rule;
        let Some(conv) = self.repo.and_then(|r| r.converter().ok()) else {
            return base;
        };
        match conv.attr(path, b"whitespace") {
            rgit_attr::State::Set => WsRule {
                bits: ws_rule::ATTRIBUTE_SET_RULES,
                tab_width: base.tab_width,
            },
            rgit_attr::State::Unset => WsRule::none(base.tab_width),
            rgit_attr::State::Value(v) => {
                WsRule::parse(&String::from_utf8_lossy(&v)).unwrap_or(base)
            }
            rgit_attr::State::Unspecified => base,
        }
    }

    /// Reports one whitespace error (the first few in full).
    fn ws_report(&mut self, source: &str, line: usize, what: &str, text: &[u8]) {
        if self.s.ws_action == WsAction::ErrorAll || self.ws.shown < WS_SHOWN {
            self.ws.shown += 1;
            eprintln!("{source}:{line}: {what}.\n{}", lossy(text));
        } else {
            self.ws.squelched += 1;
        }
    }

    /// Looks for whitespace errors in the added lines (fixing them with
    /// `--whitespace=fix`); returns how many lines have errors and how
    /// many added lines were fixed.
    fn whitespace_check(&mut self, patches: &mut [FilePatch], source: &str) -> (usize, usize) {
        use rgit_diff::ws_rule;
        let action = self.s.ws_action;
        if action == WsAction::NoWarn {
            return (0, 0);
        }
        let kinds = [
            (ws_rule::BLANK_AT_EOL, crate::messages::diff::WS_TRAILING),
            (ws_rule::SPACE_BEFORE_TAB, crate::messages::diff::WS_SPACE_BEFORE_TAB),
            (ws_rule::INDENT_WITH_NON_TAB, crate::messages::diff::WS_INDENT_WITH_SPACES),
            (ws_rule::TAB_IN_INDENT, crate::messages::diff::WS_TAB_IN_INDENT),
            (ws_rule::INCOMPLETE_LINE, crate::messages::diff::WS_INCOMPLETE),
        ];
        let mut found = 0;
        let mut fixed = 0;
        // `--whitespace=fix` also fixes (and reports) context lines.
        let context = action == WsAction::Fix && !self.s.reverse;
        for p in patches.iter_mut() {
            if p.binary {
                continue;
            }
            let path = p.new_name.clone().or_else(|| p.old_name.clone()).unwrap_or_default();
            let rule = self.ws_rule_for(&path);
            // A symbolic link has no final line feed.
            let link = [p.old_mode, p.new_mode].contains(&Some(S_IFLNK));
            let incomplete_rule = rule.has(ws_rule::INCOMPLETE_LINE) && !link;
            let mut reports = Vec::new();
            for h in &mut p.hunks {
                for l in &mut h.lines {
                    if l.kind == LineKind::Remove || (l.kind == LineKind::Context && !context) {
                        continue;
                    }
                    let body = l.text.strip_suffix(b"\n").unwrap_or(&l.text).to_vec();
                    let mut errors = ws_rule::check_line(&body, &rule).errors;
                    let incomplete = l.kind == LineKind::Add
                        && incomplete_rule
                        && !l.text.ends_with(b"\n");
                    if incomplete {
                        errors |= ws_rule::INCOMPLETE_LINE;
                    }
                    if action == WsAction::Fix && l.kind == LineKind::Add {
                        l.text = ws_rule::fix_line(&l.text, &rule);
                        if incomplete {
                            l.text.push(b'\n');
                        }
                    }
                    let names: Vec<&str> = kinds
                        .iter()
                        .filter(|(bit, _)| errors & bit != 0)
                        .map(|(_, n)| *n)
                        .collect();
                    if names.is_empty() {
                        continue;
                    }
                    found += 1;
                    if l.kind == LineKind::Add {
                        fixed += 1;
                    }
                    reports.push((l.line, names.join(", "), body));
                }
            }
            for (line, names, text) in reports {
                self.ws_report(source, line, &names, &text);
            }
        }
        // New blank lines at the end of a file, found when the patches
        // are applied: not when other errors already stop them.
        let fatal = matches!(action, WsAction::Error | WsAction::ErrorAll);
        if !(fatal && found > 0) && (self.s.apply || self.s.check) {
            for p in patches.iter_mut() {
                let path = p.new_name.clone().or_else(|| p.old_name.clone()).unwrap_or_default();
                if p.binary || !self.ws_rule_for(&path).has(ws_rule::BLANK_AT_EOF) {
                    continue;
                }
                let Some(h) = p.hunks.last_mut() else { continue };
                // The hunk ends the file when it has less than the usual
                // trailing context; its end of blank lines, added or
                // not, holds the new blank lines.
                if h.trailing >= 3 || self.s.match_opts.unidiff_zero {
                    continue;
                }
                let run = h
                    .lines
                    .iter()
                    .rev()
                    .take_while(|l| l.kind != LineKind::Remove && ws_rule::is_blank(&l.text))
                    .count();
                let tail = h.lines.len() - run;
                let blank = h.lines[tail..]
                    .iter()
                    .filter(|l| l.kind == LineKind::Add)
                    .count();
                if blank == 0 {
                    continue;
                }
                let line = h.lines[tail..]
                    .iter()
                    .find(|l| l.kind == LineKind::Add)
                    .map_or(0, |l| l.line);
                if action == WsAction::Fix {
                    let mut k = 0;
                    h.lines.retain(|l| {
                        k += 1;
                        k <= tail || l.kind != LineKind::Add
                    });
                    h.new_count -= blank;
                    p.added = p.added.saturating_sub(blank);
                    h.finish();
                }
                found += 1;
                self.ws_report(source, line, crate::messages::diff::WS_BLANK_AT_EOF, b"+");
            }
        }
        self.ws.lines += found;
        (found, fixed)
    }

    /// The summary of the whitespace errors; 128 when they are fatal.
    fn ws_summary(&self) -> i32 {
        let n = self.ws.lines;
        if self.ws.squelched > 0 {
            eprintln!("warning: {}", msg::ws_squelched(self.ws.squelched));
        }
        if n == 0 {
            return 0;
        }
        match self.s.ws_action {
            WsAction::NoWarn => 0,
            WsAction::Warn => {
                eprintln!("warning: {}", msg::ws_lines_add(n));
                0
            }
            // Nothing applied (`--check`, or errors only in context
            // lines): the errors are only reported.
            WsAction::Fix if self.ws.fixed == 0 => {
                eprintln!("warning: {}", msg::ws_lines_add(n));
                0
            }
            WsAction::Fix => {
                eprintln!("warning: {}", msg::ws_lines_fixed(self.ws.fixed));
                0
            }
            WsAction::Error | WsAction::ErrorAll => {
                eprintln!("error: {}", msg::ws_lines_add(n));
                128
            }
        }
    }

    /// `--3way`: merge the change from the preimage blob named on the
    /// `index` line to the patched blob into `ours`. `None` (after a
    /// message) when that is not possible and the patch is applied
    /// directly; else the result and, with conflicts, the index stages.
    #[allow(clippy::type_complexity)]
    fn three_way(
        &self,
        p: &FilePatch,
        ours: &[u8],
        created: bool,
        add_add: Option<&[u8]>,
    ) -> Option<(Vec<u8>, Option<(Option<rgit_hash::ObjectId>, Vec<u8>, Vec<u8>)>)> {
        let fallback = |lacks: bool| {
            if lacks {
                eprintln!("error: {}", msg::LACKS_BLOB);
            }
            eprintln!("{}", msg::FALLING_BACK);
            None
        };
        let (base_id, base, ours) = if let Some(existing) = add_add {
            eprintln!("{}", msg::PERFORMING_3WAY);
            // git records the empty base.
            if self.s.index || self.s.cached {
                let _ = self.write_blob(b"");
            }
            (None, Vec::new(), existing)
        } else {
            if created || p.is_delete == Some(true) {
                return fallback(false);
            }
            let repo = self.repo?;
            let base_id = p.old_oid.as_deref().and_then(|h| resolve_blob(repo, h));
            let Some(base_id) = base_id else {
                return fallback(true);
            };
            let Some(rgit_object::Object::Blob(base)) = repo.read_object(&base_id) else {
                return fallback(true);
            };
            (Some(base_id), base.data, ours)
        };
        let applied = rgit_apply::apply_hunks(&base, &p.hunks, &self.s.match_opts);
        if applied
            .hunks
            .iter()
            .any(|h| matches!(h, HunkResult::Failed { .. }))
        {
            return fallback(false);
        }
        let theirs = applied.content;
        let opts = rgit_diff::merge::MergeOptions {
            style: self.s.conflict_style,
            favor: self.s.favor,
            marker_size: 0,
            ours_label: b"ours",
            base_label: b"base",
            theirs_label: b"theirs",
            diff: rgit_diff::merge::merge_diff_options(),
        };
        let name = lossy(p.new_name.as_deref().or(p.old_name.as_deref()).unwrap_or_default());
        let Ok(result) = rgit_diff::merge::merge(&base, ours, &theirs, &opts) else {
            return fallback(false);
        };
        if result.is_clean() {
            eprintln!("{}", msg::applied_3way_cleanly(&name));
            Some((result.content, None))
        } else {
            eprintln!("{}", msg::applied_3way_conflicts(&name));
            Some((result.content, Some((base_id, ours.to_vec(), theirs))))
        }
    }

    /// The result of a binary patch: the preimage must have the object
    /// id of the `index` line, the result the new one. Without data the
    /// new blob has to be in the repository.
    fn binary_content(&self, p: &FilePatch, image: &[u8], name: &str) -> Result<Vec<u8>, Failure> {
        let hash = self.hash();
        let full = |o: &Option<String>| {
            o.as_deref()
                .filter(|h| h.len() == hash.hex_len())
                .and_then(|h| rgit_hash::ObjectId::from_hex(hash, h.as_bytes()).ok())
        };
        let fail = |m: String| {
            eprintln!("error: {m}");
            eprintln!("error: {}", msg::does_not_apply(name));
            Failure
        };
        let (old_id, new_id) = (full(&p.old_oid), full(&p.new_oid));
        let blob_id = |data: &[u8]| rgit_hash::hash_object(hash, "blob", data).expect("hashable");
        if p.binary_data.is_none() && (old_id.is_none() || new_id.is_none()) {
            return Err(fail(msg::binary_without_index(name)));
        }
        if let Some(old) = old_id {
            let have = blob_id(image);
            let expected = if old.is_null() { blob_id(b"") } else { old };
            if have != expected {
                return Err(fail(msg::binary_preimage_mismatch(name, &have.to_hex())));
            }
        }
        let result = match &p.binary_data {
            Some((hunk, _)) if hunk.delta => rgit_pack::apply_delta(image, &hunk.data)
                .map_err(|_| fail(msg::binary_delta_failed(name)))?,
            Some((hunk, _)) => hunk.data.clone(),
            None => {
                let id = new_id.expect("checked");
                if id.is_null() {
                    Vec::new()
                } else {
                    match self.repo.and_then(|r| r.read_object(&id)) {
                        Some(rgit_object::Object::Blob(b)) => b.data,
                        _ => return Err(fail(msg::binary_without_index(name))),
                    }
                }
            }
        };
        if let Some(new) = new_id {
            let got = blob_id(&result);
            let expected = if new.is_null() { blob_id(b"") } else { new };
            if got != expected {
                return Err(fail(msg::binary_wrong_result(name, &new.to_hex(), &got.to_hex())));
            }
        }
        Ok(result)
    }

    fn hash(&self) -> rgit_hash::HashKind {
        self.repo.map_or(rgit_hash::HashKind::Sha1, |r| r.hash)
    }

    fn write_blob(&self, data: &[u8]) -> Result<rgit_hash::ObjectId, Die> {
        let repo = self.repo.ok_or_else(|| Die::Fatal(String::new()))?;
        repo.odb()
            .write(ObjectKind::Blob, data)
            .map_err(|e| Die::Fatal(e.to_string()))
    }

    fn add_entry(&mut self, entry: Entry) -> Result<(), Die> {
        let Some(index) = &mut self.index else {
            return Ok(());
        };
        index.remove_path(entry.path());
        let path = lossy(entry.path());
        if let Err(e) = index.add(entry, AddOptions::default()) {
            eprintln!("error: {e}");
            return Err(Die::Error(msg::unable_to_add(&path), 128));
        }
        self.index_changed = true;
        Ok(())
    }

    /// Whether `p` is used: inside the current directory (git patches)
    /// and selected by `--include`/`--exclude`.
    fn use_patch(&self, p: &FilePatch) -> bool {
        let name = p
            .new_name
            .as_deref()
            .or(p.old_name.as_deref())
            .unwrap_or_default();
        if !self.prefix.is_empty() && !name.starts_with(&self.prefix) {
            return false;
        }
        let mut has_include = false;
        for (include, pat) in &self.s.filters {
            has_include |= *include;
            if rgit_pathspec::wildmatch::wildmatch(
                pat.as_bytes(),
                name,
                rgit_pathspec::wildmatch::WildmatchFlags::NONE,
            ) {
                return *include;
            }
        }
        !has_include
    }

    /// Processes one input. Returns the exit code so far (0 or 1).
    fn input(&mut self, data: &[u8], source: &str) -> Result<i32, Die> {
        let parsed = rgit_apply::parse(data, source, &mut self.parse_opts);
        for w in &parsed.warnings {
            eprintln!("warning: {w}");
        }
        let all = parsed.patches.map_err(|e| Die::Error(e.0, 128))?;
        if all.is_empty() && !self.s.allow_empty {
            return Err(Die::Error(msg::NO_VALID_PATCHES.into(), 128));
        }
        let mut patches = Vec::new();
        for mut p in all {
            if !self.prefix.is_empty() && !p.is_git && !self.parse_opts.names_from_top {
                for name in [&mut p.old_name, &mut p.new_name].into_iter().flatten() {
                    let mut full = self.prefix.clone();
                    full.extend_from_slice(name);
                    *name = full;
                }
            }
            if !self.use_patch(&p) {
                if self.s.verbose {
                    let name = p
                        .new_name
                        .as_deref()
                        .or(p.old_name.as_deref())
                        .unwrap_or_default();
                    eprintln!("{}", msg::skipped(&lossy(name)));
                }
                continue;
            }
            if self.s.reverse {
                p.reverse();
            }
            if self.s.no_add {
                for h in &mut p.hunks {
                    h.lines.retain(|l| l.kind != LineKind::Add);
                    h.finish();
                }
                p.added = 0;
            }
            patches.push(p);
        }
        if self.s.reverse {
            patches.reverse();
        }
        let mut code = 0;
        let (ws_errors, ws_fixed) = self.whitespace_check(&mut patches, source);
        let ws_fatal =
            ws_errors > 0 && matches!(self.s.ws_action, WsAction::Error | WsAction::ErrorAll);
        if (self.s.check || self.s.apply) && !ws_fatal {
            let plans = self.check(&mut patches)?;
            if self.s.apply {
                if !self.write(&patches, &plans)? {
                    code = 1;
                }
                self.ws.fixed += ws_fixed;
            }
        }
        if let Some(file) = &self.s.fake_ancestor {
            self.fake_ancestor(&patches, file)?;
        }
        let stats = self.s.stat || self.s.numstat || self.s.summary;
        if ws_fatal && stats && !self.ws_summary_printed {
            // The fatal errors are summed up before any statistics.
            self.ws_summary_printed = true;
            let _ = self.ws_summary();
        }
        let mut out = Vec::new();
        if self.s.stat {
            self.write_stat(&mut out, &patches);
        }
        if self.s.numstat {
            self.write_numstat(&mut out, &patches);
        }
        if self.s.summary {
            self.write_summary(&mut out, &patches);
        }
        let _ = std::io::stdout().write_all(&out);
        Ok(code)
    }

    fn write_stat(&self, out: &mut Vec<u8>, patches: &[FilePatch]) {
        let names: Vec<String> = patches
            .iter()
            .map(|p| {
                let n = p.new_name.as_deref().or(p.old_name.as_deref()).unwrap_or_default();
                self.quote(n)
            })
            .collect();
        // The column fits the old names too.
        let max_len = patches
            .iter()
            .flat_map(|p| [&p.old_name, &p.new_name])
            .flatten()
            .map(|n| self.quote(n).chars().count())
            .max()
            .unwrap_or(0)
            .min(50);
        let max_change = patches.iter().map(|p| p.added + p.deleted).max().unwrap_or(0);
        let width = if max_change + max_len > 70 {
            70 - max_len
        } else {
            max_change
        };
        let scale = |n: usize| {
            if max_change == 0 || width == max_change {
                n
            } else {
                (n * width + max_change / 2) / max_change
            }
        };
        let (mut ins, mut del) = (0, 0);
        for (p, name) in patches.iter().zip(&names) {
            let name = truncate_name(name, max_len);
            let pad = max_len.saturating_sub(name.chars().count());
            if p.binary {
                let _ = writeln!(out, " {name}{} |  Bin", " ".repeat(pad));
                continue;
            }
            ins += p.added;
            del += p.deleted;
            let _ = writeln!(
                out,
                " {name}{} |{:5} {}{}",
                " ".repeat(pad),
                p.added + p.deleted,
                "+".repeat(scale(p.added)),
                "-".repeat(scale(p.deleted))
            );
        }
        let _ = writeln!(
            out,
            "{}",
            crate::messages::diff::stat_summary(patches.len(), ins, del)
        );
    }

    fn write_numstat(&self, out: &mut Vec<u8>, patches: &[FilePatch]) {
        for p in patches {
            let n = p.new_name.as_deref().or(p.old_name.as_deref()).unwrap_or_default();
            if p.binary {
                out.extend_from_slice(b"-\t-\t");
            } else {
                let _ = write!(out, "{}\t{}\t", p.added, p.deleted);
            }
            if self.s.nul {
                out.extend_from_slice(n);
                out.push(0);
            } else {
                out.extend_from_slice(self.quote(n).as_bytes());
                out.push(b'\n');
            }
        }
    }

    fn write_summary(&self, out: &mut Vec<u8>, patches: &[FilePatch]) {
        for p in patches {
            let old = p.old_name.as_deref().unwrap_or_default();
            let new = p.new_name.as_deref().unwrap_or_default();
            let mode_change = match (p.old_mode, p.new_mode) {
                (Some(a), Some(b)) if a != b => Some((a, b)),
                _ => None,
            };
            // Traditional patches know no modes.
            let mode = |m: Option<u32>| m.map(|m| format!("mode {m:06o} ")).unwrap_or_default();
            if p.is_new == Some(true) {
                let _ = writeln!(out, " create {}{}", mode(p.new_mode), lossy(new));
            } else if p.is_delete == Some(true) {
                let _ = writeln!(out, " delete {}{}", mode(p.old_mode), lossy(old));
            } else if p.is_rename || p.is_copy {
                let what = if p.is_copy { "copy" } else { "rename" };
                let _ = writeln!(
                    out,
                    " {what} {} ({}%)",
                    pprint_rename(old, new),
                    p.score.unwrap_or(0)
                );
                if let Some((a, b)) = mode_change {
                    let _ = writeln!(out, " mode change {a:06o} => {b:06o}");
                }
            } else {
                if p.is_rewrite {
                    let _ = writeln!(out, " rewrite {} ({}%)", lossy(new), p.score.unwrap_or(0));
                }
                if let Some((a, b)) = mode_change {
                    let _ = writeln!(out, " mode change {a:06o} => {b:06o} {}", lossy(new));
                }
            }
        }
    }

    /// `--build-fake-ancestor=<file>`: an index with the preimages named by
    /// the patches' `index` lines.
    fn fake_ancestor(&mut self, patches: &[FilePatch], file: &str) -> Result<(), Die> {
        let Some(repo) = self.repo else {
            return Ok(());
        };
        let mut out = Index::new(repo.hash);
        for p in patches {
            let Some(name) = &p.old_name else { continue };
            let shown = lossy(name);
            let lacking = || Die::Error(msg::sha1_lacking(&shown), 128);
            let (id, mode) = match &p.old_oid {
                None => {
                    // A pure mode change: take the current index entry.
                    let e = self.index_entry(name).ok_or_else(lacking)?;
                    (e.id, e.mode)
                }
                Some(hex) => {
                    let ok = !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit());
                    let id = ok
                        .then(|| resolve_blob(repo, hex))
                        .flatten()
                        .ok_or_else(lacking)?;
                    (id, IndexMode(canonical_mode(p.old_mode.unwrap_or(0o100644))))
                }
            };
            let entry = Entry::new(name.clone(), Stage::MERGED, mode, id);
            out.add(
                entry,
                AddOptions {
                    ok_to_replace: true,
                    ..Default::default()
                },
            )
            .map_err(|e| Die::Fatal(e.to_string()))?;
        }
        let path = repo.setup.original_cwd.join(file);
        std::fs::write(&path, out.to_bytes(&WriteOptions::default()))
            .map_err(|e| Die::Fatal(format!("{file}: {}", crate::global::strerror(&e))))?;
        Ok(())
    }
}

/// An object name (possibly abbreviated) of an existing blob.
fn resolve_blob(repo: &Repo, hex: &str) -> Option<rgit_hash::ObjectId> {
    let id = if hex.len() == repo.hash.hex_len() {
        rgit_hash::ObjectId::from_hex(repo.hash, hex.as_bytes()).ok()?
    } else {
        let prefix = rgit_hash::Prefix::from_hex(repo.hash, hex.as_bytes()).ok()?;
        match repo.odb().resolve_prefix(&prefix).ok()? {
            rgit_odb::PrefixMatch::Unique(id) => id,
            // An ambiguous prefix names the one blob among the candidates.
            rgit_odb::PrefixMatch::Ambiguous(ids) => {
                let mut blobs = ids.into_iter().filter(|id| {
                    matches!(repo.odb().header(id), Ok(Some(h)) if h.kind == ObjectKind::Blob)
                });
                let one = blobs.next()?;
                if blobs.next().is_some() {
                    return None;
                }
                one
            }
            rgit_odb::PrefixMatch::None => return None,
        }
    };
    if id.is_null() {
        return None;
    }
    match repo.odb().header(&id) {
        Ok(Some(h)) if h.kind == ObjectKind::Blob => Some(id),
        _ => None,
    }
}

/// A stat name cut to `width` columns: `...` and the tail of the name
/// from a `/` on.
fn truncate_name(name: &str, width: usize) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= width {
        return name.to_string();
    }
    let keep = width.saturating_sub(3);
    let mut tail = &chars[chars.len() - keep..];
    if let Some(p) = tail.iter().position(|&c| c == '/') {
        tail = &tail[p..];
    }
    let mut s = String::from("...");
    s.extend(tail.iter());
    s
}

/// `a/{b => c}/d` for a rename of `a/b/d` to `a/c/d`.
fn pprint_rename(old: &[u8], new: &[u8]) -> String {
    // Common directory prefix (up to and including a `/`).
    let mut pfx = 0;
    for (i, (a, b)) in old.iter().zip(new).enumerate() {
        if a != b {
            break;
        }
        if *a == b'/' {
            pfx = i + 1;
        }
    }
    // Common suffix starting at a `/`, not overlapping the prefix.
    let mut sfx = 0;
    let max = old.len().min(new.len()) - pfx;
    let mut k = 0;
    while k < max && old[old.len() - 1 - k] == new[new.len() - 1 - k] {
        if old[old.len() - 1 - k] == b'/' {
            sfx = k + 1;
        }
        k += 1;
    }
    let a = &old[pfx..old.len() - sfx];
    let b = &new[pfx..new.len() - sfx];
    if pfx == 0 && sfx == 0 {
        return format!("{} => {}", lossy(old), lossy(new));
    }
    format!(
        "{}{{{} => {}}}{}",
        lossy(&old[..pfx]),
        lossy(a),
        lossy(b),
        lossy(&old[old.len() - sfx..])
    )
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let stat = Cell::new(false);
    let numstat = Cell::new(false);
    let summary = Cell::new(false);
    let check = Cell::new(false);
    let index = Cell::new(false);
    let cached = Cell::new(false);
    let ita = Cell::new(false);
    let unsafe_paths = Cell::new(false);
    let force_apply = Cell::new(false);
    let no_add = Cell::new(false);
    let threeway = Cell::new(false);
    let conflict_style = Cell::new(0);
    let fake_ancestor: RefCell<Option<String>> = RefCell::new(None);
    let nul = Cell::new(false);
    let context = Cell::new(u64::MAX);
    let whitespace: RefCell<Option<String>> = RefCell::new(None);
    let ignore_ws = Cell::new(false);
    let reverse = Cell::new(false);
    let unidiff_zero = Cell::new(false);
    let reject = Cell::new(false);
    let overlap = Cell::new(false);
    let verbose = Cell::new(0);
    let quiet = Cell::new(0);
    let inaccurate_eof = Cell::new(false);
    let recount = Cell::new(false);
    let directory: RefCell<Option<String>> = RefCell::new(None);
    let allow_empty = Cell::new(false);
    let filters: RefCell<Vec<(bool, String)>> = RefCell::new(Vec::new());
    let p_value: Cell<Option<usize>> = Cell::new(None);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::callback(None, "exclude", Some("path"), msg::HELP_EXCLUDE, |v, _| {
                filters
                    .borrow_mut()
                    .push((false, v.unwrap_or_default().to_string()));
                Ok(())
            })
            .noneg(),
            Opt::callback(None, "include", Some("path"), msg::HELP_INCLUDE, |v, _| {
                filters
                    .borrow_mut()
                    .push((true, v.unwrap_or_default().to_string()));
                Ok(())
            })
            .noneg(),
            Opt::callback('p', None, Some("num"), msg::HELP_P, |v, _| {
                let arg = v.unwrap_or_default();
                match arg.parse::<usize>() {
                    Ok(n) if arg.bytes().all(|b| b.is_ascii_digit()) => {
                        p_value.set(Some(n));
                        Ok(())
                    }
                    _ => Err(CallbackError::Fatal(msg::bad_p_value(arg))),
                }
            }),
            Opt::flag(None, "no-add", &no_add, msg::HELP_NO_ADD).noneg(),
            Opt::callback(None, "add", None, msg::HELP_ADD, |_, _| {
                no_add.set(false);
                Ok(())
            })
            .noarg()
            .noneg(),
            Opt::flag(None, "stat", &stat, msg::HELP_STAT),
            Opt::flag(None, "numstat", &numstat, msg::HELP_NUMSTAT),
            Opt::flag(None, "summary", &summary, msg::HELP_SUMMARY),
            Opt::flag(None, "check", &check, msg::HELP_CHECK),
            Opt::flag(None, "index", &index, msg::HELP_INDEX),
            Opt::flag('N', "intent-to-add", &ita, msg::HELP_ITA),
            Opt::flag(None, "cached", &cached, msg::HELP_CACHED),
            Opt::flag(None, "unsafe-paths", &unsafe_paths, msg::HELP_UNSAFE),
            Opt::flag(None, "apply", &force_apply, msg::HELP_APPLY),
            Opt::flag('3', "3way", &threeway, msg::HELP_3WAY),
            Opt::set_int(None, "ours", &conflict_style, msg::HELP_OURS, 1).noneg(),
            Opt::set_int(None, "theirs", &conflict_style, msg::HELP_THEIRS, 2).noneg(),
            Opt::set_int(None, "union", &conflict_style, msg::HELP_UNION, 3).noneg(),
            Opt::filename(None, "build-fake-ancestor", &fake_ancestor, msg::HELP_FAKE_ANCESTOR),
            Opt::flag('z', None, &nul, msg::HELP_Z),
            Opt::unsigned('C', None, &context, msg::HELP_C),
            Opt::string(None, "whitespace", &whitespace, "action", msg::HELP_WHITESPACE),
            Opt::flag(None, "ignore-space-change", &ignore_ws, msg::HELP_IGNORE_WS),
            Opt::flag(None, "ignore-whitespace", &ignore_ws, msg::HELP_IGNORE_WS),
            Opt::flag('R', "reverse", &reverse, msg::HELP_REVERSE),
            Opt::flag(None, "unidiff-zero", &unidiff_zero, msg::HELP_UNIDIFF_ZERO),
            Opt::flag(None, "reject", &reject, msg::HELP_REJECT),
            Opt::flag(None, "allow-overlap", &overlap, msg::HELP_OVERLAP),
            Opt::verbose(&verbose, msg::HELP_VERBOSE),
            Opt::quiet(&quiet, msg::HELP_QUIET),
            Opt::flag(None, "inaccurate-eof", &inaccurate_eof, msg::HELP_INACCURATE_EOF),
            Opt::flag(None, "recount", &recount, msg::HELP_RECOUNT),
            Opt::string(None, "directory", &directory, "root", msg::HELP_DIRECTORY),
            Opt::flag(None, "allow-empty", &allow_empty, msg::HELP_ALLOW_EMPTY),
            // Old options, accepted and ignored.
            Opt::noop(None, "binary"),
            Opt::noop(None, "allow-binary-replacement"),
        ],
    );
    let args = parser.parse(argv)?.args;
    drop(parser);

    let mut root_dir = Vec::new();
    if let Some(d) = directory.borrow().as_deref() {
        root_dir = normalize_directory(d).ok_or_else(|| Die::Error(msg::bad_directory(d), 129))?;
    }
    if let Some(ws) = whitespace.borrow().as_deref() {
        if !matches!(ws, "nowarn" | "warn" | "fix" | "error" | "error-all" | "strip") {
            return Err(Die::Error(msg::bad_whitespace(ws), 129));
        }
    }
    if reject.get() && threeway.get() {
        return Err(Die::Error(msg::REJECT_AND_3WAY.into(), 128));
    }
    if conflict_style.get() != 0 && !threeway.get() {
        return Err(Die::Fatal(msg::OURS_NEEDS_3WAY.into()));
    }

    let repo = Repo::open_gently(ctx)?;
    let in_worktree = repo
        .as_ref()
        .is_some_and(|r| r.setup.work_tree.is_some() && !r.setup.is_inside_git_dir());
    let (index_wanted, cached_wanted) = (
        index.get() || cached.get() || threeway.get(),
        cached.get(),
    );
    if index_wanted && repo.is_none() {
        let opt = if cached_wanted { "--cached" } else { "--index" };
        return Err(Die::Error(msg::outside_repository(opt), 128));
    }
    let loaded;
    let config = match &repo {
        Some(r) => &r.config,
        None => {
            loaded = crate::repo_objects::load_config(ctx, None)?;
            &loaded
        }
    };
    let cfg_bool = |k: &str, d: bool| config.bool(k).ok().flatten().unwrap_or(d);
    let quote_path = cfg_bool("core.quotepath", true);
    let file_mode = cfg_bool("core.filemode", true);
    let symlinks = cfg_bool("core.symlinks", true);

    // `--whitespace`, else `apply.whitespace`; without either, warnings
    // only when the patch is applied.
    let applies = force_apply.get()
        || !(stat.get()
            || numstat.get()
            || summary.get()
            || check.get()
            || fake_ancestor.borrow().is_some());
    let ws_action = match whitespace.borrow().as_deref() {
        Some(v) => WsAction::parse(v).expect("checked"),
        None => match config.string("apply.whitespace").ok().flatten() {
            Some(v) => {
                let v = v.to_string();
                WsAction::parse(&v).ok_or_else(|| Die::Error(msg::bad_whitespace(&v), 128))?
            }
            None if applies => WsAction::Warn,
            None => WsAction::NoWarn,
        },
    };
    let ignore_ws_config = match config.string("apply.ignorewhitespace").ok().flatten() {
        Some(v) => match v.to_string().as_str() {
            "change" => true,
            "no" | "none" | "never" | "false" => false,
            other => return Err(Die::Error(msg::bad_ignore_whitespace(other), 128)),
        },
        None => false,
    };

    let quiet_v = quiet.get() > 0;
    let mut settings = Settings {
        stat: stat.get(),
        numstat: numstat.get(),
        summary: summary.get(),
        check: check.get(),
        index: (index.get() || threeway.get()) && !cached.get(),
        cached: cached.get(),
        ita: ita.get() && !index_wanted && in_worktree,
        unsafe_paths: unsafe_paths.get(),
        apply: true,
        no_add: no_add.get(),
        reverse: reverse.get(),
        reject: reject.get(),
        verbose: !quiet_v && (verbose.get() > 0 || reject.get()),
        quiet: quiet_v,
        allow_empty: allow_empty.get(),
        nul: nul.get(),
        match_opts: ApplyOptions {
            min_context: usize::try_from(context.get()).unwrap_or(usize::MAX),
            unidiff_zero: unidiff_zero.get(),
            allow_overlap: overlap.get(),
            ignore_whitespace: ignore_ws.get() || ignore_ws_config,
        },
        filters: filters.take(),
        fake_ancestor: fake_ancestor.take(),
        quote_path,
        file_mode,
        symlinks,
        threeway: threeway.get(),
        favor: match conflict_style.get() {
            1 => rgit_diff::merge::Favor::Ours,
            2 => rgit_diff::merge::Favor::Theirs,
            3 => rgit_diff::merge::Favor::Union,
            _ => rgit_diff::merge::Favor::None,
        },
        conflict_style: config
            .string("merge.conflictstyle")
            .ok()
            .flatten()
            .and_then(|v| rgit_diff::merge::ConflictStyle::from_name(&v.to_string()))
            .unwrap_or_default(),
        ws_action,
        inaccurate_eof: inaccurate_eof.get(),
        ws_rule: config
            .string("core.whitespace")
            .ok()
            .flatten()
            .and_then(|v| rgit_diff::ws_rule::WsRule::parse(&v.to_string()).ok())
            .unwrap_or_default(),
    };
    settings.apply = force_apply.get()
        || !(settings.stat
            || settings.numstat
            || settings.summary
            || settings.check
            || settings.fake_ancestor.is_some());

    let original_cwd = match &repo {
        Some(r) => r.setup.original_cwd.clone(),
        None => std::env::current_dir().unwrap_or_default(),
    };
    let (root, prefix) = match &repo {
        Some(r) if in_worktree => (r.work_tree()?, r.prefix()),
        _ => (original_cwd.clone(), Vec::new()),
    };

    // The index, locked when it may be written.
    let writes_index = settings.apply && (index_wanted || settings.ita);
    // --build-fake-ancestor takes the blobs of patches without index lines
    // (pure renames and mode changes) from the index.
    // Submodules take their commit from the index in every mode.
    let needs_index = repo.is_some();
    let mut lock = None;
    let mut index_data = None;
    let mut checker = None;
    if let (true, Some(r)) = (needs_index, &repo) {
        if writes_index {
            let path = r.setup.cwd.join(r.index_path());
            lock = Some(
                rgit_fs::LockFile::acquire(&path, &r.index_lock_options()).map_err(|e| {
                    if let Some(w) = e.warning() {
                        eprintln!("warning: {w}");
                    }
                    Die::Fatal(e.to_string())
                })?,
            );
        }
        let wanted = index_wanted
            || settings.ita
            || !settings.file_mode
            || settings.fake_ancestor.is_some();
        match r.read_index() {
            Ok((idx, mtime)) => {
                index_data = Some(idx);
                if in_worktree {
                    checker = Some(r.checker(mtime)?);
                }
            }
            Err(e) if wanted => return Err(e),
            Err(_) => {}
        }
    }

    let parse_prefix = prefix.clone();
    let mut state = State {
        s: &settings,
        repo: repo.as_ref(),
        root,
        prefix,
        index: index_data,
        checker,
        index_changed: false,
        conflicted: false,
        ws: WsCount::default(),
        ws_summary_printed: false,
        parse_opts: ParseOptions {
            p_value: p_value.get().unwrap_or(1),
            p_explicit: p_value.get().is_some(),
            root: root_dir,
            recount: recount.get(),
            prefix: parse_prefix,
            names_from_top: false,
        },
    };

    let inputs: Vec<String> = if args.is_empty() {
        vec!["-".to_string()]
    } else {
        args
    };
    let mut code = 0;
    for arg in &inputs {
        let (data, source) = if arg == "-" {
            let mut d = Vec::new();
            std::io::stdin()
                .read_to_end(&mut d)
                .map_err(|e| Die::Error(crate::global::strerror(&e), 128))?;
            (d, "<stdin>".to_string())
        } else {
            let path = original_cwd.join(arg);
            let d = std::fs::read(&path).map_err(|e| {
                Die::Error(msg::cannot_open(arg, &crate::global::strerror(&e)), 128)
            })?;
            (d, arg.clone())
        };
        match state.input(&data, &source) {
            Ok(c) => code |= c,
            Err(Die::Exit(c)) => return Ok(c),
            Err(e) => return Err(e),
        }
        if code != 0 {
            return Ok(code);
        }
    }
    if let (Some(mut lock), Some(index), true) = (lock, &state.index, state.index_changed) {
        let r = repo.as_ref().expect("lock implies repository");
        let skip_hash = r.config.bool("index.skiphash").ok().flatten().unwrap_or(false);
        index
            .write_locked(&mut lock, &WriteOptions { skip_hash })
            .and_then(|()| lock.commit())
            .map_err(|e| Die::Fatal(e.to_string()))?;
    }
    let ws = if state.ws_summary_printed {
        128
    } else {
        state.ws_summary()
    };
    if ws != 0 {
        return Ok(ws);
    }
    Ok(code | i32::from(state.conflicted))
}
