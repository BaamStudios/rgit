// SPDX-License-Identifier: MIT
//! `git ls-files` (Documentation/git-ls-files.adoc): show index entries
//! (cached, deleted, modified, unmerged) and work tree files (others,
//! ignored, killed).

use std::cell::Cell;
use std::io::Write;
use std::path::{Path, PathBuf};

use rgit_hash::ObjectId;
use rgit_index::{Entry, Index, Mode as IndexMode, Stage};
use rgit_object::{Object, ObjectKind};
use rgit_pathspec::{ParseOptions, Pathspec};
use rgit_worktree::ignore::{Excludes, default_excludes_file};
use rgit_worktree::stat::{Checker, Status};

use crate::abbrev::Abbrev;
use crate::attributes::PathspecAttrs;
use crate::format;
use crate::messages::ls_files as msg;
use crate::prelude::*;
use crate::quote::{relative_to, write_quoted};
use crate::repo::Mode;
use crate::repository::{self, Repo};

/// A field of `--format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Mode,
    Type,
    Name,
    Size,
    SizePadded,
    Stage,
    Path,
}

fn field_named(name: &str) -> Option<Field> {
    Some(match name {
        "objectmode" => Field::Mode,
        "objecttype" => Field::Type,
        "objectname" => Field::Name,
        "objectsize" => Field::Size,
        "objectsize:padded" => Field::SizePadded,
        "stage" => Field::Stage,
        "path" => Field::Path,
        _ => return None,
    })
}

/// An exclude source, in command line order.
#[derive(Debug, Clone)]
pub(crate) enum ExcludeArg {
    Pattern(String),
    File(String),
    PerDirectory(Option<String>),
    Standard,
}

/// What to list and how.
#[derive(Default)]
struct Show {
    cached: bool,
    deleted: bool,
    modified: bool,
    others: bool,
    ignored: bool,
    stage: bool,
    unmerged: bool,
    killed: bool,
    directory: bool,
    no_empty_directory: bool,
    tags: bool,
    lower_assume: bool,
    nul: bool,
    full_name: bool,
    error_unmatch: bool,
    debug: bool,
    dedup: bool,
}

struct Lister<'a> {
    repo: &'a Repo,
    show: Show,
    prefix: Vec<u8>,
    pathspec: Pathspec,
    /// No pathspec was given: only paths below the prefix are shown.
    implicit: bool,
    seen: Vec<bool>,
    /// Evaluates `attr:` pathspec magic, when there is any.
    attrs: Option<RefCell<PathspecAttrs<'a>>>,
    quote_high: bool,
    abbrev: usize,
    format: Option<Result<Vec<format::Piece<Field>>, String>>,
    out: Vec<u8>,
    last_path: Option<Vec<u8>>,
}

