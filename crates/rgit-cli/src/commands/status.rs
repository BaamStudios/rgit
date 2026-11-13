// SPDX-License-Identifier: MIT
//! `git status` (Documentation/git-status.adoc): the long format (branch
//! and upstream lines, sections with `advice.statusHints`, closing
//! summary), `-s`/`--short`, `--porcelain[=v1]` and `--porcelain=v2`, with
//! `-z`, `-b`, `--[no-]ahead-behind` and `--untracked-files=no|normal|all`.
//! Rename detection, submodule details, `--ignored`, colour and the state
//! banners other than merging are not implemented yet.

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
use crate::quote::{relative_to, write_quoted};
use crate::refs_support::Revs;
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
    /// The short format and porcelain v1 also quote paths containing a
    /// space.
    space_quote: bool,
    /// Show paths relative to this directory (empty or ending in `/`).
    prefix: Vec<u8>,
    out: Vec<u8>,
}

impl Printer {
    fn path(&mut self, p: &[u8]) {
        let rel;
        let p = if self.prefix.is_empty() {
            p
        } else {
            rel = relative_to(p, &self.prefix);
            &rel[..]
        };
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

/// How the branch relates to its upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tracking {
    /// The upstream ref does not exist.
    Gone,
    /// Ahead and behind counts.
    Counts(usize, usize),
    /// Different commits, not counted (`--no-ahead-behind`).
    Different,
}

/// The branch header data.
struct BranchInfo {
    head: Option<ObjectId>,
    branch: Option<String>,
    /// Upstream short name and how the branch relates to it.
    upstream: Option<(String, Tracking)>,
}

fn branch_info(repo: &mut Repo, revs: &Revs, count: bool) -> Result<BranchInfo, Die> {
    let head = repo.resolve("HEAD");
    let target = repo.refs.head_target();
    let branch = target
        .as_deref()
        .map(|t| t.strip_prefix("refs/heads/").unwrap_or(t).to_string());
    let mut upstream = None;
    if let Some(b) = &branch {
        let names = revs.with_resolver(|r| {
            let full = r.upstream_ref(b)?;
            let short = r.shorten_ref(&full, false).unwrap_or_else(|_| full.clone());
            let _ = r.take_diagnostics();
            Some((full, short))
        });
        if let Some((full, short)) = names {
            let state = match (repo.refs.resolve(&full), head) {
                (None, _) => Tracking::Gone,
                (Some(u), Some(h)) if u == h => Tracking::Counts(0, 0),
                (Some(_), Some(_)) if !count => Tracking::Different,
                (Some(u), Some(h)) => {
                    let counts = revs.with_resolver(|r| -> Result<(usize, usize), Die> {
                        let fatal = |e: rgit_revwalk::revision::Error| Die::Fatal(e.to_string());
                        let u = r.peel_to_commit(&u).map_err(fatal)?;
                        let h = r.peel_to_commit(&h).map_err(fatal)?;
                        r.ahead_behind(&h, &u).map_err(fatal)
                    })?;
                    Tracking::Counts(counts.0, counts.1)
                }
                // git reports the upstream of an unborn branch as gone.
                (Some(_), None) => Tracking::Gone,
            };
            upstream = Some((short, state));
        }
    }
    Ok(BranchInfo {
        head,
        branch,
        upstream,
    })
}

/// "HEAD detached at/from <name>": the target of the last checkout in the
/// HEAD reflog. `None` when there is no such entry.
fn detached_from(repo: &Repo, revs: &Revs, head: ObjectId) -> Option<(String, bool)> {
    revs.with_resolver(|r| {
        let log = r.repo().refs.reflog("HEAD").ok()??;
        let entry = log.iter().rev().find_map(|e| {
            let rest = e.message.strip_prefix("checkout: moving from ")?;
            let pos = rest.rfind(" to ")?;
            Some((e, rest[pos + 4..].to_string()))
        })?;
        let (e, target) = entry;
        let by_name = r.dwim_ref(&target).ok().filter(|d| {
            d.count > 0
                && d.full_name.starts_with("refs/")
                && d.oid
                    .and_then(|o| r.peel_to_commit(&o).ok())
                    .is_some_and(|c| c == e.new)
        });
        let _ = r.take_diagnostics();
        let name = match by_name {
            Some(d) => d
                .full_name
                .strip_prefix("refs/tags/")
                .or_else(|| d.full_name.strip_prefix("refs/remotes/"))
                .unwrap_or(&d.full_name)
                .to_string(),
            None => repo.abbrev(&e.new, repo.default_abbrev()),
        };
        Some((name, head == e.new))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Long,
    Short,
    Porcelain(u8),
}

/// No format option given (`--no-short` etc. set 0).
const F_UNSET: i32 = -1;
const F_LONG: i32 = 1;
const F_SHORT: i32 = 2;
const F_PORCELAIN: i32 = 3;

fn config_bool(repo: &Repo, key: &str) -> Result<Option<bool>, Die> {
    repo.config.bool(key).map_err(|e| Die::Fatal(e.to_string()))
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let branch = Cell::new(-1);
    let nul = Cell::new(false);
    let format = Cell::new(F_UNSET);
    let porcelain: RefCell<Option<String>> = RefCell::new(None);
    let untracked: RefCell<Option<String>> = RefCell::new(None);
    let ahead_behind = Cell::new(-1);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::set_int('s', "short", &format, msg::HELP_SHORT, F_SHORT),
            Opt::bool('b', "branch", &branch, msg::HELP_BRANCH),
            Opt::bool(None, "ahead-behind", &ahead_behind, msg::HELP_AHEAD_BEHIND),
            Opt::callback(
                None,
                "porcelain",
                Some("version"),
                msg::HELP_PORCELAIN,
                |v, unset| {
                    if unset {
                        format.set(0);
                    } else {
                        format.set(F_PORCELAIN);
                        porcelain.replace(Some(v.unwrap_or("v1").to_string()));
                    }
                    Ok(())
                },
            )
            .optional(None),
            Opt::set_int(None, "long", &format, msg::HELP_LONG, F_LONG),
            Opt::flag('z', "null", &nul, msg::HELP_Z),
            Opt::string('u', "untracked-files", &untracked, "mode", msg::HELP_UNTRACKED)
                .optional(Some("all")),
        ],
    );
    let args = parser.parse(argv)?.args;
    drop(parser);

    let porcelain_version = match porcelain.borrow().as_deref() {
        None | Some("v1") => 1,
        Some("v2") => 2,
        Some(v) => return Err(Die::Fatal(msg::unsupported_porcelain(v))),
    };

    let mut repo = Repo::open(ctx, Mode::RequiredQuiet)?;
    let format = match format.get() {
        F_PORCELAIN => Format::Porcelain(porcelain_version),
        F_SHORT => Format::Short,
        F_LONG if nul.get() => return Err(Die::Fatal(msg::LONG_AND_Z.to_owned())),
        F_LONG => Format::Long,
        F_UNSET if config_bool(&repo, "status.short")? == Some(true) => Format::Short,
        _ if nul.get() => Format::Porcelain(1),
        _ => Format::Long,
    };
    let show_branch = match branch.get() {
        -1 => config_bool(&repo, "status.branch")?.unwrap_or(false),
        b => b != 0,
    };
    let count = match ahead_behind.get() {
        -1 if matches!(format, Format::Porcelain(_)) => true,
        -1 => config_bool(&repo, "status.aheadbehind")?.unwrap_or(true),
        b => b != 0,
    };
    let parse_mode = |m: &str| match m {
        "normal" => Some(Untracked::Normal),
        "all" => Some(Untracked::All),
        "no" => Some(Untracked::No),
        _ => None,
    };
    let mode = match untracked.borrow().as_deref() {
        Some(m) => parse_mode(m).ok_or_else(|| Die::Fatal(msg::invalid_untracked(m)))?,
        None => match repo.config.get("status.showuntrackedfiles") {
            None => Untracked::Normal,
            Some(e) => {
                let v = e.value.as_ref().map(|v| v.to_string());
                let parsed = match v.as_deref() {
                    Some(m) => parse_mode(m).or_else(|| {
                        rgit_config::value::parse_bool(Some(m.as_bytes()))
                            .map(|b| if b { Untracked::Normal } else { Untracked::No })
                    }),
                    None => Some(Untracked::Normal),
                };
                match parsed {
                    Some(m) => m,
                    None => {
                        crate::messages::print_error(&msg::invalid_untracked(
                            v.as_deref().unwrap_or_default(),
                        ));
                        return Err(Die::Fatal(crate::revisions::unable_to_parse(
                            "status.showuntrackedfiles",
                            e,
                        )));
                    }
                }
            }
        },
    };

    let work_tree = repo.work_tree()?;
    let revs = Revs::new(ctx, &repo.setup)?;
    let info = branch_info(&mut repo, &revs, count)?;
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
    untracked_paths.retain(|u| {
        let bare = u.strip_suffix(b"/").unwrap_or(u);
        wanted(bare) || wanted(u)
    });

    let relative = !matches!(format, Format::Porcelain(_))
        && !nul.get()
        && config_bool(&repo, "status.relativepaths")?.unwrap_or(true);
    let mut p = Printer {
        nul: nul.get(),
        quote_high: repo.quote_path(),
        space_quote: format != Format::Long && format != Format::Porcelain(2),
        prefix: if relative { repo.prefix() } else { Vec::new() },
        out: Vec::new(),
    };
    let zero = ObjectId::null(repo.hash);
    let entries: Vec<(&Vec<u8>, &Change)> = changes.iter().filter(|(p, _)| wanted(p)).collect();
    match format {
        Format::Long => {
            let detached = match (&info.branch, info.head) {
                (None, Some(h)) => Some(detached_from(&repo, &revs, h)),
                _ => None,
            };
            let ctx = LongContext {
                info: &info,
                detached,
                hints: config_bool(&repo, "advice.statushints")?.unwrap_or(true),
                merging: repo.setup.git_dir.join("MERGE_HEAD").exists(),
                mode,
            };
            long(&mut p, &ctx, &entries, &untracked_paths);
        }
        Format::Short | Format::Porcelain(1) => {
            if show_branch {
                header(&mut p, 1, &info);
            }
            short(&mut p, &entries, &untracked_paths);
        }
        Format::Porcelain(_) => {
            if show_branch {
                header(&mut p, 2, &info);
            }
            porcelain_v2(&mut p, &entries, &untracked_paths, zero);
        }
    }
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(&p.out);
    let _ = o.flush();
    Ok(0)
}

/// Short format and porcelain v1: `XY <path>`.
fn short(p: &mut Printer, entries: &[(&Vec<u8>, &Change)], untracked: &[Vec<u8>]) {
    for (path, c) in entries {
        if let Some(code) = conflict_code(c) {
            p.text(code);
            p.text(" ");
            p.path(path);
            p.end();
            continue;
        }
        let x = index_letter(c);
        let y = if c.wt == 0 { b' ' } else { c.wt };
        if x == b' ' && y == b' ' {
            continue;
        }
        p.out.push(x);
        p.out.push(y);
        p.out.push(b' ');
        p.path(path);
        p.end();
    }
    for u in untracked {
        p.text("?? ");
        p.path(u);
        p.end();
    }
}

fn porcelain_v2(
    p: &mut Printer,
    entries: &[(&Vec<u8>, &Change)],
    untracked: &[Vec<u8>],
    zero: ObjectId,
) {
    // Ordinary entries first, then unmerged ones (like git).
    let (unmerged, ordinary): (Vec<_>, Vec<_>) = entries
        .iter()
        .partition(|(_, c)| conflict_code(c).is_some());
    for (path, c) in ordinary.into_iter().chain(unmerged) {
        if let Some(code) = conflict_code(c) {
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
            p.path(path);
            p.end();
            continue;
        }
        let x = index_letter(c);
        let y = if c.wt == 0 { b' ' } else { c.wt };
        if x == b' ' && y == b' ' {
            continue;
        }
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
        p.path(path);
        p.end();
    }
    for u in untracked {
        p.text("? ");
        p.path(u);
        p.end();
    }
}

struct LongContext<'a> {
    info: &'a BranchInfo,
    /// For a detached HEAD: the checkout target and whether HEAD is still
    /// there (`None`: no checkout in the reflog).
    detached: Option<Option<(String, bool)>>,
    hints: bool,
    merging: bool,
    mode: Untracked,
}

