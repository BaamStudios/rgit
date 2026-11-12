// SPDX-License-Identifier: MIT
//! `git update-index` (Documentation/git-update-index.adoc): register
//! work tree files, raw entries and flags in the index.
//!
//! Options and paths are handled strictly in command line order, as git
//! does: an option affects the paths after it, and actions such as
//! `--refresh`, `--cacheinfo` or `--again` run where they appear.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;

use rgit_hash::ObjectId;
use rgit_index::{AddError, AddOptions, Entry, Index, Mode as IndexMode, Stage, WriteOptions};
use rgit_object::{Object, ObjectKind};
use rgit_options::ParseFlags;
use rgit_pathspec::{ParseOptions, Pathspec};
use rgit_worktree::stat::{
    Checker, Status, mode_from_metadata, smudge_racy_entries, stat_from_metadata,
};

use crate::messages::update_index as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::{self, Repo};

/// One option as it appeared on the command line, replayed in order.
#[derive(Debug, Clone)]
enum Event {
    Flag(Flag, bool),
    Refresh { really: bool },
    Cacheinfo(Option<String>),
    Chmod(char),
    Mark(MarkBit, bool),
    Stdin,
    IndexInfo,
    Again,
    IndexVersion(Option<i64>),
    ShowIndexVersion,
    ClearResolveUndo,
    Unsupported(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flag {
    Quiet,
    IgnoreSubmodules,
    Add,
    Replace,
    Remove,
    Unmerged,
    IgnoreSkipWorktree,
    InfoOnly,
    ForceRemove,
    Nul,
    IgnoreMissing,
    Verbose,
    ForceWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkBit {
    AssumeUnchanged,
    SkipWorktree,
}

/// Why processing stopped.
enum Stop {
    /// Print as given and exit.
    Die(Die),
}

impl From<Die> for Stop {
    fn from(d: Die) -> Self {
        Stop::Die(d)
    }
}

fn fatal(m: String) -> Stop {
    Stop::Die(Die::Fatal(m))
}

fn lossy(p: &[u8]) -> String {
    String::from_utf8_lossy(p).into_owned()
}

struct State<'a> {
    repo: &'a Repo,
    head_tree: Option<ObjectId>,
    index: Index,
    index_mtime: Option<rgit_index::Time>,
    work_tree: Option<PathBuf>,
    changed: bool,
    has_errors: bool,
    flags: HashMap<FlagKey, bool>,
    chmod: Option<char>,
    mark: [Option<bool>; 2],
    index_version: Option<i64>,
    out: Vec<u8>,
}

type FlagKey = u8;

fn key(f: Flag) -> FlagKey {
    f as u8
}

impl State<'_> {
    fn flag(&self, f: Flag) -> bool {
        self.flags.get(&key(f)).copied().unwrap_or(false)
    }

    fn say(&mut self, line: String) {
        if self.flag(Flag::Verbose) {
            self.out.extend_from_slice(line.as_bytes());
            self.out.push(b'\n');
        }
    }

    fn flush(&mut self) {
        let mut o = std::io::stdout().lock();
        let _ = o.write_all(&self.out);
        let _ = o.flush();
        self.out.clear();
    }

    fn work_tree(&self) -> Result<PathBuf, Stop> {
        match &self.work_tree {
            Some(w) => Ok(w.clone()),
            None => Err(Stop::Die(self.repo.work_tree().err().unwrap_or(Die::Exit(128)))),
        }
    }

    fn checker(&self) -> Result<Checker, Stop> {
        Ok(Checker::new(
            self.work_tree()?,
            self.repo.hash,
            self.repo.stat_options(),
            self.index_mtime,
        ))
    }

    /// A path from the command line or `--stdin`, relative to the current
    /// directory.
    fn path_arg(&mut self, arg: &[u8]) -> Result<(), Stop> {
        let path = self.repo.prefix_path(arg).map_err(|()| {
            let root = self
                .repo
                .setup
                .work_tree
                .as_ref()
                .map(|w| w.display().to_string())
                .unwrap_or_default();
            fatal(msg::outside_repository(&lossy(arg), &root))
        })?;
        self.update_one(&path)
    }

    /// Handle one path (relative to the top).
    fn update_one(&mut self, path: &[u8]) -> Result<(), Stop> {
        let shown = lossy(path);
        if !rgit_index::verify_path(path) {
            eprintln!("{}", msg::ignoring_path(&shown));
            return Ok(());
        }
        if let Some(set) = self.mark[0] {
            return self.mark_path(path, MarkBit::AssumeUnchanged, set);
        }
        if let Some(set) = self.mark[1] {
            return self.mark_path(path, MarkBit::SkipWorktree, set);
        }
        if self.flag(Flag::ForceRemove) {
            if !self.index.remove_path(path).is_empty() {
                self.changed = true;
            }
            self.say(msg::verbose_remove(&shown));
            return Ok(());
        }
        if let Err(e) = self.process_path(path) {
            if let Some(m) = e {
                eprintln!("error: {m}");
            }
            return Err(fatal(msg::unable_to_process(&shown)));
        }
        self.say(msg::verbose_add(&shown));
        if let Some(flip) = self.chmod {
            self.chmod_path(path, flip)?;
        }
        Ok(())
    }

    fn mark_path(&mut self, path: &[u8], bit: MarkBit, set: bool) -> Result<(), Stop> {
        let Ok(pos) = self.index.position(path, Stage::MERGED) else {
            return Err(fatal(msg::unable_to_mark(&lossy(path))));
        };
        let e = self.index.entry_mut(pos).expect("position is valid");
        let flag = match bit {
            MarkBit::AssumeUnchanged => &mut e.flags.assume_valid,
            MarkBit::SkipWorktree => &mut e.flags.skip_worktree,
        };
        if *flag != set {
            *flag = set;
            self.changed = true;
        }
        Ok(())
    }

    fn chmod_path(&mut self, path: &[u8], flip: char) -> Result<(), Stop> {
        let cannot = || fatal(msg::cannot_chmod(flip, &lossy(path)));
        let pos = self.index.position(path, Stage::MERGED).map_err(|_| cannot())?;
        let e = self.index.entry_mut(pos).expect("position is valid");
        if !e.mode.is_file() {
            return Err(cannot());
        }
        let mode = if flip == '+' {
            IndexMode::EXECUTABLE
        } else {
            IndexMode::FILE
        };
        if e.mode != mode {
            e.mode = mode;
            self.changed = true;
            self.index.invalidate_path(path);
        }
        self.say(msg::verbose_chmod(flip, &lossy(path)));
        Ok(())
    }

    /// Add, update or remove `path` from the work tree. The error is the
    /// `error:` line to print, if any.
    fn process_path(&mut self, path: &[u8]) -> Result<(), Option<String>> {
        let shown = lossy(path);
        let checker = self.checker().map_err(|_| None)?;
        let existing: Vec<Entry> = self.index.entries_for_path(path).to_vec();
        if self.flag(Flag::IgnoreSkipWorktree)
            && existing.iter().any(|e| e.flags.skip_worktree)
        {
            return Ok(());
        }
        let beyond = beyond_symlink(&self.work_tree().map_err(|_| None)?, path);
        if beyond {
            return Err(Some(msg::beyond_symlink(&shown)));
        }
        let meta = match checker.lstat(path) {
            Ok(Some(m)) => m,
            Ok(None) => {
                if !self.flag(Flag::Remove) {
                    return Err(Some(msg::missing_remove(&shown)));
                }
                if !self.index.remove_path(path).is_empty() {
                    self.changed = true;
                }
                return Ok(());
            }
            Err(e) => return Err(Some(format!("lstat(\"{shown}\"): {e}"))),
        };
        let wt = self.work_tree().map_err(|_| None)?;
        let full = os_path(&wt, path);
        let (mode, id) = if meta.is_dir() {
            let gitlink = existing.iter().any(|e| e.mode.is_gitlink());
            let head = repository::submodule_head(&full, self.repo.hash);
            match head {
                Some(h) if gitlink || self.flag(Flag::Add) || !existing.is_empty() => {
                    (IndexMode::GITLINK, h)
                }
                Some(_) => return Err(Some(msg::missing_add(&shown))),
                None if gitlink => return Ok(()),
                None => {
                    let tracked = !self.index.dir_range(path).is_empty();
                    return Err(Some(if tracked {
                        msg::is_directory_tracked(&shown)
                    } else {
                        msg::is_directory(&shown)
                    }));
                }
            }
        } else {
            let old = existing
                .iter()
                .find(|e| e.stage() == Stage::MERGED)
                .or(existing.first())
                .map(|e| e.mode);
            let Some(mode) = mode_from_metadata(&meta, checker.options(), old) else {
                return Err(Some(msg::missing_remove(&shown)));
            };
            let data = if meta.file_type().is_symlink() {
                std::fs::read_link(&full)
                    .map(|t| t.as_os_str().as_encoded_bytes().to_vec())
                    .map_err(|e| Some(format!("readlink(\"{shown}\"): {e}")))?
            } else {
                std::fs::read(&full).map_err(|e| Some(format!("open(\"{shown}\"): {e}")))?
            };
            let id = if self.flag(Flag::InfoOnly) {
                rgit_hash::hash_object(self.repo.hash, "blob", &data)
                    .map_err(|e| Some(e.to_string()))?
            } else {
                self.repo
                    .odb()
                    .write(ObjectKind::Blob, &data)
                    .map_err(|e| Some(e.to_string()))?
            };
            if existing.is_empty() && !self.flag(Flag::Add) {
                return Err(Some(msg::missing_add(&shown)));
            }
            (mode, id)
        };
        let mut entry = Entry::new(path.to_vec(), Stage::MERGED, mode, id);
        entry.stat = stat_from_metadata(&meta);
        if let Some(old) = existing.iter().find(|e| e.stage() == Stage::MERGED) {
            entry.flags = old.flags;
            entry.flags.intent_to_add = false;
            if existing.len() == 1 && old.mode == entry.mode && old.id == entry.id && old.stat == entry.stat && !old.flags.intent_to_add {
                return Ok(());
            }
        }
        self.add_entry(entry, path)
    }

    /// Add a stage-0 entry, dropping the path's other stages.
    fn add_entry(&mut self, entry: Entry, path: &[u8]) -> Result<(), Option<String>> {
        let opts = AddOptions {
            ok_to_replace: self.flag(Flag::Replace),
            ..Default::default()
        };
        match self.index.add(entry, opts) {
            Ok(()) => {}
            Err(AddError::FileDirectoryConflict(_)) => {
                eprintln!("error: {}", msg::file_and_directory(&lossy(path)));
                return Err(Some(msg::missing_add(&lossy(path))));
            }
            Err(e) => return Err(Some(e.to_string())),
        }
        for s in 1..=3 {
            let _ = self.index.remove(path, Stage::new(s).expect("stage 1-3"));
        }
        self.changed = true;
        Ok(())
    }

    fn cacheinfo(&mut self, mode: &str, sha: &str, path: &str) -> Result<(), Stop> {
        let bad = || Stop::Die(Die::Output(usage_error(msg::CACHEINFO_ARGS)));
        let mode = u32::from_str_radix(mode, 8).map_err(|_| bad())?;
        let id = ObjectId::from_hex(self.repo.hash, sha.as_bytes()).map_err(|_| bad())?;
        let p = path.as_bytes();
        if !rgit_index::verify_path(p) {
            eprintln!("error: {}", msg::invalid_path(path));
            return Err(fatal(msg::cacheinfo_cannot_add(path)));
        }
        let mode = canonical_mode(mode);
        let exists = !self.index.path_range(p).is_empty();
        if !exists && !self.flag(Flag::Add) {
            eprintln!("error: {}", msg::missing_add(path));
            return Err(fatal(msg::cacheinfo_cannot_add(path)));
        }
        let entry = Entry::new(p.to_vec(), Stage::MERGED, IndexMode(mode), id);
        if let Err(e) = self.add_entry(entry, p) {
            if let Some(m) = e {
                eprintln!("error: {m}");
            }
            return Err(fatal(msg::cacheinfo_cannot_add(path)));
        }
        self.say(msg::verbose_add(path));
        Ok(())
    }

    fn refresh(&mut self, really: bool) -> Result<(), Stop> {
        let checker = self.checker()?;
        let quiet = self.flag(Flag::Quiet);
        let mut last_merge: Option<Vec<u8>> = None;
        let mut lines = Vec::new();
        let mut merges = Vec::new();
        for pos in 0..self.index.len() {
            let e = self.index.entries()[pos].clone();
            if e.stage().is_conflict() {
                if self.flag(Flag::Unmerged) {
                    continue;
                }
                if last_merge.as_deref() != Some(e.path()) {
                    last_merge = Some(e.path().to_vec());
                    merges.push(msg::needs_merge(&lossy(e.path())));
                }
                continue;
            }
            if e.flags.skip_worktree || (e.flags.assume_valid && !really) {
                continue;
            }
            if e.flags.assume_valid {
                // --really-refresh drops the assume-unchanged bit.
                self.index.entry_mut(pos).expect("valid").flags.assume_valid = false;
                self.changed = true;
            }
            if e.mode.is_gitlink() {
                if self.flag(Flag::IgnoreSubmodules) {
                    continue;
                }
                let full = os_path(&self.work_tree()?, e.path());
                match repository::submodule_head(&full, self.repo.hash) {
                    Some(h) if h != e.id => lines.push(msg::needs_update(&lossy(e.path()))),
                    _ => {}
                }
                continue;
            }
            let meta = match checker.lstat(e.path()) {
                Ok(Some(m)) => m,
                _ => {
                    if !self.flag(Flag::IgnoreMissing) {
                        lines.push(msg::needs_update(&lossy(e.path())));
                    }
                    continue;
                }
            };
            match checker.check_with(&e, &meta) {
                Ok(Status::Unchanged) => {}
                Ok(Status::StatOnly) => {
                    let entry = self.index.entry_mut(pos).expect("valid");
                    checker.refresh(entry, &meta);
                    self.changed = true;
                }
                _ => lines.push(msg::needs_update(&lossy(e.path()))),
            }
        }
        if !merges.is_empty() {
            self.has_errors = true;
        }
        if quiet {
            lines.clear();
        }
        // git reports both kinds in index order.
        lines.extend(merges);
        lines.sort_by(|a, b| a.rsplit_once(": ").map(|x| x.0).cmp(&b.rsplit_once(": ").map(|x| x.0)));
        if !lines.is_empty() {
            for l in lines {
                self.out.extend_from_slice(l.as_bytes());
                self.out.push(b'\n');
            }
            self.has_errors = true;
        }
        Ok(())
    }

    /// `--again`: update the entries that differ from `HEAD`.
    fn again(&mut self, ctx: &Context, args: &[String]) -> Result<(), Stop> {
        let mut head: HashMap<Vec<u8>, (u32, ObjectId)> = HashMap::new();
        if let Some(tree) = self.head_tree {
            flatten_tree(self.repo, &tree, b"", &mut head);
        }
        let opts = ParseOptions {
            prefix: self.repo.prefix(),
            root: self
                .repo
                .setup
                .work_tree
                .as_ref()
                .map(|w| w.as_os_str().as_encoded_bytes().to_vec()),
            globals: repository::pathspec_globals(ctx),
            ..Default::default()
        };
        let ps = Pathspec::parse(args, &opts).map_err(|e| fatal(e.to_string()))?;
        let prefix = self.repo.prefix();
        let paths: Vec<Vec<u8>> = self
            .index
            .entries()
            .iter()
            .filter(|e| e.stage() == Stage::MERGED)
            .filter(|e| {
                if args.is_empty() {
                    e.path().starts_with(&prefix)
                } else {
                    ps.matches(e.path())
                }
            })
            .filter(|e| head.get(e.path()) != Some(&(e.mode.0, e.id)))
            .map(|e| e.path().to_vec())
            .collect();
        for p in paths {
            self.update_one(&p)?;
        }
        Ok(())
    }

    fn read_stdin(&self) -> Vec<Vec<u8>> {
        let mut data = Vec::new();
        let _ = std::io::stdin().lock().read_to_end(&mut data);
        let sep = if self.flag(Flag::Nul) { 0 } else { b'\n' };
        let mut lines: Vec<Vec<u8>> = data.split(|&b| b == sep).map(|l| l.to_vec()).collect();
        if lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines
    }

    fn unquote(&self, line: Vec<u8>) -> Result<Vec<u8>, Stop> {
        if self.flag(Flag::Nul) || !line.starts_with(b"\"") {
            return Ok(line);
        }
        rgit_pathspec::unquote_c_style(&line).ok_or_else(|| fatal(msg::badly_quoted(&lossy(&line))))
    }

    fn stdin_paths(&mut self) -> Result<(), Stop> {
        for line in self.read_stdin() {
            let path = self.unquote(line)?;
            self.path_arg(&path)?;
        }
        Ok(())
    }

    fn index_info(&mut self) -> Result<(), Stop> {
        let hash = self.repo.hash;
        for raw in self.read_stdin() {
            let malformed = || fatal(msg::malformed_index_info(&lossy(&raw)));
            let Some(tab) = raw.iter().position(|&b| b == b'\t') else {
                return Err(malformed());
            };
            let (meta, rest) = (&raw[..tab], raw[tab + 1..].to_vec());
            let path = self.unquote(rest)?;
            let meta = std::str::from_utf8(meta).map_err(|_| malformed())?;
            let toks: Vec<&str> = meta.split(' ').collect();
            let (mode, sha, stage) = match toks.as_slice() {
                [m, s] => (*m, *s, 0u8),
                [m, a, b] if b.len() == 1 && b.as_bytes()[0].is_ascii_digit() => {
                    (*m, *a, b.as_bytes()[0] - b'0')
                }
                [m, t, s] if ObjectKind::from_name(t.as_bytes()).is_some() => (*m, *s, 0),
                _ => return Err(malformed()),
            };
            let mode = u32::from_str_radix(mode, 8).map_err(|_| malformed())?;
            let id = ObjectId::from_hex(hash, sha.as_bytes()).map_err(|_| malformed())?;
            let stage = Stage::new(stage).ok_or_else(malformed)?;
            if !rgit_index::verify_path(&path) {
                return Err(fatal(msg::malformed_index_info(&lossy(&raw))));
            }
            if mode == 0 {
                if !self.index.remove_path(&path).is_empty() {
                    self.changed = true;
                }
                continue;
            }
            let entry = Entry::new(path.clone(), stage, IndexMode(canonical_mode(mode)), id);
            let opts = AddOptions {
                ok_to_replace: true,
                ..Default::default()
            };
            if stage == Stage::MERGED {
                if let Err(e) = self.add_entry(entry, &path) {
                    if let Some(m) = e {
                        eprintln!("error: {m}");
                    }
                    return Err(fatal(msg::malformed_index_info(&lossy(&raw))));
                }
            } else {
                let _ = self.index.remove(&path, Stage::MERGED);
                self.index
                    .add(entry, opts)
                    .map_err(|_| fatal(msg::malformed_index_info(&lossy(&raw))))?;
                self.changed = true;
            }
            self.say(msg::verbose_add(&lossy(&path)));
        }
        Ok(())
    }

    fn write(&mut self) -> Result<(), Stop> {
        for e in self.index.entries() {
            if e.id.is_null() {
                eprintln!("error: {}", msg::null_sha1(&lossy(e.path())));
                return Err(fatal(msg::WRITE_FAILED.to_string()));
            }
        }
        if let (Some(reference), Ok(checker)) = (self.index_mtime, self.checker()) {
            let _ = smudge_racy_entries(&mut self.index, reference, &checker);
        }
        let path = self.repo.setup.cwd.join(self.repo.index_path());
        let skip_hash = self
            .repo
            .config
            .bool("index.skiphash")
            .ok()
            .flatten()
            .unwrap_or(false);
        let opts = WriteOptions { skip_hash };
        match self
            .index
            .write_file(&path, &rgit_fs::LockOptions::default(), &opts)
        {
            Ok(()) => Ok(()),
            Err(rgit_index::WriteError::Lock(e)) => {
                if let Some(w) = e.warning() {
                    eprintln!("warning: {w}");
                }
                Err(fatal(e.to_string()))
            }
            Err(e) => Err(fatal(e.to_string())),
        }
    }
}

fn usage_error(m: &str) -> rgit_options::Exit {
    rgit_options::Exit {
        code: rgit_options::EXIT_USAGE,
        stdout: String::new(),
        stderr: format!("error: {m}\n"),
    }
}

/// The mode git stores for a raw mode: regular files become 100644 or
/// 100755.
fn canonical_mode(mode: u32) -> u32 {
    match mode & 0o170000 {
        0o100000 if mode & 0o111 != 0 => 0o100755,
        0o100000 => 0o100644,
        0o120000 => 0o120000,
        0o160000 => 0o160000,
        0o040000 => 0o040000,
        _ => mode,
    }
}

fn os_path(root: &std::path::Path, rel: &[u8]) -> PathBuf {
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

/// Whether a leading directory of `path` is a symbolic link (or not a
/// directory).
fn beyond_symlink(root: &std::path::Path, path: &[u8]) -> bool {
    for (i, &b) in path.iter().enumerate() {
        if b != b'/' {
            continue;
        }
        match std::fs::symlink_metadata(os_path(root, &path[..i])) {
            Ok(m) if m.file_type().is_symlink() => return true,
            _ => {}
        }
    }
    false
}

fn flatten_tree(
    repo: &Repo,
    tree: &ObjectId,
    base: &[u8],
    out: &mut HashMap<Vec<u8>, (u32, ObjectId)>,
) {
    let Some(Object::Tree(t)) = repo.read_object(tree) else {
        return;
    };
    for e in t.entries {
        let mut path = base.to_vec();
        path.extend_from_slice(&e.name);
        let mode = e.mode.canonical();
        if mode.is_tree() {
            path.push(b'/');
            flatten_tree(repo, &e.oid, &path, out);
        } else {
            out.insert(path, (mode.bits(), e.oid));
        }
    }
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let events: RefCell<Vec<Event>> = RefCell::new(Vec::new());
    let push = |e: Event| events.borrow_mut().push(e);
    let flag = |f: Flag| move |_: Option<&str>, neg: bool| {
        push(Event::Flag(f, !neg));
        Ok(())
    };
    let opts = vec![
        Opt::callback('q', None, None, msg::HELP_Q, flag(Flag::Quiet))
            .noarg()
            .noneg(),
        Opt::callback(
            None,
            "ignore-submodules",
            None,
            msg::HELP_IGNORE_SUBMODULES,
            flag(Flag::IgnoreSubmodules),
        )
        .noarg(),
        Opt::callback(None, "add", None, msg::HELP_ADD, flag(Flag::Add)).noarg(),
        Opt::callback(None, "replace", None, msg::HELP_REPLACE, flag(Flag::Replace)).noarg(),
        Opt::callback(None, "remove", None, msg::HELP_REMOVE, flag(Flag::Remove)).noarg(),
        Opt::callback(None, "unmerged", None, msg::HELP_UNMERGED, flag(Flag::Unmerged)).noarg(),
        Opt::callback(None, "refresh", None, msg::HELP_REFRESH, |_, _| {
            push(Event::Refresh { really: false });
            Ok(())
        })
        .noarg()
        .noneg(),
        Opt::callback(
            None,
            "really-refresh",
            None,
            msg::HELP_REALLY_REFRESH,
            |_, _| {
                push(Event::Refresh { really: true });
                Ok(())
            },
        )
        .noarg()
        .noneg(),
        Opt::callback(
            None,
            "cacheinfo",
            Some("<mode>,<object>,<path>"),
            msg::HELP_CACHEINFO,
            |v, _| {
                push(Event::Cacheinfo(v.map(str::to_string)));
                Ok(())
            },
        )
        .literal_argh()
        .noneg(),
        Opt::callback(None, "chmod", Some("(+|-)x"), msg::HELP_CHMOD, |v, _| {
            match v {
                Some("+x") => push(Event::Chmod('+')),
                Some("-x") => push(Event::Chmod('-')),
                _ => return Err(CallbackError::Error(msg::CHMOD_ARG.to_string())),
            }
            Ok(())
        })
        .literal_argh()
        .noneg(),
        Opt::callback(
            None,
            "assume-unchanged",
            None,
            msg::HELP_ASSUME_UNCHANGED,
            |_, _| {
                push(Event::Mark(MarkBit::AssumeUnchanged, true));
                Ok(())
            },
        )
        .noarg()
        .noneg(),
        Opt::callback(
            None,
            "no-assume-unchanged",
            None,
            msg::HELP_NO_ASSUME_UNCHANGED,
            |_, _| {
                push(Event::Mark(MarkBit::AssumeUnchanged, false));
                Ok(())
            },
        )
        .noarg()
        .noneg(),
        Opt::callback(
            None,
            "skip-worktree",
            None,
            msg::HELP_SKIP_WORKTREE,
            |_, _| {
                push(Event::Mark(MarkBit::SkipWorktree, true));
                Ok(())
            },
        )
        .noarg()
        .noneg(),
        Opt::callback(
            None,
            "no-skip-worktree",
            None,
            msg::HELP_NO_SKIP_WORKTREE,
            |_, _| {
                push(Event::Mark(MarkBit::SkipWorktree, false));
                Ok(())
            },
        )
        .noarg()
        .noneg(),
        Opt::callback(
            None,
            "ignore-skip-worktree-entries",
            None,
            msg::HELP_IGNORE_SKIP_WORKTREE,
            flag(Flag::IgnoreSkipWorktree),
        )
        .noarg(),
        Opt::callback(None, "info-only", None, msg::HELP_INFO_ONLY, flag(Flag::InfoOnly)).noarg(),
        Opt::callback(
            None,
            "force-remove",
            None,
            msg::HELP_FORCE_REMOVE,
            flag(Flag::ForceRemove),
        )
        .noarg(),
        Opt::callback('z', None, None, msg::HELP_Z, flag(Flag::Nul))
            .noarg()
            .noneg(),
        Opt::callback(None, "stdin", None, msg::HELP_STDIN, |_, _| {
            push(Event::Stdin);
            Ok(())
        })
        .noarg()
        .noneg(),
        Opt::callback(None, "index-info", None, msg::HELP_INDEX_INFO, |_, _| {
            push(Event::IndexInfo);
            Ok(())
        })
        .noarg()
        .noneg(),
        Opt::callback(None, "unresolve", None, msg::HELP_UNRESOLVE, |_, _| {
            push(Event::Unsupported("--unresolve"));
            Ok(())
        })
        .noarg()
        .noneg(),
        Opt::callback('g', "again", None, msg::HELP_AGAIN, |_, _| {
            push(Event::Again);
            Ok(())
        })
        .noarg()
        .noneg(),
        Opt::callback(
            None,
            "ignore-missing",
            None,
            msg::HELP_IGNORE_MISSING,
            flag(Flag::IgnoreMissing),
        )
        .noarg(),
        Opt::callback(None, "verbose", None, msg::HELP_VERBOSE, flag(Flag::Verbose)).noarg(),
        Opt::callback(
            None,
            "clear-resolve-undo",
            None,
            msg::HELP_CLEAR_RESOLVE_UNDO,
            |_, _| {
                push(Event::ClearResolveUndo);
                Ok(())
            },
        )
        .noarg()
        .noneg(),
        Opt::callback(
            None,
            "index-version",
            Some("n"),
            msg::HELP_INDEX_VERSION,
            |v, neg| {
                if neg {
                    push(Event::IndexVersion(None));
                    return Ok(());
                }
                let v = v.unwrap_or_default();
                match v.parse::<i64>() {
                    Ok(n) => push(Event::IndexVersion(Some(n))),
                    Err(_) => {
                        return Err(CallbackError::Error(crate::messages::abbrev_not_numeric(
                            "index-version",
                        )));
                    }
                }
                Ok(())
            },
        ),
        Opt::callback(
            None,
            "show-index-version",
            None,
            msg::HELP_SHOW_INDEX_VERSION,
            |_, neg| {
                if !neg {
                    push(Event::ShowIndexVersion);
                }
                Ok(())
            },
        )
        .noarg(),
        Opt::callback(None, "split-index", None, msg::HELP_SPLIT_INDEX, |_, neg| {
            if !neg {
                push(Event::Unsupported("--split-index"));
            }
            Ok(())
        })
        .noarg(),
        Opt::callback(
            None,
            "untracked-cache",
            None,
            msg::HELP_UNTRACKED_CACHE,
            |_, neg| {
                if !neg {
                    push(Event::Unsupported("--untracked-cache"));
                }
                Ok(())
            },
        )
        .noarg(),
        Opt::callback(
            None,
            "test-untracked-cache",
            None,
            msg::HELP_TEST_UNTRACKED_CACHE,
            |_, neg| {
                if !neg {
                    push(Event::Unsupported("--test-untracked-cache"));
                }
                Ok(())
            },
        )
        .noarg(),
        Opt::callback(
            None,
            "force-untracked-cache",
            None,
            msg::HELP_FORCE_UNTRACKED_CACHE,
            |_, neg| {
                if !neg {
                    push(Event::Unsupported("--force-untracked-cache"));
                }
                Ok(())
            },
        )
        .noarg(),
        Opt::callback(
            None,
            "force-write-index",
            None,
            msg::HELP_FORCE_WRITE,
            flag(Flag::ForceWrite),
        )
        .noarg(),
        Opt::callback(None, "fsmonitor", None, msg::HELP_FSMONITOR, |_, neg| {
            if !neg {
                push(Event::Unsupported("--fsmonitor"));
            }
            Ok(())
        })
        .noarg(),
        Opt::callback(
            None,
            "fsmonitor-valid",
            None,
            msg::HELP_FSMONITOR_VALID,
            |_, _| {
                push(Event::Unsupported("--fsmonitor-valid"));
                Ok(())
            },
        )
        .noarg()
        .noneg(),
        Opt::callback(
            None,
            "no-fsmonitor-valid",
            None,
            msg::HELP_NO_FSMONITOR_VALID,
            |_, _| {
                push(Event::Unsupported("--no-fsmonitor-valid"));
                Ok(())
            },
        )
        .noarg()
        .noneg(),
    ];
    let mut parser = Parser::new(msg::USAGE, opts).flags(ParseFlags {
        stop_at_non_option: true,
        keep_dashdash: true,
        keep_argv0: false,
        ..Default::default()
    });

    let cmd = argv[0].clone();
    let parse = |parser: &mut Parser<'_>, rest: &[String]| -> Result<Vec<String>, Die> {
        let mut v = vec![cmd.clone()];
        v.extend(rest.iter().cloned());
        parser.parse(&v).map(|p| p.args).map_err(|mut e| {
            // update-index quotes unknown options as 'name'.
            for prefix in ["error: unknown option `", "error: unknown switch `"] {
                if let Some(rest) = e.stderr.strip_prefix(prefix) {
                    if let Some(end) = rest.find('\'') {
                        e.stderr = format!("{}'{}'{}", &prefix[..prefix.len() - 1], &rest[..end], &rest[end + 1..]);
                    }
                }
            }
            if e.stderr.starts_with("error: option `cacheinfo' requires a value") {
                e.stderr = format!("error: {}\n", msg::CACHEINFO_ARGS);
            }
            Die::Output(e)
        })
    };
    let mut rest = parse(&mut parser, &argv[1..])?;

    let mut repo = Repo::open(ctx, Mode::RequiredQuiet)?;
    let head_tree = repo
        .resolve("HEAD")
        .and_then(|id| repo.peel_to(id, ObjectKind::Tree));
    let repo = repo;
    let (index, index_mtime) = repo.read_index()?;
    let mut st = State {
        repo: &repo,
        head_tree,
        index,
        index_mtime,
        work_tree: repo.setup.work_tree.clone(),
        changed: false,
        has_errors: false,
        flags: HashMap::new(),
        chmod: None,
        mark: [None, None],
        index_version: None,
        out: Vec::new(),
    };
    let ctx: &Context = ctx;
    let mut only_paths = false;
    let result: Result<(), Stop> = (|| {
        loop {
            let batch: Vec<Event> = std::mem::take(&mut *events.borrow_mut());
            let n = batch.len();
            for (i, ev) in batch.into_iter().enumerate() {
                let last_in_batch = i + 1 == n;
                st.replay(ev, last_in_batch, &mut rest, ctx)?;
            }
            if rest.is_empty() {
                break;
            }
            let arg = rest.remove(0);
            if arg == "--" && !only_paths {
                only_paths = true;
            } else {
                st.path_arg(arg.as_bytes())?;
            }
            if !only_paths && !rest.is_empty() {
                rest = parse(&mut parser, &rest).map_err(Stop::Die)?;
            }
        }
        Ok(())
    })();

    if let Err(Stop::Die(d)) = result {
        st.flush();
        return Err(d);
    }
    if let Some(v) = st.index_version {
        if !(2..=4).contains(&v) {
            st.flush();
            return Err(Die::Fatal(msg::index_version_range(v)));
        }
        let old = st.index.version();
        st.index.set_version(v as u32);
        st.changed = true;
        st.say(msg::index_version_changed(old, v as u32));
    }
    if st.changed || st.flag(Flag::ForceWrite) {
        if let Err(Stop::Die(d)) = st.write() {
            st.flush();
            return Err(d);
        }
    }
    st.flush();
    Ok(if st.has_errors { 1 } else { 0 })
}

impl State<'_> {
    /// Apply one option event. `rest` holds the arguments after the batch
    /// it came from.
    fn replay(
        &mut self,
        ev: Event,
        last_in_batch: bool,
        rest: &mut Vec<String>,
        ctx: &Context,
    ) -> Result<(), Stop> {
        let last = last_in_batch && rest.is_empty();
        match ev {
            Event::Flag(f, on) => {
                self.flags.insert(key(f), on);
            }
            Event::Refresh { really } => self.refresh(really)?,
            Event::Cacheinfo(Some(v)) => {
                let parts: Vec<&str> = v.splitn(3, ',').collect();
                if let [m, s, p] = parts.as_slice() {
                    self.cacheinfo(m, s, p)?;
                } else if last_in_batch && rest.len() >= 2 {
                    let (s, p) = (rest.remove(0), rest.remove(0));
                    self.cacheinfo(&v, &s, &p)?;
                } else {
                    return Err(Stop::Die(Die::Output(usage_error(msg::CACHEINFO_ARGS))));
                }
            }
            Event::Cacheinfo(None) => {
                let bad = || Stop::Die(Die::Output(usage_error(msg::CACHEINFO_ARGS)));
                if !last_in_batch || rest.is_empty() {
                    return Err(bad());
                }
                if rest[0].matches(',').count() >= 2 {
                    let v = rest.remove(0);
                    let parts: Vec<&str> = v.splitn(3, ',').collect();
                    self.cacheinfo(parts[0], parts[1], parts[2])?;
                } else if rest.len() >= 3 {
                    let (m, sh, p) = (rest.remove(0), rest.remove(0), rest.remove(0));
                    self.cacheinfo(&m, &sh, &p)?;
                } else {
                    return Err(bad());
                }
            }
            Event::Chmod(c) => self.chmod = Some(c),
            Event::Mark(bit, set) => {
                let idx = match bit {
                    MarkBit::AssumeUnchanged => 0,
                    MarkBit::SkipWorktree => 1,
                };
                self.mark[idx] = Some(set);
            }
            Event::Stdin => {
                if !last {
                    return Err(Stop::Die(Die::Output(usage_error(&msg::must_be_last(
                        "stdin",
                    )))));
                }
                self.stdin_paths()?;
            }
            Event::IndexInfo => {
                if !last {
                    return Err(Stop::Die(Die::Output(usage_error(&msg::must_be_last(
                        "index-info",
                    )))));
                }
                self.index_info()?;
            }
            Event::Again => {
                let args: Vec<String> = if last_in_batch {
                    std::mem::take(rest)
                        .into_iter()
                        .filter(|a| a != "--")
                        .collect()
                } else {
                    Vec::new()
                };
                self.again(ctx, &args)?;
            }
            Event::IndexVersion(v) => self.index_version = v,
            Event::ShowIndexVersion => {
                let line = format!("{}\n", self.index.version());
                self.out.extend_from_slice(line.as_bytes());
            }
            Event::ClearResolveUndo => {
                if self.index.resolve_undo_raw().is_some() {
                    self.index.clear_resolve_undo();
                    self.changed = true;
                }
            }
            Event::Unsupported(what) => return Err(fatal(msg::not_supported(what))),
        }
        Ok(())
    }
}