impl Lister<'_> {
    /// Whether `path` is selected.
    fn wanted(&self, path: &[u8]) -> bool {
        if self.implicit {
            return path.starts_with(&self.prefix);
        }
        if self.pathspec.is_empty() {
            return true;
        }
        match &self.attrs {
            Some(a) => self
                .pathspec
                .match_path_with(path, None, &mut |p, spec| a.borrow_mut().check(p, spec))
                .is_some(),
            None => self.pathspec.match_path(path).is_some(),
        }
    }

    /// Record which pathspec items matched a path that was shown (for
    /// `--error-unmatch`).
    fn mark_seen(&mut self, path: &[u8]) {
        if !self.implicit && !self.pathspec.is_empty() {
            match &self.attrs {
                Some(a) => {
                    let _ = self.pathspec.match_path_with(path, Some(&mut self.seen), &mut |p, spec| {
                        a.borrow_mut().check(p, spec)
                    });
                }
                None => {
                    let _ = self.pathspec.match_path_seen(path, &mut self.seen);
                }
            }
        }
    }

    /// Whether anything below the directory `dir` (no trailing slash) can
    /// be selected.
    fn dir_wanted(&self, dir: &[u8]) -> bool {
        if self.implicit {
            let mut d = dir.to_vec();
            d.push(b'/');
            return d.starts_with(&self.prefix) || self.prefix.starts_with(&d);
        }
        self.pathspec.dir_interest(dir) != rgit_pathspec::DirInterest::None
    }

    fn write_path(&mut self, path: &[u8]) {
        let shown = if self.show.full_name {
            path.to_vec()
        } else {
            relative_to(path, &self.prefix)
        };
        if self.show.nul {
            self.out.extend_from_slice(&shown);
        } else {
            write_quoted(&mut self.out, &shown, self.quote_high);
        }
    }

    fn end_line(&mut self) {
        self.out.push(if self.show.nul { 0 } else { b'\n' });
        if self.out.len() >= 64 * 1024 {
            self.flush();
        }
    }

    fn flush(&mut self) {
        let mut o = std::io::stdout().lock();
        let _ = o.write_all(&self.out);
        let _ = o.flush();
        self.out.clear();
    }

    /// A work tree path (others, killed).
    fn show_file(&mut self, tag: &str, path: &[u8]) {
        let bare = path.strip_suffix(b"/").unwrap_or(path).to_vec();
        self.mark_seen(&bare);
        if self.show.tags {
            self.out.extend_from_slice(tag.as_bytes());
            self.out.push(b' ');
        }
        self.write_path(path);
        self.end_line();
    }

    fn name(&self, id: &ObjectId) -> String {
        if self.abbrev == 0 {
            id.to_hex()
        } else {
            self.repo.abbrev(id, self.abbrev)
        }
    }

    fn kind_of(mode: IndexMode) -> ObjectKind {
        if mode.is_gitlink() {
            ObjectKind::Commit
        } else if mode.is_dir() {
            ObjectKind::Tree
        } else {
            ObjectKind::Blob
        }
    }

    fn size(&self, e: &Entry) -> Result<String, Die> {
        if Self::kind_of(e.mode) != ObjectKind::Blob {
            return Ok("-".to_string());
        }
        match self.repo.odb().header(&e.id) {
            Ok(Some(h)) => Ok(h.size.to_string()),
            _ => Err(Die::Fatal(msg::no_object_info(&e.id.to_hex()))),
        }
    }

    /// An index entry with the tag `tag`.
    fn show_entry(&mut self, tag: &str, e: &Entry) -> Result<(), Die> {
        self.mark_seen(e.path());
        if self.show.dedup && !self.show.tags && !self.show.stage {
            if self.last_path.as_deref() == Some(e.path()) {
                return Ok(());
            }
            self.last_path = Some(e.path().to_vec());
        }
        if let Some(fmt) = &self.format {
            let pieces = fmt.clone().map_err(Die::Fatal)?;
            let start = self.out.len();
            for p in pieces {
                match p {
                    format::Piece::Literal(l) => self.out.extend_from_slice(&l),
                    format::Piece::Field(f) => {
                        if let Err(d) = self.field(f, e) {
                            self.out.truncate(start);
                            return Err(d);
                        }
                    }
                }
            }
            self.end_line();
            self.debug_block(e);
            return Ok(());
        }
        if self.show.tags {
            let mut t = tag.to_string();
            if self.show.lower_assume && e.flags.assume_valid {
                t = t.to_lowercase();
            }
            self.out.extend_from_slice(t.as_bytes());
            self.out.push(b' ');
        }
        if self.show.stage {
            let line = format!(
                "{:06o} {} {}\t",
                e.mode.0,
                self.name(&e.id),
                e.stage().get()
            );
            self.out.extend_from_slice(line.as_bytes());
        }
        self.write_path(e.path());
        self.end_line();
        self.debug_block(e);
        Ok(())
    }

    /// `--debug`: the cached stat data and flags after the entry's line.
    fn debug_block(&mut self, e: &Entry) {
        if self.show.debug {
            let s = &e.stat;
            let text = format!(
                "  ctime: {}:{}\n  mtime: {}:{}\n  dev: {}\tino: {}\n  uid: {}\tgid: {}\n  size: {}\tflags: {:x}\n",
                s.ctime.secs,
                s.ctime.nsecs,
                s.mtime.secs,
                s.mtime.nsecs,
                s.dev,
                s.ino,
                s.uid,
                s.gid,
                s.size,
                e.debug_flags()
            );
            self.out.extend_from_slice(text.as_bytes());
        }
    }

    fn field(&mut self, f: Field, e: &Entry) -> Result<(), Die> {
        let text = match f {
            Field::Mode => format!("{:06o}", e.mode.0),
            Field::Type => Self::kind_of(e.mode).name().to_string(),
            Field::Name => self.name(&e.id),
            Field::Size => self.size(e)?,
            Field::SizePadded => format!("{:>7}", self.size(e)?),
            Field::Stage => e.stage().get().to_string(),
            Field::Path => {
                self.write_path(e.path());
                return Ok(());
            }
        };
        self.out.extend_from_slice(text.as_bytes());
        Ok(())
    }

    /// The index part of the listing.
    fn show_index(
        &mut self,
        entries: &[Entry],
        checker: Option<&Checker>,
        excludes: &mut Option<Excludes>,
    ) -> Result<(), Die> {
        let show_cached = self.show.cached || self.show.stage || self.show.unmerged;
        for e in entries {
            if !self.wanted(e.path()) {
                continue;
            }
            if self.show.ignored && self.show.cached {
                let ex = excludes.as_mut().expect("excludes for -i");
                let excluded = ex
                    .match_with_leading_dirs(e.path(), e.mode.is_gitlink())
                    .is_some_and(|m| m.is_excluded());
                if !excluded {
                    continue;
                }
            }
            if show_cached && (!self.show.unmerged || e.stage().is_conflict()) {
                let tag = if e.stage().is_conflict() {
                    "M"
                } else if e.flags.skip_worktree {
                    "S"
                } else {
                    "H"
                };
                self.show_entry(tag, e)?;
            }
            if !(self.show.deleted || self.show.modified) || e.flags.skip_worktree {
                continue;
            }
            let checker = checker.expect("work tree for -d/-m");
            let deleted = checker
                .lstat(e.path())
                .map(|m| m.is_none())
                .unwrap_or(false);
            if self.show.deleted && deleted {
                self.show_entry("R", e)?;
            }
            if self.show.modified {
                let gitlink_moved = e.mode.is_gitlink() && !deleted && {
                    let dir = checker_root(self.repo).join(lossy_os(e.path()));
                    repository::submodule_head(&dir, self.repo.hash).is_some_and(|h| h != e.id)
                };
                let modified = deleted
                    || gitlink_moved
                    || e.stage().is_conflict()
                    || !matches!(
                        checker.check(e),
                        Ok(Status::Unchanged | Status::StatOnly)
                    );
                if modified {
                    self.show_entry("C", e)?;
                }
            }
        }
        Ok(())
    }
}

