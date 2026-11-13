// SPDX-License-Identifier: MIT
//! `git status` (Documentation/git-status.adoc), machine-readable formats
//! only: `--porcelain[=v1]` and `--porcelain=v2`, with `-z`, `-b` and
//! `--untracked-files=no|normal|all`. Rename detection, submodule details,
//! `--ignored` and the long/short human formats are not implemented yet.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use rgit_hash::ObjectId;
use rgit_index::Entry;
use rgit_object::{Object, ObjectKind};
use rgit_pathspec::{ParseOptions, Pathspec};
use rgit_worktree::ignore::Excludes;
use rgit_worktree::stat::{Checker, Status};

use super::ls_files::{ExcludeArg, build_excludes};
use crate::messages::status as msg;
use crate::prelude::*;
use crate::quote::write_quoted;
use crate::repo::Mode;
use crate::repository::{self, Repo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Untracked {
    No,
    Normal,
    All,
}

/// One tracked path with its HEAD, index and work tree state.
#[derive(Default, Clone)]
pub(crate) struct Change {
    pub(crate) head: Option<(u32, ObjectId)>,
    /// Stage 0 entry (mode, id, intent-to-add).
    pub(crate) index: Option<(u32, ObjectId, bool)>,
    /// Conflict stages 1..=3.
    pub(crate) stages: [Option<(u32, ObjectId)>; 3],
    /// Work tree mode; `None` when deleted.
    pub(crate) wt_mode: Option<u32>,
    pub(crate) wt: u8,
}

pub(crate) fn collect_tree(
    repo: &Repo,
    tree: &ObjectId,
    base: &[u8],
    out: &mut BTreeMap<Vec<u8>, Change>,
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
            out.entry(path).or_default().head = Some((mode.bits(), e.oid));
        }
    }
    Ok(())
}

fn type_bits(mode: u32) -> u32 {
    mode & 0o170000
}

fn index_letter(c: &Change) -> u8 {
    match (c.head, c.index) {
        (None, Some((_, _, false))) => b'A',
        (Some(_), None) => b'D',
        (Some(_), Some((_, _, true))) => b'D',
        (Some((hm, hid)), Some((im, iid, false))) => {
            if type_bits(hm) != type_bits(im) {
                b'T'
            } else if hm != im || hid != iid {
                b'M'
            } else {
                b' '
            }
        }
        _ => b' ',
    }
}

fn conflict_code(c: &Change) -> Option<&'static str> {
    let s = [
        c.stages[0].is_some(),
        c.stages[1].is_some(),
        c.stages[2].is_some(),
    ];
    Some(match s {
        [false, false, false] => return None,
        [true, false, false] => "DD",
        [false, true, false] => "AU",
        [false, false, true] => "UA",
        [true, true, false] => "UD",
        [true, false, true] => "DU",
        [false, true, true] => "AA",
        [true, true, true] => "UU",
    })
}

pub(crate) struct Walker<'a> {
    pub(crate) root: &'a Path,
    pub(crate) tracked: &'a BTreeMap<Vec<u8>, Change>,
    pub(crate) excludes: Excludes,
    pub(crate) mode: Untracked,
    pub(crate) out: Vec<Vec<u8>>,
}

