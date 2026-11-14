// SPDX-License-Identifier: MIT
//! `git status` (Documentation/git-status.adoc): the long format (branch
//! and upstream lines, sections with `advice.statusHints`, closing
//! summary), `-s`/`--short`, `--porcelain[=v1]` and `--porcelain=v2`, with
//! `-z`, `-b`, `--[no-]ahead-behind`, `--untracked-files=no|normal|all` and
//! `--ignored[=traditional|matching|no]` (the walk is in
//! [`crate::untracked`]) and staged renames (`status.renames`,
//! `--no-renames`, `-M`). Submodule details, colour and the state banners
//! other than merging are not implemented yet.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use rgit_hash::ObjectId;
use rgit_index::Entry;
use rgit_object::{Object, ObjectKind};
use rgit_pathspec::{ParseOptions, Pathspec};
use rgit_worktree::stat::{Checker, Status};

use super::ls_files::{ExcludeArg, build_excludes};
use crate::messages::status as msg;
use crate::prelude::*;
use crate::quote::{relative_to, write_quoted};
use crate::refs_support::Revs;
use crate::repo::Mode;
use crate::repository::{self, Repo};
use crate::untracked::{self, IgnoredMode};

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
    /// A staged rename or copy to this path: source path, `R`/`C`, score
    /// (percent) and the source's HEAD mode and id.
    pub(crate) rename: Option<(Vec<u8>, u8, u32, u32, ObjectId)>,
    /// The source of a staged rename (shown at the new path only).
    pub(crate) renamed_away: bool,
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
    if let Some(r) = &c.rename {
        return r.1;
    }
    if c.renamed_away {
        return b' ';
    }
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