/// A work tree walk for `--others`, `--ignored` and `--killed`.
struct Walk<'a> {
    root: PathBuf,
    index: &'a Index,
    excludes: Option<Excludes>,
    ignored: bool,
    directory: bool,
    no_empty_directory: bool,
    others: Vec<Vec<u8>>,
    killed: Vec<Vec<u8>>,
    want_killed: bool,
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

fn name_bytes(name: &std::ffi::OsStr) -> Vec<u8> {
    name.as_encoded_bytes().to_vec()
}

impl Walk<'_> {
    fn tracked(&self, path: &[u8]) -> bool {
        !self.index.path_range(path).is_empty()
    }

    fn has_tracked_below(&self, dir: &[u8]) -> bool {
        !self.index.dir_range(dir).is_empty()
    }

    fn excluded(&mut self, path: &[u8], is_dir: bool) -> bool {
        match &mut self.excludes {
            Some(ex) => ex.is_excluded(path, is_dir),
            None => false,
        }
    }

    /// Whether some leading directory of `path` is a file in the index.
    fn below_tracked_file(&self, path: &[u8]) -> bool {
        path.iter()
            .enumerate()
            .filter(|(_, b)| **b == b'/')
            .any(|(i, _)| self.tracked(&path[..i]))
    }

    fn untracked_file(&mut self, path: Vec<u8>, excluded: bool) {
        if self.want_killed && (self.has_tracked_below(&path) || self.below_tracked_file(&path))
        {
            self.killed.push(path.clone());
        }
        if excluded == self.ignored {
            self.others.push(path);
        }
    }

    /// Whether the directory holds anything at all (for
    /// `--no-empty-directory`).
    fn dir_has_files(&self, rel: &[u8]) -> bool {
        let Ok(rd) = std::fs::read_dir(os_path(&self.root, rel)) else {
            return false;
        };
        for ent in rd.flatten() {
            let name = name_bytes(&ent.file_name());
            if name == b".git" {
                continue;
            }
            let mut p = rel.to_vec();
            p.push(b'/');
            p.extend_from_slice(&name);
            match ent.file_type() {
                Ok(t) if t.is_dir() => {
                    if self.dir_has_files(&p) {
                        return true;
                    }
                }
                Ok(_) => return true,
                Err(_) => {}
            }
        }
        false
    }

    /// Walk the directory `dir` (empty or ending in `/`).
    fn walk(&mut self, dir: &[u8], lister: &Lister<'_>, parent_excluded: bool) {
        let Ok(rd) = std::fs::read_dir(os_path(&self.root, dir)) else {
            return;
        };
        let mut entries: Vec<(Vec<u8>, std::fs::FileType)> = rd
            .flatten()
            .filter_map(|e| Some((name_bytes(&e.file_name()), e.file_type().ok()?)))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, ft) in entries {
            if name == b".git" {
                continue;
            }
            let mut path = dir.to_vec();
            path.extend_from_slice(&name);
            if ft.is_dir() {
                // A submodule is skipped; a tracked file that became a
                // directory is only entered when files are listed one by
                // one.
                if self.tracked(&path)
                    && (self.directory
                        || self
                            .index
                            .entries_for_path(&path)
                            .iter()
                            .any(|e| e.mode.is_gitlink()))
                {
                    continue;
                }
                if !lister.dir_wanted(&path) {
                    continue;
                }
                let excluded = parent_excluded || self.excluded(&path, true);
                if !self.has_tracked_below(&path) {
                    let nested = std::fs::symlink_metadata(os_path(&self.root, &path).join(".git"))
                        .is_ok();
                    if nested || (self.directory && excluded == self.ignored) {
                        if self.no_empty_directory && !nested && !self.dir_has_files(&path) {
                            continue;
                        }
                        if excluded == self.ignored {
                            let mut shown = path.clone();
                            shown.push(b'/');
                            self.others.push(shown);
                        }
                        continue;
                    }
                    if excluded && !self.ignored && !self.want_killed {
                        continue;
                    }
                }
                path.push(b'/');
                self.walk(&path, lister, excluded);
            } else {
                if self.tracked(&path) {
                    continue;
                }
                let excluded = parent_excluded || self.excluded(&path, false);
                self.untracked_file(path, excluded);
            }
        }
    }
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let nul = Cell::new(false);
    let tags = Cell::new(false);
    let lower_assume = Cell::new(false);
    let lower_fsmonitor = Cell::new(false);
    let cached = Cell::new(false);
    let deleted = Cell::new(false);
    let modified = Cell::new(false);
    let others = Cell::new(false);
    let ignored = Cell::new(false);
    let stage = Cell::new(false);
    let killed = Cell::new(false);
    let directory = Cell::new(false);
    let eol = Cell::new(false);
    let empty_directory = Cell::new(true);
    let unmerged = Cell::new(false);
    let resolve_undo = Cell::new(false);
    let excludes: RefCell<Vec<ExcludeArg>> = RefCell::new(Vec::new());
    let full_name = Cell::new(false);
    let recurse_submodules = Cell::new(false);
    let error_unmatch = Cell::new(false);
    let with_tree = RefCell::new(None);
    let abbrev = Cell::new(Abbrev::Full);
    let debug = Cell::new(false);
    let dedup = Cell::new(false);
    let sparse = Cell::new(false);
    let format = RefCell::new(None);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag('z', None, &nul, msg::HELP_Z),
            Opt::flag('t', None, &tags, msg::HELP_T),
            Opt::flag('v', None, &lower_assume, msg::HELP_V),
            Opt::flag('f', None, &lower_fsmonitor, msg::HELP_F),
            Opt::flag('c', "cached", &cached, msg::HELP_CACHED),
            Opt::flag('d', "deleted", &deleted, msg::HELP_DELETED),
            Opt::flag('m', "modified", &modified, msg::HELP_MODIFIED),
            Opt::flag('o', "others", &others, msg::HELP_OTHERS),
            Opt::flag('i', "ignored", &ignored, msg::HELP_IGNORED),
            Opt::flag('s', "stage", &stage, msg::HELP_STAGE),
            Opt::flag('k', "killed", &killed, msg::HELP_KILLED),
            Opt::flag(None, "directory", &directory, msg::HELP_DIRECTORY),
            Opt::flag(None, "eol", &eol, msg::HELP_EOL),
            Opt::flag(
                None,
                "empty-directory",
                &empty_directory,
                msg::HELP_EMPTY_DIRECTORY,
            ),
            Opt::flag('u', "unmerged", &unmerged, msg::HELP_UNMERGED),
            Opt::flag(None, "resolve-undo", &resolve_undo, msg::HELP_RESOLVE_UNDO),
            Opt::callback('x', "exclude", Some("pattern"), msg::HELP_EXCLUDE, |v, _| {
                let v = v.unwrap_or_default().to_string();
                excludes.borrow_mut().push(ExcludeArg::Pattern(v));
                Ok(())
            })
            .noneg(),
            Opt::callback(
                'X',
                "exclude-from",
                Some("file"),
                msg::HELP_EXCLUDE_FROM,
                |v, _| {
                    let v = v.unwrap_or_default().to_string();
                    excludes.borrow_mut().push(ExcludeArg::File(v));
                    Ok(())
                },
            )
            .noneg(),
            Opt::callback(
                None,
                "exclude-per-directory",
                Some("file"),
                msg::HELP_EXCLUDE_PER_DIR,
                |v, neg| {
                    let v = if neg { None } else { v.map(str::to_string) };
                    excludes.borrow_mut().push(ExcludeArg::PerDirectory(v));
                    Ok(())
                },
            ),
            Opt::callback(
                None,
                "exclude-standard",
                None,
                msg::HELP_EXCLUDE_STANDARD,
                |_, _| {
                    excludes.borrow_mut().push(ExcludeArg::Standard);
                    Ok(())
                },
            )
            .noarg()
            .noneg(),
            Opt::flag(None, "full-name", &full_name, msg::HELP_FULL_NAME).noneg(),
            Opt::flag(
                None,
                "recurse-submodules",
                &recurse_submodules,
                msg::HELP_RECURSE_SUBMODULES,
            ),
            Opt::flag(
                None,
                "error-unmatch",
                &error_unmatch,
                msg::HELP_ERROR_UNMATCH,
            ),
            Opt::string(None, "with-tree", &with_tree, "tree-ish", msg::HELP_WITH_TREE),
            Abbrev::option(&abbrev, msg::HELP_ABBREV),
            Opt::flag(None, "debug", &debug, msg::HELP_DEBUG),
            Opt::flag(None, "deduplicate", &dedup, msg::HELP_DEDUP),
            Opt::flag(None, "sparse", &sparse, msg::HELP_SPARSE),
            Opt::string(None, "format", &format, "format", msg::HELP_FORMAT).noneg(),
        ],
    );
    let args = parser.parse(argv)?.args;
    let show_tags = tags.get() || lower_assume.get() || lower_fsmonitor.get();
    if format.borrow().is_some()
        && (stage.get()
            || others.get()
            || killed.get()
            || tags.get()
            || resolve_undo.get()
            || dedup.get()
            || eol.get())
    {
        return Err(parser.usage_msg(msg::FORMAT_EXCLUSIVE).into());
    }
    drop(parser);

    let excludes = excludes.into_inner();
    if ignored.get() && !others.get() && !cached.get() {
        return Err(Die::Fatal(msg::IGNORED_NEEDS_MODE.to_string()));
    }
    if ignored.get() && excludes.is_empty() {
        return Err(Die::Fatal(msg::IGNORED_NEEDS_EXCLUDES.to_string()));
    }
    if with_tree.borrow().is_some() && (stage.get() || unmerged.get()) {
        return Err(Die::Fatal(msg::WITH_TREE_AND_STAGE.to_string()));
    }
    if eol.get() {
        return Err(Die::Fatal(msg::not_supported("--eol")));
    }

    let mut repo = Repo::open(ctx, Mode::RequiredQuiet)?;
    let need_work_tree = deleted.get() || modified.get() || others.get() || killed.get();
    let work_tree = if need_work_tree {
        Some(repo.work_tree()?)
    } else {
        None
    };
    let prefix = repo.prefix();

    let (index, index_mtime) = repo.read_index()?;
    if resolve_undo.get() && index.resolve_undo_raw().is_some() {
        return Err(Die::Fatal(msg::not_supported("--resolve-undo")));
    }
    if recurse_submodules.get()
        && (others.get()
            || killed.get()
            || deleted.get()
            || modified.get()
            || ignored.get()
            || with_tree.borrow().is_some())
    {
        return Err(Die::Fatal(msg::RECURSE_UNSUPPORTED.to_string()));
    }
    if recurse_submodules.get() && index.entries().iter().any(|e| e.mode.is_gitlink()) {
        return Err(Die::Fatal(msg::not_supported("--recurse-submodules")));
    }
    let mut entries: Vec<Entry> = index.entries().to_vec();
    if let Some(spec) = with_tree.borrow().as_deref() {
        overlay_tree(&mut repo, &index, &mut entries, spec)?;
    }
    let repo = repo;

    let root = repo
        .setup
        .work_tree
        .as_ref()
        .map(|w| w.as_os_str().as_encoded_bytes().to_vec());
    let opts = ParseOptions {
        prefix: prefix.clone(),
        root,
        globals: repository::pathspec_globals(ctx),
        ..Default::default()
    };
    let pathspec = Pathspec::parse(&args, &opts).map_err(|e| Die::Fatal(e.to_string()))?;
    let implicit = args.is_empty() && !prefix.is_empty();
    let mut repo = repo;
    let attr_tree = PathspecAttrs::prepare(ctx, &mut repo, &pathspec)?;
    let repo = repo;

    let mut show = Show {
        cached: cached.get(),
        deleted: deleted.get(),
        modified: modified.get(),
        others: others.get(),
        ignored: ignored.get(),
        stage: stage.get() || unmerged.get(),
        unmerged: unmerged.get(),
        killed: killed.get(),
        directory: directory.get(),
        no_empty_directory: !empty_directory.get(),
        tags: show_tags,
        lower_assume: lower_assume.get(),
        nul: nul.get(),
        full_name: full_name.get(),
        error_unmatch: error_unmatch.get(),
        debug: debug.get(),
        dedup: dedup.get(),
    };
    if !(show.cached
        || show.deleted
        || show.modified
        || show.others
        || show.stage
        || show.killed
        || resolve_undo.get())
    {
        show.cached = true;
    }

    let mut ex = if excludes.is_empty() {
        None
    } else {
        Some(build_excludes(&repo, &excludes)?)
    };

    let fmt = format.borrow().clone();
    let n_items = pathspec.len();
    let mut lister = Lister {
        repo: &repo,
        show,
        prefix,
        pathspec,
        implicit,
        seen: vec![false; n_items],
        attrs: attr_tree.map(|t| RefCell::new(PathspecAttrs::new(&repo, &index, t))),
        quote_high: repo.quote_path(),
        abbrev: abbrev.get().length(&repo),
        format: fmt.map(|f| format::parse(&f, "ls-files", field_named)),
        out: Vec::new(),
        last_path: None,
    };

    if lister.show.others || lister.show.killed {
        let root = work_tree.clone().expect("work tree");
        let mut walk = Walk {
            root,
            index: &index,
            excludes: if lister.show.others { ex.take() } else { None },
            ignored: lister.show.ignored && lister.show.others,
            directory: lister.show.directory,
            no_empty_directory: lister.show.no_empty_directory,
            others: Vec::new(),
            killed: Vec::new(),
            want_killed: lister.show.killed,
        };
        let start = walk_start(&lister);
        walk.walk(&start, &lister, false);
        if let Some(e) = walk.excludes.take() {
            ex = Some(e);
        }
        let others_found = std::mem::take(&mut walk.others);
        let killed_found = std::mem::take(&mut walk.killed);
        if lister.show.others {
            for p in others_found {
                let bare = p.strip_suffix(b"/").unwrap_or(&p).to_vec();
                if lister.wanted(&bare) {
                    lister.show_file("?", &p);
                }
            }
        }
        if lister.show.killed {
            for p in killed_found {
                if lister.wanted(&p) {
                    lister.show_file("K", &p);
                }
            }
        }
    }

    let checker = if lister.show.deleted || lister.show.modified {
        Some(repo.checker(index_mtime)?)
    } else {
        None
    };
    let result = lister.show_index(&entries, checker.as_ref(), &mut ex);
    lister.flush();
    result?;

    if let Some(ex) = ex.as_mut() {
        for w in ex.take_warnings() {
            eprintln!("warning: {w}");
        }
    }

    if lister.show.error_unmatch && !lister.implicit {
        let mut bad = false;
        for item in lister.pathspec.unmatched(&lister.seen) {
            bad = true;
            eprintln!(
                "error: {}",
                msg::pathspec_unmatched(&String::from_utf8_lossy(item.original()))
            );
        }
        if bad {
            eprintln!("{}", msg::FORGOT_ADD);
            return Ok(1);
        }
    }
    Ok(0)
}