fn os_path(root: &Path, rel: &[u8]) -> std::path::PathBuf {
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

impl Walker<'_> {
    fn tracked_below(&self, dir: &[u8]) -> bool {
        let mut d = dir.to_vec();
        d.push(b'/');
        self.tracked
            .range(d.clone()..)
            .next()
            .is_some_and(|(p, c)| p.starts_with(&d) && (c.index.is_some() || c.stages.iter().any(Option::is_some)))
    }

    fn is_tracked(&self, path: &[u8]) -> bool {
        self.tracked
            .get(path)
            .is_some_and(|c| c.index.is_some() || c.stages.iter().any(Option::is_some))
    }

    /// Walk `dir` (empty or ending in `/`); collect untracked paths into
    /// `found`. Returns whether anything untracked was found.
    pub(crate) fn walk(&mut self, dir: &[u8], found: &mut Vec<Vec<u8>>) {
        let Ok(rd) = std::fs::read_dir(os_path(self.root, dir)) else {
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
                if self.is_tracked(&path) {
                    continue;
                }
                if self.excludes.is_excluded(&path, true) {
                    continue;
                }
                let nested = std::fs::symlink_metadata(os_path(self.root, &path).join(".git")).is_ok();
                if nested {
                    path.push(b'/');
                    found.push(path);
                    continue;
                }
                let has_tracked = self.tracked_below(&path);
                path.push(b'/');
                if !has_tracked && self.mode == Untracked::Normal {
                    let mut inner = Vec::new();
                    self.walk(&path, &mut inner);
                    if !inner.is_empty() {
                        found.push(path);
                    }
                } else {
                    self.walk(&path, found);
                }
            } else {
                if self.is_tracked(&path) || self.excludes.is_excluded(&path, false) {
                    continue;
                }
                found.push(path);
            }
        }
    }
}

struct Printer {
    nul: bool,
    quote_high: bool,
    /// Porcelain v1 also quotes paths containing a space.
    space_quote: bool,
    out: Vec<u8>,
}

impl Printer {
    fn path(&mut self, p: &[u8]) {
        if self.nul {
            self.out.extend_from_slice(p);
        } else {
            let start = self.out.len();
            write_quoted(&mut self.out, p, self.quote_high);
            if self.out.get(start) != Some(&b'"') && self.space_quote && p.contains(&b' ') {
                self.out.insert(start, b'"');
                self.out.push(b'"');
            }
        }
    }
    fn end(&mut self) {
        self.out.push(if self.nul { 0 } else { b'\n' });
    }
    fn text(&mut self, s: &str) {
        self.out.extend_from_slice(s.as_bytes());
    }
}

/// The branch header data.
struct BranchInfo {
    head: Option<ObjectId>,
    branch: Option<String>,
    /// Upstream short name and whether its ref exists and equals HEAD
    /// (`Some(true)`), differs (`Some(false)`) or is gone (`None`).
    upstream: Option<(String, Option<bool>)>,
}