/// The untracked and the ignored paths of the work tree, as `status`
/// shows them.
pub(crate) fn untracked_and_ignored(
    repo: &Repo,
    work_tree: &Path,
    index: &rgit_index::Index,
    pathspec: Option<&Pathspec>,
    mode: Untracked,
    ignored: IgnoredMode,
) -> Result<untracked::StatusLists, Die> {
    if mode == Untracked::No {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut excludes = build_excludes(repo, &[ExcludeArg::Standard])?;
    untracked::use_skip_worktree_ignores(repo, index, b".gitignore", &mut excludes);
    let roots = untracked::Walker {
        root: work_tree,
        index,
        excludes: Some(&mut excludes),
        pathspec,
        scan_excluded: ignored != IgnoredMode::No,
    }
    .walk();
    Ok(untracked::status_lists(&roots, mode == Untracked::All, ignored))
}

pub(crate) struct Printer {
    pub(crate) nul: bool,
    pub(crate) quote_high: bool,
    /// The short format and porcelain v1 also quote paths containing a
    /// space.
    pub(crate) space_quote: bool,
    /// Show paths relative to this directory (empty or ending in `/`).
    pub(crate) prefix: Vec<u8>,
    pub(crate) out: Vec<u8>,
}

impl Printer {
    pub(crate) fn path(&mut self, p: &[u8]) {
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
    pub(crate) fn end(&mut self) {
        self.out.push(if self.nul { 0 } else { b'\n' });
    }
    pub(crate) fn text(&mut self, s: &str) {
        self.out.extend_from_slice(s.as_bytes());
    }
}

/// How the branch relates to its upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tracking {
    /// The upstream ref does not exist.
    Gone,
    /// Ahead and behind counts.
    Counts(usize, usize),
    /// Different commits, not counted (`--no-ahead-behind`).
    Different,
}

/// The branch header data.
pub(crate) struct BranchInfo {
    pub(crate) head: Option<ObjectId>,
    pub(crate) branch: Option<String>,
    /// Upstream short name and how the branch relates to it.
    pub(crate) upstream: Option<(String, Tracking)>,
}

pub(crate) fn branch_info(repo: &mut Repo, revs: &Revs, count: bool) -> Result<BranchInfo, Die> {
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
    let ignored: RefCell<Option<String>> = RefCell::new(None);
    let ahead_behind = Cell::new(-1);
    let no_renames = Cell::new(-1);
    let find_renames: RefCell<Option<Option<String>>> = RefCell::new(None);
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
            Opt::string(None, "ignored", &ignored, "mode", msg::HELP_IGNORED)
                .optional(Some("traditional")),
            Opt::bool(None, "no-renames", &no_renames, msg::HELP_NO_RENAMES),
            Opt::callback(
                'M',
                "find-renames",
                Some("n"),
                msg::HELP_FIND_RENAMES,
                |v, unset| {
                    if unset {
                        no_renames.set(1);
                    } else {
                        find_renames.replace(Some(v.map(str::to_string)));
                    }
                    Ok(())
                },
            )
            .optional(None),
        ],
    );
    let args = parser.parse(argv)?.args;
    drop(parser);
    let find_renames = find_renames.into_inner();

    let porcelain_version = match porcelain.borrow().as_deref() {
        None | Some("v1") => 1,
        Some("v2") => 2,
        Some(v) => return Err(Die::Fatal(msg::unsupported_porcelain(v))),
    };

    let mut repo = Repo::open(ctx, Mode::RequiredQuiet)?;
    let advisor = ctx.advisor(&repo.config)?;
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

    let core = rename_options(&repo, no_renames.get(), find_renames.as_ref())?;
    staged_renames(&repo, &mut changes, &core);

    let root = work_tree.as_os_str().as_encoded_bytes().to_vec();
    let opts = ParseOptions {
        prefix: repo.prefix(),
        root: Some(root),
        globals: repository::pathspec_globals(ctx),
        ..Default::default()
    };
    let pathspec = Pathspec::parse(&args, &opts).map_err(|e| Die::Fatal(e.to_string()))?;
    let wanted = |p: &[u8]| pathspec.is_empty() || pathspec.match_path(p).is_some();

    let ignored_mode = match ignored.borrow().as_deref() {
        None | Some("no") => IgnoredMode::No,
        Some("traditional") => IgnoredMode::Traditional,
        Some("matching") => IgnoredMode::Matching,
        Some(m) => return Err(Die::Fatal(msg::invalid_ignored(m))),
    };
    if ignored_mode == IgnoredMode::Matching && mode == Untracked::No {
        return Err(Die::Fatal(msg::IGNORED_UNTRACKED_COMBINATION.to_string()));
    }
    let (untracked_paths, ignored_paths) = untracked_and_ignored(
        &repo,
        &work_tree,
        &index,
        (!pathspec.is_empty()).then_some(&pathspec),
        mode,
        ignored_mode,
    )?;

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
                hints: advisor
                    .enabled(Advice::StatusHints)
                    .map_err(Die::Fatal)?,
                merging: repo.setup.git_dir.join("MERGE_HEAD").exists(),
                sequencing: crate::wt_status::sequencing(&repo),
                mode,
            };
            long(&mut p, &ctx, &entries, &untracked_paths, &ignored_paths);
        }
        Format::Short | Format::Porcelain(1) => {
            if show_branch {
                header(&mut p, 1, &info);
            }
            short(&mut p, &entries, &untracked_paths, &ignored_paths);
        }
        Format::Porcelain(_) => {
            if show_branch {
                header(&mut p, 2, &info);
            }
            porcelain_v2(&mut p, &entries, &untracked_paths, &ignored_paths, zero);
        }
    }
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(&p.out);
    let _ = o.flush();
    Ok(0)
}