/// Where the work tree walk starts: the longest leading directory every
/// pathspec item (or the prefix) shares.
fn walk_start(lister: &Lister<'_>) -> Vec<u8> {
    if lister.implicit {
        return lister.prefix.clone();
    }
    lister.pathspec.common_prefix().to_vec()
}

pub(crate) fn build_excludes(repo: &Repo, args: &[ExcludeArg]) -> Result<Excludes, Die> {
    let root = repo
        .setup
        .work_tree
        .clone()
        .unwrap_or_else(|| repo.setup.cwd.clone());
    let mut ex = Excludes::new(root);
    ex.set_ignore_case(repo.config.bool("core.ignorecase").ok().flatten().unwrap_or(false));
    for a in args {
        match a {
            ExcludeArg::Pattern(p) => ex.add_pattern(p.as_bytes()),
            ExcludeArg::File(f) => match ex.add_file(Path::new(f), f.as_bytes().to_vec()) {
                Ok(true) => {}
                _ => return Err(Die::Fatal(msg::bad_exclude_file(f))),
            },
            ExcludeArg::PerDirectory(Some(name)) => ex.set_per_directory(name.as_bytes().to_vec()),
            ExcludeArg::PerDirectory(None) => {}
            ExcludeArg::Standard => {
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
            }
        }
    }
    Ok(ex)
}