fn branch_info(repo: &mut Repo) -> Result<BranchInfo, Die> {
    let head = repo.resolve("HEAD");
    let target = repo.refs.head_target();
    let branch = target
        .as_deref()
        .map(|t| t.strip_prefix("refs/heads/").unwrap_or(t).to_string());
    let mut upstream = None;
    if let Some(b) = &branch {
        let remote = repo
            .config
            .string(format!("branch.{b}.remote"))
            .ok()
            .flatten()
            .map(|s| s.to_string());
        let merge = repo
            .config
            .string(format!("branch.{b}.merge"))
            .ok()
            .flatten()
            .map(|s| s.to_string());
        if let (Some(remote), Some(merge)) = (remote, merge) {
            let short_branch = merge.strip_prefix("refs/heads/").unwrap_or(&merge).to_string();
            let (full, short) = if remote == "." {
                (merge.clone(), short_branch)
            } else {
                (
                    format!("refs/remotes/{remote}/{short_branch}"),
                    format!("{remote}/{short_branch}"),
                )
            };
            let up = repo.refs.resolve(&full);
            let state = up.map(|u| Some(u) == head);
            if state == Some(false) {
                return Err(Die::Fatal(msg::not_supported("ahead/behind counts")));
            }
            upstream = Some((short, state));
        }
    }
    Ok(BranchInfo {
        head,
        branch,
        upstream,
    })
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let branch = Cell::new(false);
    let nul = Cell::new(false);
    let porcelain: RefCell<Option<String>> = RefCell::new(None);
    let untracked: RefCell<Option<String>> = RefCell::new(None);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag('b', "branch", &branch, msg::HELP_BRANCH),
            Opt::string(None, "porcelain", &porcelain, "version", msg::HELP_PORCELAIN)
                .optional(Some("v1")),
            Opt::flag('z', None, &nul, msg::HELP_Z),
            Opt::string('u', "untracked-files", &untracked, "mode", msg::HELP_UNTRACKED)
                .optional(Some("all")),
        ],
    );
    let args = parser.parse(argv)?.args;
    drop(parser);
    let version = match porcelain.borrow().as_deref() {
        Some("v1") => 1,
        Some("v2") => 2,
        Some(v) => return Err(Die::Fatal(msg::unsupported_porcelain(v))),
        None if nul.get() => 1,
        None => return Err(Die::Fatal(msg::not_supported("the long format"))),
    };
    let mode = match untracked.borrow().as_deref() {
        None | Some("normal") => Untracked::Normal,
        Some("all") => Untracked::All,
        Some("no") => Untracked::No,
        Some(m) => return Err(Die::Fatal(msg::invalid_untracked(m))),
    };

    let mut repo = Repo::open(ctx, Mode::RequiredQuiet)?;
    let work_tree = repo.work_tree()?;
    let info = branch_info(&mut repo)?;
    let mut changes: BTreeMap<Vec<u8>, Change> = BTreeMap::new();
    if let Some(h) = info.head {
        let tree = repo
            .peel_to(h, ObjectKind::Tree)
            .ok_or_else(|| Die::Fatal(crate::messages::ls_tree::NOT_A_TREE.to_string()))?;
        collect_tree(&repo, &tree, b"", &mut changes)?;
    }
    let repo = repo;
    let (index, index_mtime) = repo.read_index()?;
    for e in index.entries() {
        let c = changes.entry(e.path().to_vec()).or_default();
        match e.stage().get() {
            0 => c.index = Some((e.mode.0, e.id, e.flags.intent_to_add)),
            s => c.stages[usize::from(s) - 1] = Some((e.mode.0, e.id)),
        }
    }
    let checker = repo.checker(index_mtime)?;
    for e in index.entries() {
        let c = changes.get_mut(e.path()).expect("entry");
        if e.stage().is_conflict() {
            if let Ok(Some(meta)) = checker.lstat(e.path()) {
                c.wt_mode = Some(wt_mode(&meta, e.mode.0));
            }
            continue;
        }
        check_worktree(&repo, &checker, &work_tree, e, c);
    }

    let root = work_tree.as_os_str().as_encoded_bytes().to_vec();
    let opts = ParseOptions {
        prefix: repo.prefix(),
        root: Some(root),
        globals: repository::pathspec_globals(ctx),
        ..Default::default()
    };
    let pathspec = Pathspec::parse(&args, &opts).map_err(|e| Die::Fatal(e.to_string()))?;
    let wanted = |p: &[u8]| pathspec.is_empty() || pathspec.match_path(p).is_some();

    let mut untracked_paths = Vec::new();
    if mode != Untracked::No {
        let excludes = build_excludes(&repo, &[ExcludeArg::Standard])?;
        let mut w = Walker {
            root: &work_tree,
            tracked: &changes,
            excludes,
            mode,
            out: Vec::new(),
        };
        let mut found = Vec::new();
        w.walk(b"", &mut found);
        w.out = found;
        untracked_paths = std::mem::take(&mut w.out);
    }

    let mut p = Printer {
        nul: nul.get(),
        quote_high: repo.quote_path(),
        space_quote: version == 1,
        out: Vec::new(),
    };
    if branch.get() {
        header(&mut p, version, &info);
    }
    let zero = ObjectId::null(repo.hash);
    for (path, c) in &changes {
        if !wanted(path) {
            continue;
        }
        if let Some(code) = conflict_code(c) {
            if version == 1 {
                p.text(code);
                p.text(" ");
            } else {
                let m = |s: &Option<(u32, ObjectId)>| s.map_or(0, |x| x.0);
                let h = |s: &Option<(u32, ObjectId)>| s.map_or(zero, |x| x.1);
                p.text(&format!(
                    "u {code} N... {:06o} {:06o} {:06o} {:06o} {} {} {} ",
                    m(&c.stages[0]),
                    m(&c.stages[1]),
                    m(&c.stages[2]),
                    c.wt_mode.unwrap_or(0),
                    h(&c.stages[0]).to_hex(),
                    h(&c.stages[1]).to_hex(),
                    h(&c.stages[2]).to_hex(),
                ));
            }
            p.path(path);
            p.end();
            continue;
        }
        let x = index_letter(c);
        let y = if c.wt == 0 { b' ' } else { c.wt };
        if x == b' ' && y == b' ' {
            continue;
        }
        if version == 1 {
            p.out.push(x);
            p.out.push(y);
            p.out.push(b' ');
        } else {
            let xy = [
                if x == b' ' { b'.' } else { x },
                if y == b' ' { b'.' } else { y },
            ];
            let (hm, hid) = c.head.unwrap_or((0, zero));
            let (im, iid) = match c.index {
                Some((m, id, false)) => (m, id),
                _ => (0, zero),
            };
            p.text(&format!(
                "1 {} N... {hm:06o} {im:06o} {:06o} {} {} ",
                String::from_utf8_lossy(&xy),
                c.wt_mode.unwrap_or(0),
                hid.to_hex(),
                iid.to_hex(),
            ));
        }
        p.path(path);
        p.end();
    }
    for u in &untracked_paths {
        let bare = u.strip_suffix(b"/").unwrap_or(u);
        if !wanted(bare) && !wanted(u) {
            continue;
        }
        p.text(if version == 1 { "?? " } else { "? " });
        p.path(u);
        p.end();
    }
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(&p.out);
    let _ = o.flush();
    Ok(0)
}