/// Rename detection for the staged changes: `--no-renames`,
/// `-M[<n>]`, `status.renames` (else `diff.renames`, default on) and
/// `status.renameLimit` (else `diff.renameLimit`).
pub(crate) fn rename_options(
    repo: &Repo,
    no_renames: i32,
    find: Option<&Option<String>>,
) -> Result<rgit_diff::diffcore::Options, Die> {
    use rgit_diff::diffcore::{Detect, Options, parse_score};
    let mut core = Options::default();
    let entry = repo
        .config
        .get("status.renames")
        .or_else(|| repo.config.get("diff.renames"));
    core.detect = match entry {
        None => Detect::Renames,
        Some(e) => {
            let v = e.value.as_ref().map(|v| v.to_string()).unwrap_or_default();
            match v.to_ascii_lowercase().as_str() {
                "copies" | "copy" => Detect::Copies,
                _ if e.bool().map_err(|e| Die::Fatal(e.to_string()))? => Detect::Renames,
                _ => Detect::Off,
            }
        }
    };
    let limit = repo
        .config
        .get("status.renamelimit")
        .or_else(|| repo.config.get("diff.renamelimit"));
    if let Some(e) = limit {
        let n = e
            .int(rgit_config::IntWidth::I32)
            .map_err(|e| Die::Fatal(e.to_string()))?;
        core.rename_limit = usize::try_from(n.max(0)).unwrap_or(0);
    }
    match no_renames {
        1 => core.detect = Detect::Off,
        0 if core.detect == Detect::Off => core.detect = Detect::Renames,
        _ => {}
    }
    if let Some(score) = find {
        // `-M` keeps copy detection from `status.renames=copies`.
        if core.detect == Detect::Off {
            core.detect = Detect::Renames;
        }
        // Unlike the diff commands, status takes `-M=<n>` and ignores
        // what does not parse (observed).
        if let Some(v) = score {
            if let (Some(s), _) = parse_score(v.trim_start_matches('=')) {
                core.rename_score = s;
            }
        }
    }
    Ok(core)
}

/// Find staged renames and copies (HEAD against the index).
pub(crate) fn staged_renames(
    repo: &Repo,
    changes: &mut BTreeMap<Vec<u8>, Change>,
    core: &rgit_diff::diffcore::Options,
) {
    use rgit_diff::diffcore::{self, FilePair, Side};
    if core.detect == diffcore::Detect::Off {
        return;
    }
    let side = |path: &[u8], mode: u32, oid: ObjectId| Side {
        path: path.to_vec(),
        mode,
        oid,
        extra: (),
    };
    let mut input: Vec<FilePair<()>> = Vec::new();
    for (path, c) in changes.iter() {
        if conflict_code(c).is_some() || index_letter(c) == b' ' {
            continue;
        }
        let one = c.head.map(|(m, id)| side(path, m, id));
        let two = match c.index {
            Some((m, id, false)) => Some(side(path, m, id)),
            _ => None,
        };
        input.push(FilePair::new(one, two));
    }
    let contents = |s: &Side<()>| match repo.read_object(&s.oid) {
        Some(Object::Blob(b)) => b.data,
        _ => Vec::new(),
    };
    // status does not warn about the rename limit.
    let (out, _) = diffcore::run(input, core, &contents);
    for p in out {
        let letter = match p.status {
            diffcore::Status::Renamed => b'R',
            diffcore::Status::Copied => b'C',
            _ => continue,
        };
        let (Some(one), Some(two)) = (&p.one, &p.two) else {
            continue;
        };
        if letter == b'R' {
            if let Some(src) = changes.get_mut(&one.path) {
                src.renamed_away = true;
            }
        }
        if let Some(dst) = changes.get_mut(&two.path) {
            dst.rename = Some((one.path.clone(), letter, p.percent(), one.mode, one.oid));
        }
    }
}

/// Short format and porcelain v1: `XY <path>`.
fn short(
    p: &mut Printer,
    entries: &[(&Vec<u8>, &Change)],
    untracked: &[Vec<u8>],
    ignored: &[Vec<u8>],
) {
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
        match &c.rename {
            Some((old, ..)) if p.nul => {
                p.path(path);
                p.end();
                p.path(old);
            }
            Some((old, ..)) => {
                p.path(old);
                p.text(" -> ");
                p.path(path);
            }
            None => p.path(path),
        }
        p.end();
    }
    for u in untracked {
        p.text("?? ");
        p.path(u);
        p.end();
    }
    for u in ignored {
        p.text("!! ");
        p.path(u);
        p.end();
    }
}