fn label_of(letter: u8) -> &'static str {
    match letter {
        b'A' => "new file:",
        b'D' => "deleted:",
        b'T' => "typechange:",
        _ => "modified:",
    }
}

fn conflict_label(code: &str) -> &'static str {
    match code {
        "DD" => "both deleted:",
        "AU" => "added by us:",
        "UD" => "deleted by them:",
        "UA" => "added by them:",
        "DU" => "deleted by us:",
        "AA" => "both added:",
        _ => "both modified:",
    }
}

/// The long format (Documentation/git-status.adoc, "OUTPUT").
fn long(
    p: &mut Printer,
    ctx: &LongContext,
    entries: &[(&Vec<u8>, &Change)],
    untracked: &[Vec<u8>],
) {
    let info = ctx.info;
    let hint = |p: &mut Printer, text: &str| {
        if ctx.hints {
            p.text("  (");
            p.text(text);
            p.text(")\n");
        }
    };
    match (&info.branch, &ctx.detached) {
        (Some(b), _) => p.text(&format!("On branch {b}\n")),
        (None, Some(Some((name, true)))) => p.text(&format!("HEAD detached at {name}\n")),
        (None, Some(Some((name, false)))) => p.text(&format!("HEAD detached from {name}\n")),
        (None, _) => p.text("Not currently on any branch.\n"),
    }
    let unborn = info.head.is_none();
    if let (Some((up, state)), false) = (&info.upstream, unborn) {
        tracking(p, up, *state, ctx.hints);
        p.text("\n");
    }
    let staged: Vec<(&Vec<u8>, u8)> = entries
        .iter()
        .filter(|(_, c)| conflict_code(c).is_none())
        .map(|(path, c)| (*path, index_letter(c)))
        .filter(|(_, x)| *x != b' ')
        .collect();
    let unmerged: Vec<(&Vec<u8>, &str)> = entries
        .iter()
        .filter_map(|(path, c)| conflict_code(c).map(|code| (*path, code)))
        .collect();
    let changed: Vec<(&Vec<u8>, u8)> = entries
        .iter()
        .filter(|(_, c)| conflict_code(c).is_none() && c.wt != 0 && c.wt != b' ')
        .map(|(path, c)| (*path, c.wt))
        .collect();
    if ctx.merging {
        if unmerged.is_empty() {
            p.text("All conflicts fixed but you are still merging.\n");
            hint(p, "use \"git commit\" to conclude merge");
        } else {
            p.text("You have unmerged paths.\n");
            hint(p, "fix conflicts and run \"git commit\"");
            hint(p, "use \"git merge --abort\" to abort the merge");
        }
        p.text("\n");
    }
    if unborn {
        p.text("\nNo commits yet\n\n");
    }
    if !staged.is_empty() {
        p.text("Changes to be committed:\n");
        if ctx.merging {
            // No unstage hint while merging.
        } else if unborn {
            hint(p, "use \"git rm --cached <file>...\" to unstage");
        } else {
            hint(p, "use \"git restore --staged <file>...\" to unstage");
        }
        for (path, x) in &staged {
            p.text(&format!("\t{:<12}", label_of(*x)));
            p.path(path);
            p.text("\n");
        }
        p.text("\n");
    }
    if !unmerged.is_empty() {
        p.text("Unmerged paths:\n");
        if !ctx.merging {
            hint(p, "use \"git restore --staged <file>...\" to unstage");
        }
        if unmerged.iter().all(|(_, c)| *c == "UU" || *c == "AA") {
            hint(p, "use \"git add <file>...\" to mark resolution");
        } else {
            hint(p, "use \"git add/rm <file>...\" as appropriate to mark resolution");
        }
        for (path, code) in &unmerged {
            p.text(&format!("\t{:<17}", conflict_label(code)));
            p.path(path);
            p.text("\n");
        }
        p.text("\n");
    }
    if !changed.is_empty() {
        p.text("Changes not staged for commit:\n");
        if changed.iter().any(|(_, y)| *y == b'D') {
            hint(p, "use \"git add/rm <file>...\" to update what will be committed");
        } else {
            hint(p, "use \"git add <file>...\" to update what will be committed");
        }
        hint(p, "use \"git restore <file>...\" to discard changes in working directory");
        for (path, y) in &changed {
            p.text(&format!("\t{:<12}", label_of(*y)));
            p.path(path);
            p.text("\n");
        }
        p.text("\n");
    }
    if !untracked.is_empty() {
        p.text("Untracked files:\n");
        hint(p, "use \"git add <file>...\" to include in what will be committed");
        for u in untracked {
            p.text("\t");
            p.path(u);
            p.text("\n");
        }
        p.text("\n");
    }
    let (text, hint_text) = if !staged.is_empty() {
        if ctx.mode != Untracked::No {
            return;
        }
        ("Untracked files not listed", " (use -u option to show untracked files)")
    } else if !changed.is_empty() || !unmerged.is_empty() {
        (
            "no changes added to commit",
            " (use \"git add\" and/or \"git commit -a\")",
        )
    } else if !untracked.is_empty() {
        (
            "nothing added to commit but untracked files present",
            " (use \"git add\" to track)",
        )
    } else if unborn {
        (
            "nothing to commit",
            " (create/copy files and use \"git add\" to track)",
        )
    } else if ctx.mode == Untracked::No {
        ("nothing to commit", " (use -u to show untracked files)")
    } else {
        ("nothing to commit, working tree clean", "")
    };
    p.text(text);
    if ctx.hints {
        p.text(hint_text);
    }
    p.text("\n");
}