/// `--with-tree`: add the files of the tree that are not in the index as
/// stage 1 entries.
fn overlay_tree(
    repo: &mut Repo,
    index: &Index,
    entries: &mut Vec<Entry>,
    spec: &str,
) -> Result<(), Die> {
    let not_found = || Die::Fatal(msg::tree_ish_not_found(spec));
    let id = repo.resolve(spec).ok_or_else(not_found)?;
    let tree = repo.peel_to(id, ObjectKind::Tree).ok_or_else(not_found)?;
    let mut files = Vec::new();
    collect_tree(repo, &tree, b"", &mut files)?;
    for (path, mode, oid) in files {
        if index.get(&path, Stage::MERGED).is_some() {
            continue;
        }
        entries.push(Entry::new(path, Stage::BASE, mode, oid));
    }
    // Stable: index entries of a path come before the tree's.
    entries.sort_by(|a, b| a.path().cmp(b.path()));
    Ok(())
}

fn collect_tree(
    repo: &Repo,
    tree: &ObjectId,
    base: &[u8],
    out: &mut Vec<(Vec<u8>, IndexMode, ObjectId)>,
) -> Result<(), Die> {
    let Some(Object::Tree(t)) = repo.read_object(tree) else {
        return Err(Die::Fatal(crate::messages::ls_tree::could_not_read(
            &tree.to_hex(),
        )));
    };
    for e in t.entries {
        let mut path = base.to_vec();
        path.extend_from_slice(&e.name);
        let mode = e.mode.canonical();
        if mode.is_tree() {
            path.push(b'/');
            collect_tree(repo, &e.oid, &path, out)?;
        } else {
            out.push((path, IndexMode(mode.bits()), e.oid));
        }
    }
    Ok(())
}

fn checker_root(repo: &Repo) -> PathBuf {
    repo.setup.work_tree.clone().unwrap_or_default()
}

fn lossy_os(p: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(p).into_owned())
}