fn porcelain_v2(
    p: &mut Printer,
    entries: &[(&Vec<u8>, &Change)],
    untracked: &[Vec<u8>],
    ignored: &[Vec<u8>],
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
        if let Some((old, letter, score, hm, hid)) = &c.rename {
            p.text(&format!(
                "2 {} N... {hm:06o} {im:06o} {:06o} {} {} {}{score} ",
                String::from_utf8_lossy(&xy),
                c.wt_mode.unwrap_or(0),
                hid.to_hex(),
                iid.to_hex(),
                char::from(*letter),
            ));
            p.path(path);
            if p.nul {
                p.end();
            } else {
                p.text("\t");
            }
            p.path(old);
            p.end();
            continue;
        }
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
    for u in ignored {
        p.text("! ");
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
    sequencing: Option<crate::wt_status::Sequencing>,
    mode: Untracked,
}

fn label_of(letter: u8) -> &'static str {
    match letter {
        b'A' => msg::LABEL_NEW,
        b'D' => msg::LABEL_DELETED,
        b'T' => msg::LABEL_TYPECHANGE,
        b'R' => msg::LABEL_RENAMED,
        b'C' => msg::LABEL_COPIED,
        _ => msg::LABEL_MODIFIED,
    }
}

fn conflict_label(code: &str) -> &'static str {
    match code {
        "DD" => msg::LABEL_BOTH_DELETED,
        "AU" => msg::LABEL_ADDED_BY_US,
        "UD" => msg::LABEL_DELETED_BY_THEM,
        "UA" => msg::LABEL_ADDED_BY_THEM,
        "DU" => msg::LABEL_DELETED_BY_US,
        "AA" => msg::LABEL_BOTH_ADDED,
        _ => msg::LABEL_BOTH_MODIFIED,
    }
}