pub(crate) fn check_worktree(repo: &Repo, checker: &Checker, work_tree: &Path, e: &Entry, c: &mut Change) {
    let meta = checker.lstat(e.path()).ok().flatten();
    let Some(meta) = meta else {
        c.wt = b'D';
        c.wt_mode = None;
        return;
    };
    c.wt_mode = Some(wt_mode(&meta, e.mode.0));
    if e.flags.intent_to_add {
        c.wt = b'A';
        return;
    }
    if e.mode.is_gitlink() {
        let dir = work_tree.join(String::from_utf8_lossy(e.path()).as_ref());
        if repository::submodule_head(&dir, repo.hash).is_some_and(|h| h != e.id) {
            c.wt = b'M';
        }
        return;
    }
    c.wt = match checker.check(e) {
        Ok(Status::Unchanged | Status::StatOnly) => b' ',
        Ok(Status::TypeChanged) => b'T',
        Ok(Status::Deleted) => {
            c.wt_mode = None;
            b'D'
        }
        _ => b'M',
    };
}

fn wt_mode(meta: &std::fs::Metadata, index_mode: u32) -> u32 {
    let ft = meta.file_type();
    if ft.is_symlink() {
        0o120000
    } else if ft.is_dir() {
        if type_bits(index_mode) == 0o160000 {
            0o160000
        } else {
            0o040000
        }
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o100 != 0 {
                return 0o100755;
            }
        }
        0o100644
    }
}

fn header(p: &mut Printer, version: u8, info: &BranchInfo) {
    if version == 1 {
        p.text("## ");
        match (&info.branch, info.head) {
            (Some(b), None) => p.text(&format!("No commits yet on {b}")),
            (Some(b), Some(_)) => p.text(b),
            (None, _) => p.text("HEAD (no branch)"),
        }
        if let Some((up, state)) = &info.upstream {
            if info.head.is_some() || state.is_some() {
                p.text(&format!("...{up}"));
                if state.is_none() {
                    p.text(" [gone]");
                }
            }
        }
        p.end();
        return;
    }
    let oid = info.head.map_or("(initial)".to_string(), |h| h.to_hex());
    p.text(&format!("# branch.oid {oid}"));
    p.end();
    p.text(&format!(
        "# branch.head {}",
        info.branch.as_deref().unwrap_or("(detached)")
    ));
    p.end();
    if let Some((up, state)) = &info.upstream {
        p.text(&format!("# branch.upstream {up}"));
        p.end();
        if state.is_some() {
            p.text("# branch.ab +0 -0");
            p.end();
        }
    }
}