/// The upstream lines of the long format.
fn tracking(p: &mut Printer, up: &str, state: Tracking, hints: bool) {
    let hint = |p: &mut Printer, text: &str| {
        if hints {
            p.text("  (");
            p.text(text);
            p.text(")\n");
        }
    };
    let commits = |n: usize| if n == 1 { "commit" } else { "commits" };
    match state {
        Tracking::Gone => {
            p.text(&format!(
                "Your branch is based on '{up}', but the upstream is gone.\n"
            ));
            hint(p, "use \"git branch --unset-upstream\" to fixup");
        }
        Tracking::Counts(0, 0) => {
            p.text(&format!("Your branch is up to date with '{up}'.\n"));
        }
        Tracking::Counts(a, 0) => {
            p.text(&format!(
                "Your branch is ahead of '{up}' by {a} {}.\n",
                commits(a)
            ));
            hint(p, "use \"git push\" to publish your local commits");
        }
        Tracking::Counts(0, b) => {
            p.text(&format!(
                "Your branch is behind '{up}' by {b} {}, and can be fast-forwarded.\n",
                commits(b)
            ));
            hint(p, "use \"git pull\" to update your local branch");
        }
        Tracking::Counts(a, b) => {
            p.text(&format!(
                "Your branch and '{up}' have diverged,\n\
                 and have {a} and {b} different commits each, respectively.\n"
            ));
            hint(p, "use \"git pull\" if you want to integrate the remote branch with yours");
        }
        Tracking::Different => {
            p.text(&format!(
                "Your branch and '{up}' refer to different commits.\n"
            ));
            hint(p, "use \"git status --ahead-behind\" for details");
        }
    }
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


/// The `-b` header of the short format and the porcelain formats.
fn header(p: &mut Printer, version: u8, info: &BranchInfo) {
    if version == 1 {
        p.text("## ");
        match (&info.branch, info.head) {
            (Some(b), None) => p.text(&format!("No commits yet on {b}")),
            (Some(b), Some(_)) => p.text(b),
            (None, _) => p.text("HEAD (no branch)"),
        }
        if let Some((up, state)) = &info.upstream {
            {
                p.text(&format!("...{up}"));
                match *state {
                    Tracking::Gone => p.text(" [gone]"),
                    Tracking::Different => p.text(" [different]"),
                    Tracking::Counts(0, 0) => {}
                    Tracking::Counts(a, 0) => p.text(&format!(" [ahead {a}]")),
                    Tracking::Counts(0, b) => p.text(&format!(" [behind {b}]")),
                    Tracking::Counts(a, b) => p.text(&format!(" [ahead {a}, behind {b}]")),
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
        match *state {
            Tracking::Gone => {}
            Tracking::Different => {
                p.text("# branch.ab +? -?");
                p.end();
            }
            Tracking::Counts(a, b) => {
                p.text(&format!("# branch.ab +{a} -{b}"));
                p.end();
            }
        }
    }
}
