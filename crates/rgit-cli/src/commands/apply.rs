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
//! Not done yet (#150): `--3way`, binary patches, whitespace warnings and
//! fixes (`--whitespace`, `--ignore-space-change` are accepted and
//! ignored), `--inaccurate-eof` (accepted, ignored). Submodule (gitlink)
//! patches are not supported.

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
        if created {
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
        if p.binary {
            let full = |o: &Option<String>| o.as_ref().is_some_and(|h| h.len() == self.hash().hex_len());
            let m = if !full(&p.old_oid) || !full(&p.new_oid) {
                msg::binary_without_index(&name_for_fail)
            } else {
                msg::binary_unsupported(&name_for_fail)
            };
            eprintln!("error: {m}");
            return Err(fail(msg::does_not_apply(&name_for_fail)));
        }
        let applied = rgit_apply::apply_hunks(&image, &p.hunks, &self.s.match_opts);
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
        let content = applied.content;
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
                let _ = rgit_unpack::remove_entry(&self.root, path, false);
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
            if to_index {
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
            if !self.prefix.is_empty() && !p.is_git {
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
        if self.s.check || self.s.apply {
            let plans = self.check(&mut patches)?;
            if self.s.apply && !self.write(&patches, &plans)? {
                code = 1;
            }
        }
        if let Some(file) = &self.s.fake_ancestor {
            self.fake_ancestor(&patches, file)?;
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
    if threeway.get() {
        return Err(Die::Fatal(msg::THREEWAY_UNSUPPORTED.into()));
    }

    let repo = Repo::open_gently(ctx)?;
    let in_worktree = repo
        .as_ref()
        .is_some_and(|r| r.setup.work_tree.is_some() && !r.setup.is_inside_git_dir());
    let (index_wanted, cached_wanted) = (index.get() || cached.get(), cached.get());
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

    let quiet_v = quiet.get() > 0;
    let mut settings = Settings {
        stat: stat.get(),
        numstat: numstat.get(),
        summary: summary.get(),
        check: check.get(),
        index: index.get() && !cached.get(),
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
        },
        filters: filters.take(),
        fake_ancestor: fake_ancestor.take(),
        quote_path,
        file_mode,
        symlinks,
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
    let needs_index = repo.is_some() && (index_wanted || settings.ita || !settings.file_mode);
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
        let (idx, mtime) = r.read_index()?;
        index_data = Some(idx);
        if in_worktree {
            checker = Some(r.checker(mtime)?);
        }
    }

    let mut state = State {
        s: &settings,
        repo: repo.as_ref(),
        root,
        prefix,
        index: index_data,
        checker,
        index_changed: false,
        parse_opts: ParseOptions {
            p_value: p_value.get().unwrap_or(1),
            p_explicit: p_value.get().is_some(),
            root: root_dir,
            recount: recount.get(),
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
    Ok(code)
}