/// The long format (Documentation/git-status.adoc, "OUTPUT").
fn long(
    p: &mut Printer,
    ctx: &LongContext,
    entries: &[(&Vec<u8>, &Change)],
    untracked: &[Vec<u8>],
    ignored: &[Vec<u8>],
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
        (Some(b), _) => p.text(&msg::on_branch(b)),
        (None, Some(Some((name, true)))) => p.text(&msg::detached_at(name)),
        (None, Some(Some((name, false)))) => p.text(&msg::detached_from(name)),
        (None, _) => p.text(msg::NOT_ON_BRANCH),
    }
    p.text("\n");
    let unborn = info.head.is_none();
    if let (Some((up, state)), false) = (&info.upstream, unborn) {
        tracking(p, up, *state, ctx.hints);
        p.text("\n");
    }
    type Staged<'a> = (&'a Vec<u8>, u8, Option<&'a Vec<u8>>);
    let staged: Vec<Staged<'_>> = entries
        .iter()
        .filter(|(_, c)| conflict_code(c).is_none())
        .map(|(path, c)| (*path, index_letter(c), c.rename.as_ref().map(|r| &r.0)))
        .filter(|(_, x, _)| *x != b' ')
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
            p.text(msg::ALL_CONFLICTS_FIXED);
            p.text("\n");
            hint(p, msg::HINT_CONCLUDE_MERGE);
        } else {
            p.text(msg::UNMERGED_PATHS);
            p.text("\n");
            hint(p, msg::HINT_FIX_CONFLICTS);
            hint(p, msg::HINT_MERGE_ABORT);
        }
        p.text("\n");
    }
    // A cherry-pick or revert that stopped has no unstage hints either.
    let picking = ctx.sequencing.as_ref().is_some_and(|s| s.commit.is_some());
    if let Some(seq) = &ctx.sequencing {
        for l in crate::wt_status::sequencing_lines(seq, !unmerged.is_empty(), ctx.hints) {
            p.text(&l);
            p.text("\n");
        }
        p.text("\n");
    }
    if unborn {
        p.text("\n");
        p.text(msg::NO_COMMITS_YET);
        p.text("\n\n");
    }
    if !staged.is_empty() {
        p.text(msg::STAGED_HEADER);
        p.text("\n");
        if ctx.merging || picking {
            // No unstage hint while merging.
        } else if unborn {
            hint(p, msg::HINT_UNSTAGE_INITIAL);
        } else {
            hint(p, msg::HINT_UNSTAGE);
        }
        for (path, x, old) in &staged {
            p.text(&format!("\t{:<12}", label_of(*x)));
            if let Some(old) = old {
                p.path(old);
                p.text(" -> ");
            }
            p.path(path);
            p.text("\n");
        }
        p.text("\n");
    }
    if !unmerged.is_empty() {
        p.text(msg::UNMERGED_HEADER);
        p.text("\n");
        if !ctx.merging && !picking {
            hint(p, msg::HINT_UNSTAGE);
        }
        if unmerged.iter().all(|(_, c)| *c == "UU" || *c == "AA") {
            hint(p, msg::HINT_MARK_RESOLUTION);
        } else {
            hint(p, msg::HINT_MARK_RESOLUTION_RM);
        }
        for (path, code) in &unmerged {
            p.text(&format!("\t{:<17}", conflict_label(code)));
            p.path(path);
            p.text("\n");
        }
        p.text("\n");
    }
    if !changed.is_empty() {
        p.text(msg::UNSTAGED_HEADER);
        p.text("\n");
        if changed.iter().any(|(_, y)| *y == b'D') {
            hint(p, msg::HINT_ADD_RM);
        } else {
            hint(p, msg::HINT_ADD);
        }
        hint(p, msg::HINT_RESTORE);
        for (path, y) in &changed {
            p.text(&format!("\t{:<12}", label_of(*y)));
            p.path(path);
            p.text("\n");
        }
        p.text("\n");
    }
    if !untracked.is_empty() {
        p.text(msg::UNTRACKED_HEADER);
        p.text("\n");
        hint(p, msg::HINT_INCLUDE);
        for u in untracked {
            p.text("\t");
            p.path(u);
            p.text("\n");
        }
        p.text("\n");
    }
    if !ignored.is_empty() {
        p.text(msg::IGNORED_HEADER);
        p.text("\n");
        hint(p, msg::HINT_INCLUDE_IGNORED);
        for u in ignored {
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
        msg::UNTRACKED_NOT_LISTED
    } else if !changed.is_empty() || !unmerged.is_empty() {
        msg::NOTHING_ADDED
    } else if !untracked.is_empty() {
        msg::UNTRACKED_ONLY
    } else if unborn {
        msg::NOTHING_INITIAL
    } else if ctx.mode == Untracked::No {
        msg::NOTHING_NO_UNTRACKED
    } else {
        msg::NOTHING_CLEAN
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
    let line = |p: &mut Printer, text: &str| {
        p.text(text);
        p.text("\n");
    };
    match state {
        Tracking::Gone => {
            line(p, &msg::upstream_gone(up));
            hint(p, msg::HINT_UNSET_UPSTREAM);
        }
        Tracking::Counts(0, 0) => {
            line(p, &msg::up_to_date(up));
        }
        Tracking::Counts(a, 0) => {
            line(p, &msg::ahead(up, a));
            hint(p, msg::HINT_PUSH);
        }
        Tracking::Counts(0, b) => {
            line(p, &msg::behind(up, b));
            hint(p, msg::HINT_PULL_FF);
        }
        Tracking::Counts(a, b) => {
            line(p, &msg::diverged(up, a, b));
            hint(p, msg::HINT_PULL_MERGE);
        }
        Tracking::Different => {
            line(p, &msg::different(up));
            hint(p, msg::HINT_AHEAD_BEHIND);
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
pub(crate) fn header(p: &mut Printer, version: u8, info: &BranchInfo) {
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
