// SPDX-License-Identifier: MIT
//! `git status` (Documentation/git-status.adoc): the long format (branch
//! and upstream lines, sections with `advice.statusHints`, closing
//! summary), `-s`/`--short`, `--porcelain[=v1]` and `--porcelain=v2`, with
//! `-z`, `-b`, `--[no-]ahead-behind`, `--untracked-files=no|normal|all` and
//! `--ignored[=traditional|matching|no]` (the walk is in
//! [`crate::untracked`]) and staged renames (`status.renames`,
//! `--no-renames`, `-M`). Submodules show new commits, modified and
//! untracked content (`--ignore-submodules`, `submodule.<name>.ignore`,
//! `diff.ignoreSubmodules`). Not implemented yet: `status.submoduleSummary`,
//! colour, and the state banners other than merging.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use rgit_hash::ObjectId;
use rgit_index::Entry;
use rgit_object::{Object, ObjectKind};
use rgit_pathspec::{ParseOptions, Pathspec};
use rgit_worktree::ignore::Excludes;
use rgit_worktree::stat::{Checker, FileStat, Status};

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
    /// The work tree state of a submodule (gitlink in the index).
    pub(crate) sub: Option<crate::submodules::WtState>,
}

/// How submodules are inspected (see [`check_worktree`]).
pub(crate) struct SubContext<'a> {
    pub(crate) rules: &'a crate::submodules::IgnoreRules,
    /// Look for untracked files in submodules (not with `-uno`).
    pub(crate) untracked: bool,
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
    let excludes = untracked_excludes(repo, index)?;
    Ok(walk_untracked(work_tree, index, excludes, pathspec, mode, ignored))
}

/// The exclude rules of the untracked walk.
fn untracked_excludes(repo: &Repo, index: &rgit_index::Index) -> Result<Excludes, Die> {
    let mut excludes = build_excludes(repo, &[ExcludeArg::Standard])?;
    untracked::use_skip_worktree_ignores(repo, index, b".gitignore", &mut excludes);
    Ok(excludes)
}

/// The global exclude files the untracked cache watches.
fn untracked_cache_files(repo: &Repo) -> crate::untracked_cache::GlobalFiles {
    let env = rgit_config::SystemEnv::default();
    let excludes_file = match repo.config.path("core.excludesfile", &env) {
        Ok(Some(p)) => Some(p),
        _ => rgit_worktree::ignore::default_excludes_file(|k| std::env::var_os(k)),
    };
    crate::untracked_cache::GlobalFiles {
        info_exclude: repo.setup.common_dir.join("info").join("exclude"),
        excludes_file,
    }
}

/// The trace2 data of the untracked walk (the counters only when the
/// untracked cache was used), as git reports them.
fn trace_read_directory(stats: Option<crate::untracked_cache::Stats>) {
    let _status = rgit_trace::trace2::region("status", "untracked", true);
    let _dir = rgit_trace::trace2::region("dir", "read_directory", true);
    let data = |k: &str, v: String| rgit_trace::trace2::data("read_directory", k, &v, true);
    data("path", String::new());
    if let Some(s) = stats {
        data("node-creation", s.node_creation.to_string());
        data("gitignore-invalidation", s.gitignore_invalidation.to_string());
        data("directory-invalidation", s.directory_invalidation.to_string());
        data("opendir", s.opendir.to_string());
    }
}

/// [`untracked_and_ignored`] with the exclude rules already read (needs no
/// repository, so it can run on a thread of its own).
fn walk_untracked(
    work_tree: &Path,
    index: &rgit_index::Index,
    mut excludes: Excludes,
    pathspec: Option<&Pathspec>,
    mode: Untracked,
    ignored: IgnoredMode,
) -> untracked::StatusLists {
    let roots = untracked::Walker {
        root: work_tree,
        index,
        excludes: Some(&mut excludes),
        pathspec,
        scan_excluded: ignored != IgnoredMode::No,
    }
    .walk();
    untracked::status_lists(&roots, mode == Untracked::All, ignored)
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
    /// `color.status`: the colours of the short format.
    pub(crate) colors: Option<StatusColors>,
}

/// The colours of `color.status.<slot>` (also `status.color.<slot>`;
/// Documentation/config/color.adoc). An empty colour prints plain text.
#[derive(Debug, Clone, Default)]
pub(crate) struct StatusColors {
    header: String,
    added: String,
    changed: String,
    untracked: String,
    ignored: String,
    branch: String,
    nobranch: String,
    unmerged: String,
    local: String,
    remote: String,
}

impl StatusColors {
    /// The colours when `color.status` (else `color.ui`) asks for them.
    pub(crate) fn load(config: &rgit_config::ConfigSet) -> Result<Option<StatusColors>, Die> {
        if !crate::color::want(config, Some("color.status"), None)? {
            return Ok(None);
        }
        let slot = |names: &[&str], default: &str| -> Result<String, Die> {
            let mut value = default.to_owned();
            for name in names {
                for key in [format!("status.color.{name}"), format!("color.status.{name}")] {
                    if config.get(&key).is_some() {
                        value = crate::color::slot(config, &key, default)?;
                    }
                }
            }
            Ok(value)
        };
        let header = slot(&["header"], "")?;
        Ok(Some(StatusColors {
            added: slot(&["added", "updated"], "\x1b[32m")?,
            changed: slot(&["changed"], "\x1b[31m")?,
            untracked: slot(&["untracked"], "\x1b[31m")?,
            ignored: "\x1b[31m".to_owned(),
            branch: slot(&["branch"], &header)?,
            nobranch: slot(&["nobranch"], "\x1b[31m")?,
            unmerged: slot(&["unmerged"], "\x1b[31m")?,
            local: slot(&["localBranch"], "\x1b[32m")?,
            remote: slot(&["remoteBranch"], "\x1b[31m")?,
            header,
        }))
    }
}

/// `text` in `color` (plain when the colour is empty).
fn paint(out: &mut Vec<u8>, color: &str, text: &[u8]) {
    if color.is_empty() {
        out.extend_from_slice(text);
    } else {
        out.extend_from_slice(color.as_bytes());
        out.extend_from_slice(text);
        out.extend_from_slice(b"\x1b[m");
    }
}

/// Colour the long format in `out` up to `end` (the closing summary line
/// stays plain): headings and advice in the header colour, the entries of
/// each section in the colour of the section, the branch name in the
/// branch colour. Blank lines get the header colour too, except those
/// after the upstream lines and after the untracked and ignored files.
fn colorize_long(out: &[u8], end: usize, c: &StatusColors) -> Vec<u8> {
    #[derive(Clone, Copy, PartialEq)]
    enum Section {
        None,
        Tracking,
        Added,
        Unmerged,
        Changed,
        Untracked,
        Ignored,
    }
    let mut res = Vec::with_capacity(out.len() * 2);
    let mut section = Section::None;
    for (n, line) in out[..end].split_inclusive(|&b| b == b'\n').enumerate() {
        let (text, nl) = match line.strip_suffix(b"\n") {
            Some(t) => (t, true),
            None => (line, false),
        };
        if n == 0 {
            paint(&mut res, &c.header, b"");
            let on = msg::on_branch("");
            if let Some(b) = text.strip_prefix(on.as_bytes()) {
                paint(&mut res, &c.header, on.as_bytes());
                paint(&mut res, &c.branch, b);
            } else if let Some(split) = [msg::detached_at(""), msg::detached_from("")]
                .iter()
                .find(|p| text.starts_with(p.as_bytes()))
            {
                paint(&mut res, &c.nobranch, split.as_bytes());
                paint(&mut res, &c.branch, &text[split.len()..]);
            } else {
                paint(&mut res, &c.nobranch, text);
            }
        } else if text.is_empty() {
            match section {
                Section::Tracking | Section::Untracked | Section::Ignored => {}
                _ => paint(&mut res, &c.header, b""),
            }
        } else if let Some(entry) = text.strip_prefix(b"\t") {
            let color = match section {
                Section::Added => &c.added,
                Section::Unmerged => &c.unmerged,
                Section::Changed => &c.changed,
                Section::Untracked => &c.untracked,
                Section::Ignored => &c.ignored,
                _ => &c.header,
            };
            paint(&mut res, &c.header, b"\t");
            paint(&mut res, color, entry);
        } else {
            let heading = [
                (msg::STAGED_HEADER, Section::Added),
                (msg::UNMERGED_HEADER, Section::Unmerged),
                (msg::UNSTAGED_HEADER, Section::Changed),
                (msg::UNTRACKED_HEADER, Section::Untracked),
                (msg::IGNORED_HEADER, Section::Ignored),
            ]
            .iter()
            .find(|(h, _)| text == h.as_bytes())
            .map(|(_, s)| *s);
            if let Some(sec) = heading {
                section = sec;
            } else if text.starts_with(b"Your branch ")
                && matches!(section, Section::None | Section::Tracking)
            {
                section = Section::Tracking;
            } else if section == Section::Tracking && !text.starts_with(b"  (") && !text.starts_with(b"and have ") {
                section = Section::None;
            }
            paint(&mut res, &c.header, text);
        }
        if nl {
            res.push(b'\n');
        }
    }
    res.extend_from_slice(&out[end..]);
    res
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
    /// The comparisons of the long format (`status.compareBranches`).
    pub(crate) compare: Vec<crate::compare_branches::Item>,
}

pub(crate) fn branch_info(repo: &mut Repo, revs: &Revs, count: bool) -> Result<BranchInfo, Die> {
    let head = repo.resolve("HEAD");
    let target = repo.refs.head_target();
    let branch = target
        .as_deref()
        .map(|t| t.strip_prefix("refs/heads/").unwrap_or(t).to_string());
    let mut upstream = None;
    let mut upstream_full = None;
    if let Some(b) = &branch {
        let names = revs.with_resolver(|r| {
            let full = r.upstream_ref(b)?;
            let short = r.shorten_ref(&full, false).unwrap_or_else(|_| full.clone());
            let _ = r.take_diagnostics();
            Some((full, short))
        });
        if let Some((full, short)) = names {
            upstream_full = Some(full.clone());
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
    let compare = match &branch {
        Some(b) => crate::compare_branches::plan(repo, b, head, upstream_full.as_deref(), count),
        None => Vec::new(),
    };
    Ok(BranchInfo {
        head,
        branch,
        upstream,
        compare,
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
    let show_stash = Cell::new(-1);
    let no_renames = Cell::new(-1);
    let find_renames: RefCell<Option<Option<String>>> = RefCell::new(None);
    let ignore_submodules: RefCell<Option<String>> = RefCell::new(None);
    let column: RefCell<Option<String>> = RefCell::new(None);
    let verbose = Cell::new(0);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::countup('v', "verbose", &verbose, msg::HELP_VERBOSE),
            Opt::set_int('s', "short", &format, msg::HELP_SHORT, F_SHORT),
            Opt::bool('b', "branch", &branch, msg::HELP_BRANCH),
            Opt::bool(None, "show-stash", &show_stash, msg::HELP_SHOW_STASH),
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
            Opt::string(
                None,
                "ignore-submodules",
                &ignore_submodules,
                "when",
                crate::messages::submodule::HELP_IGNORE_SUBMODULES,
            )
            .optional(Some("all")),
            Opt::callback(None, "column", Some("style"), msg::HELP_COLUMN, |v, unset| {
                *column.borrow_mut() = Some(if unset {
                    "never".to_owned()
                } else {
                    v.unwrap_or("always").to_owned()
                });
                Ok(())
            })
            .optional(None),
            Opt::bool(None, "no-renames", &no_renames, msg::HELP_NO_RENAMES),
            Opt::callback(
                'M',
                "find-renames",
                Some("n"),
                msg::HELP_FIND_RENAMES,
                |v, _| {
                    find_renames.replace(Some(v.map(str::to_string)));
                    Ok(())
                },
            )
            .optional(None)
            .noneg(),
        ],
    );
    let args = parser.parse(argv)?.args;
    drop(parser);
    let find_renames = find_renames.into_inner();

    let porcelain_version = match porcelain.borrow().as_deref() {
        None | Some("v1" | "1") => 1,
        Some("v2" | "2") => 2,
        Some(v) => return Err(Die::Fatal(msg::unsupported_porcelain(v))),
    };

    let mut repo = Repo::open(ctx, Mode::RequiredQuiet)?;
    repo.read_grafts();
    let advisor = ctx.advisor(&repo.config)?;
    // column.ui and column.status, then --column (long format only).
    let mut columns = crate::column::Mode::from_config(&repo.config, Some("status"))?;
    if let Some(c) = column.borrow().as_deref() {
        if let Err(tok) = columns.apply(c) {
            return Err(Die::Error(crate::messages::column::unsupported_option(&tok), 129));
        }
    }
    let columns = columns.active().then_some(columns);
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
        // The configuration does not apply to the porcelain formats.
        -1 if matches!(format, Format::Porcelain(_)) => false,
        -1 => config_bool(&repo, "status.branch")?.unwrap_or(false),
        b => b != 0,
    };
    let show_stash = match show_stash.get() {
        -1 => config_bool(&repo, "status.showstash")?.unwrap_or(false),
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
    // The mode the configuration asks for (the untracked cache is kept for
    // it; another mode on the command line bypasses the cache).
    let config_mode = match repo.config.get("status.showuntrackedfiles") {
        None => Some(Untracked::Normal),
        Some(e) => match e.value.as_ref().map(|v| v.to_string()) {
            None => Some(Untracked::Normal),
            Some(m) => parse_mode(&m).or_else(|| {
                rgit_config::value::parse_bool(Some(m.as_bytes()))
                    .map(|b| if b { Untracked::Normal } else { Untracked::No })
            }),
        },
    };
    let mode = match untracked.borrow().as_deref() {
        Some(m) => parse_mode(m)
            .or_else(|| {
                rgit_config::value::parse_bool(Some(m.as_bytes()))
                    .map(|b| if b { Untracked::Normal } else { Untracked::No })
            })
            .ok_or_else(|| Die::Fatal(msg::invalid_untracked(m)))?,
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
    let mut base_tree = ObjectId::empty_tree(repo.hash);
    if let Some(h) = info.head {
        base_tree = repo
            .peel_to(h, ObjectKind::Tree)
            .ok_or_else(|| Die::Fatal(crate::messages::ls_tree::NOT_A_TREE.to_string()))?;
    }
    let root = work_tree.as_os_str().as_encoded_bytes().to_vec();
    let opts = ParseOptions {
        prefix: repo.prefix(),
        root: Some(root),
        globals: repository::pathspec_globals(ctx),
        implicit_base: rgit_pathspec::ImplicitBase::Top,
        ..Default::default()
    };
    let pathspec = Pathspec::parse(&args, &opts).map_err(|e| Die::Fatal(e.to_string()))?;
    let attr_tree = crate::attributes::PathspecAttrs::prepare(ctx, &mut repo, &pathspec)?;
    let stash_count = if show_stash { stash_entries(&mut repo) } else { 0 };
    let repo = repo;
    // The refreshed index is written back under this lock; quietly
    // skipped when the index is locked or cannot be written.
    let lock = if !ctx.no_optional_locks && std::env::var("GIT_OPTIONAL_LOCKS").as_deref() != Ok("0") {
        let index_path = repo.setup.cwd.join(repo.index_path());
        index_path
            .exists()
            .then(|| rgit_fs::LockFile::acquire(&index_path, &repo.index_lock_options()).ok())
            .flatten()
    } else {
        None
    };
    let (mut index, index_mtime) = repo.read_index()?;
    let checker = repo.checker(index_mtime)?;
    let ignored_mode = match ignored.borrow().as_deref() {
        None | Some("no") => Ok(IgnoredMode::No),
        Some("traditional") => Ok(IgnoredMode::Traditional),
        Some("matching") => Ok(IgnoredMode::Matching),
        Some(m) => Err(Die::Fatal(msg::invalid_ignored(m))),
    };
    // The untracked walk runs on a thread of its own while the tracked
    // files are compared (invalid options end the command later, after
    // the index is written, as before).
    let walk_mode = match ignored_mode {
        Ok(m) if mode != Untracked::No => Some(m),
        _ => None,
    };
    let excludes = match walk_mode {
        Some(_) => Some(untracked_excludes(&repo, &index)?),
        None => None,
    };
    // The untracked cache serves the plain listing in the configured mode.
    let original_uc = index.untracked_cache().cloned();
    let mut cache = None;
    if walk_mode == Some(IgnoredMode::No)
        && pathspec.is_empty()
        && Some(mode) == config_mode
        && let Some(uc) = index.untracked_cache()
    {
        if uc.ident != crate::index_config::untracked_ident(&work_tree) {
            eprintln!("warning: {}", crate::messages::update_index::UNTRACKED_CACHE_DISABLED);
        } else {
            let flags = if mode == Untracked::All {
                crate::index_config::UNTRACKED_FLAGS_ALL
            } else {
                crate::index_config::UNTRACKED_FLAGS_NORMAL
            };
            let mut uc = index.take_untracked_cache().expect("checked");
            if uc.dir_flags != flags {
                uc = rgit_index::UntrackedCache::new(repo.hash, uc.ident, flags);
            }
            cache = Some((uc, untracked_cache_files(&repo)));
        }
    }
    // With `attr:` magic the walk sees every path and the attributes
    // decide afterwards.
    let walk_pathspec = (!pathspec.is_empty() && attr_tree.is_none()).then_some(&pathspec);
    // On an unborn branch everything in the index is new.
    let unborn = info.head.is_none();
    let (metas, statuses, heads, walked) = std::thread::scope(|s| {
        let index = &index;
        let work_tree = &work_tree;
        let walker = walk_mode.zip(excludes).map(|(ignored, mut excludes)| {
            let cache = cache.take();
            s.spawn(move || {
                let started = std::time::Instant::now();
                let (lists, cache) = match cache {
                    Some((mut uc, files)) => {
                        let mut w = crate::untracked_cache::CachedWalk {
                            root: work_tree,
                            index,
                            excludes: &mut excludes,
                            all: mode == Untracked::All,
                            stats: Default::default(),
                            out: Vec::new(),
                        };
                        w.run(&mut uc, &files);
                        let stats = w.stats;
                        ((w.out, Vec::new()), Some((uc, stats)))
                    }
                    None => (
                        walk_untracked(work_tree, index, excludes, walk_pathspec, mode, ignored),
                        None,
                    ),
                };
                (lists, started.elapsed().as_secs_f64(), cache)
            })
        });
        let metas = checker.preload(index.entries(), preload_threads(&repo));
        let statuses = compare_worktree(index, &checker, &metas);
        let heads = if unborn {
            Ok(Vec::new())
        } else {
            head_changes(&repo, &base_tree, index)
        };
        let walked = walker.map(|h| match h.join() {
            Ok(r) => r,
            Err(panic) => std::panic::resume_unwind(panic),
        });
        (metas, statuses, heads, walked)
    });
    // Like git, status refreshes the stat data of the index and writes it
    // back when that changed something or when entries are racily clean.
    let (walked, cached) = match walked {
        Some((lists, secs, cached)) => (Some((lists, secs)), cached),
        None => (None, None),
    };
    if walk_mode.is_some() {
        trace_read_directory(cached.as_ref().map(|c| c.1));
    }
    let mut uc_changed = repo.untracked_toggled.get();
    if let Some((uc, _)) = cached {
        uc_changed = original_uc.as_ref() != Some(&uc)
            || std::env::var_os("GIT_FORCE_UNTRACKED_CACHE").is_some();
        index.set_untracked_cache(Some(uc));
    }
    for &pos in &statuses.refresh {
        if let (Some(meta), Some(entry)) = (metas[pos].as_ref(), index.entry_mut(pos)) {
            checker.refresh_stat(entry, meta);
        }
    }
    if let Some(lock) = lock {
        let racy = index.entries().iter().any(|e| checker.is_racy(e));
        if racy {
            if let Some(reference) = index_mtime {
                let _ = rgit_worktree::stat::smudge_racy_entries(&mut index, reference, &checker);
            }
        }
        if racy || !statuses.refresh.is_empty() || uc_changed {
            let _ = crate::switch_branch::write_index(&repo, &index, lock);
        }
    }
    let index = index;
    let mut heads = heads?;
    heads.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let mut changes: BTreeMap<Vec<u8>, Change> = BTreeMap::new();
    for (path, head) in &heads {
        changes.entry(path.clone()).or_default().head = *head;
    }
    let attrs = attr_tree.map(|t| RefCell::new(crate::attributes::PathspecAttrs::new(&repo, &index, t)));
    let ignore_cli = ignore_submodules
        .borrow()
        .as_deref()
        .map(crate::submodules::parse_ignore_arg)
        .transpose()?;
    let rules = if index.entries().iter().any(|e| e.mode.is_gitlink()) {
        Some(crate::submodules::IgnoreRules::load(
            &repo,
            Some(&index),
            ignore_cli,
            true,
            // `-uno` leaves out untracked files in submodules too, unless
            // an `ignore` setting asks for them.
            if mode == Untracked::No {
                rgit_submodule::Ignore::Untracked
            } else {
                rgit_submodule::Ignore::None
            },
        )?)
    } else {
        None
    };
    let sub_ctx = rules.as_ref().map(|rules| SubContext {
        rules,
        untracked: true,
    });
    // Only paths with something to show get a `Change`: those that differ
    // from HEAD (above), conflicts, intent-to-add entries, submodules and
    // files changed in the work tree.
    let mut next_head = 0;
    for (pos, e) in index.entries().iter().enumerate() {
        let meta = metas[pos].as_ref();
        // `heads` and the index are both sorted by path.
        while heads.get(next_head).is_some_and(|h| h.0.as_slice() < e.path()) {
            next_head += 1;
        }
        let listed = unborn || heads.get(next_head).is_some_and(|h| h.0 == e.path());
        if e.stage().is_conflict() {
            let c = changes.entry(e.path().to_vec()).or_default();
            c.stages[usize::from(e.stage().get()) - 1] = Some((e.mode.0, e.id));
            if let Some(meta) = meta {
                c.wt_mode = Some(wt_mode(meta, e.mode.0));
            }
            continue;
        }
        let mut wt = Change::default();
        let status = statuses.status[pos].unwrap_or(Status::Modified);
        check_worktree_with(&repo, &work_tree, e, &mut wt, sub_ctx.as_ref(), meta, || Ok(status))?;
        let c = if listed {
            changes.entry(e.path().to_vec()).or_default()
        } else if wt.wt != b' ' || e.flags.intent_to_add || e.mode.is_gitlink() {
            let c = changes.entry(e.path().to_vec()).or_default();
            // Not among the HEAD changes: HEAD has the same entry.
            c.head = Some((e.mode.0, e.id));
            c
        } else {
            continue;
        };
        c.index = Some((e.mode.0, e.id, e.flags.intent_to_add));
        c.wt = wt.wt;
        c.wt_mode = wt.wt_mode;
        c.sub = wt.sub;
    }

    // `--ignore-submodules=all` also hides staged submodule changes.
    if ignore_cli == Some(rgit_submodule::Ignore::All) {
        for c in changes.values_mut() {
            if let (Some((0o160000, _)), Some((0o160000, id, _))) = (c.head, c.index) {
                c.head = Some((0o160000, id));
            }
        }
    }
    let core = rename_options(&repo, no_renames.get(), find_renames.as_ref())?;
    staged_renames(&repo, &mut changes, &core);

    let wanted = |p: &[u8]| {
        pathspec.is_empty()
            || match &attrs {
                Some(a) => pathspec
                    .match_path_with(p, None, &mut |p, s| a.borrow_mut().check(p, s))
                    .is_some(),
                None => pathspec.match_path(p).is_some(),
            }
    };

    let ignored_mode = ignored_mode?;
    if ignored_mode == IgnoredMode::Matching && mode == Untracked::No {
        return Err(Die::Fatal(msg::IGNORED_UNTRACKED_COMBINATION.to_string()));
    }
    let ((mut untracked_paths, mut ignored_paths), walk_seconds) =
        walked.unwrap_or_else(|| ((Vec::new(), Vec::new()), 0.0));
    if attrs.is_some() {
        untracked_paths.retain(|p| wanted(p));
        ignored_paths.retain(|p| wanted(p));
    }
    // advice.statusUoption: looking for untracked files took long
    // (GIT_TEST_UF_DELAY_WARNING pretends 3.25 seconds).
    let seconds = if std::env::var_os("GIT_TEST_UF_DELAY_WARNING").is_some() {
        3.25
    } else {
        walk_seconds
    };
    let uf_delay = (format == Format::Long
        && mode != Untracked::No
        && seconds > 2.0
        && advisor.enabled(Advice::StatusUoption).map_err(Die::Fatal)?)
    .then(|| {
        let cached = config_bool(&repo, "core.untrackedcache").ok().flatten() == Some(true)
            && config_bool(&repo, "core.fsmonitor").ok().flatten() == Some(true);
        msg::untracked_delay(seconds, cached)
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
        colors: match format {
            Format::Long | Format::Short => StatusColors::load(&repo.config)?,
            Format::Porcelain(_) => None,
        },
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
                rebasing: crate::wt_status::rebasing(&repo),
                am: crate::wt_status::am_session(&repo),
                bisecting: crate::wt_status::bisecting(&repo),
                mode,
                columns,
                uf_delay,
                summary: submodule_summary(&repo, &entries, rules.as_ref()),
                sparse_percent: sparse_percent(&repo, &index),
            };
            let mut summary = long(&mut p, &ctx, &entries, &untracked_paths, &ignored_paths);
            if let Some(c) = &p.colors {
                let end = summary.unwrap_or(p.out.len());
                let colored = colorize_long(&p.out, end, c);
                let grown = colored.len() - p.out.len();
                p.out = colored;
                summary = summary.map(|s| s + grown);
            }
            let old_len = p.out.len();
            if config_bool(&repo, "status.displaycommentprefix")? == Some(true) {
                // Every line but the summary as a comment, like the commit
                // template.
                let cm = crate::comment::comment(&repo.config)?.or_hash();
                let end = summary.unwrap_or(p.out.len());
                let mut out = Vec::with_capacity(p.out.len() * 2);
                for line in p.out[..end].split_inclusive(|&b| b == b'\n') {
                    out.extend_from_slice(&cm);
                    if !matches!(line.first(), Some(b'\n' | b'\t') | None) {
                        out.push(b' ');
                    }
                    out.extend_from_slice(line);
                }
                out.extend_from_slice(&p.out[end..]);
                p.out = out;
            }
            // -v: the diffs go before the summary line.
            let tail = summary.map_or(0, |s| old_len - s);
            if verbose.get() > 0 {
                let diff =
                    super::commit::verbose::status_diff(&repo, &base_tree, &index, verbose.get())?;
                let at = p.out.len() - tail;
                p.out.splice(at..at, diff);
            }
            if stash_count > 0 {
                p.text(&msg::stash_count(stash_count));
                p.text("\n");
            }
        }
        Format::Short | Format::Porcelain(1) => {
            if show_branch {
                header(&mut p, 1, &info);
            }
            short(
                &mut p,
                &entries,
                &untracked_paths,
                &ignored_paths,
                format == Format::Short,
            );
        }
        Format::Porcelain(_) => {
            if show_branch {
                header(&mut p, 2, &info);
            }
            if stash_count > 0 {
                p.text(&format!("# stash {stash_count}"));
                p.end();
            }
            porcelain_v2(&mut p, &entries, &untracked_paths, &ignored_paths, zero);
        }
    }
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(&p.out);
    let _ = o.flush();
    Ok(0)
}

/// The number of entries of the stash (its reflog).
fn stash_entries(repo: &mut Repo) -> usize {
    if repo.resolve("refs/stash").is_none() {
        return 0;
    }
    let log = repo.setup.common_dir.join("logs/refs/stash");
    std::fs::read(log)
        .map(|t| t.split(|&b| b == b'\n').filter(|l| !l.is_empty()).count())
        .unwrap_or(0)
}

/// How many threads look at the work tree files: `core.preloadIndex`
/// (default on) spreads the `lstat` calls over up to 20 threads.
fn preload_threads(repo: &Repo) -> usize {
    if config_bool(repo, "core.preloadindex").ok().flatten() == Some(false) {
        return 1;
    }
    std::thread::available_parallelism().map_or(1, |n| n.get().min(20))
}

/// The work tree state of the index entries (see [`compare_worktree`]).
struct WorktreeStatus {
    /// By index position: the state of each stage-0 entry that is not a
    /// submodule or intent-to-add (`None` for the others).
    status: Vec<Option<Status>>,
    /// The positions of the entries that get new stat data: files found
    /// unchanged in content.
    refresh: Vec<usize>,
}

/// Compares the stage-0 entries of `index` with their work tree files
/// (`metas`, the `lstat` data by index position). A file that cannot be
/// read counts as modified.
fn compare_worktree(
    index: &rgit_index::Index,
    checker: &Checker,
    metas: &[Option<FileStat>],
) -> WorktreeStatus {
    let mut out = WorktreeStatus {
        status: vec![None; index.len()],
        refresh: Vec::new(),
    };
    for (pos, (e, meta)) in index.entries().iter().zip(metas).enumerate() {
        if e.stage().is_conflict() || e.flags.intent_to_add || e.mode.is_gitlink() {
            continue;
        }
        let status = if e.flags.skip_worktree || e.flags.assume_valid {
            Status::Unchanged
        } else {
            match meta {
                Some(meta) => checker.check_stat(e, meta).unwrap_or(Status::Modified),
                None => Status::Deleted,
            }
        };
        if status == Status::StatOnly {
            out.refresh.push(pos);
        }
        out.status[pos] = Some(status);
    }
    out
}

/// A `HEAD` entry: mode and id.
type HeadEntry = Option<(u32, ObjectId)>;

/// The paths whose `HEAD` entry and index entry differ (staged changes,
/// conflicts, intent-to-add entries), with the `HEAD` side (`None` when
/// the path is not in `HEAD`). Like git, a subtree is not read at all when
/// the cache tree (`TREE` extension) of the index records it as `HEAD` has
/// it.
fn head_changes(
    repo: &Repo,
    tree: &ObjectId,
    index: &rgit_index::Index,
) -> Result<Vec<(Vec<u8>, HeadEntry)>, Die> {
    // The cache tree leaves out conflicts and intent-to-add entries; with
    // those every tree is read.
    let exact = index
        .entries()
        .iter()
        .any(|e| e.stage().is_conflict() || e.flags.intent_to_add);
    let node = if exact {
        None
    } else {
        index.cache_tree().map(|t| &t.root)
    };
    let mut out = Vec::new();
    if head_walk(repo, tree, b"", index.entries(), node, &mut out)? {
        return Ok(out);
    }
    // A tree with duplicate names: like a map from path to entry, the
    // last entry for a path counts.
    let mut head = BTreeMap::new();
    collect_tree(repo, tree, b"", &mut head)?;
    let entries = index.entries();
    let mut out = Vec::new();
    for (path, &(mode, id)) in &head {
        let same = match &entries[index.path_range(path)] {
            [x] => {
                !x.stage().is_conflict() && !x.flags.intent_to_add && x.mode.0 == mode && x.id == id
            }
            _ => false,
        };
        if !same {
            out.push((path.clone(), Some((mode, id))));
        }
    }
    let mut last: Option<&[u8]> = None;
    for e in entries {
        if last != Some(e.path()) && !head.contains_key(e.path()) {
            out.push((e.path().to_vec(), None));
        }
        last = Some(e.path());
    }
    Ok(out)
}

/// Every blob and gitlink of `tree` below `base` (empty or ending in `/`),
/// by path; for duplicate names the last entry counts.
fn collect_tree(
    repo: &Repo,
    tree: &ObjectId,
    base: &[u8],
    out: &mut BTreeMap<Vec<u8>, (u32, ObjectId)>,
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
            out.insert(path, (mode.bits(), e.oid));
        }
    }
    Ok(())
}

/// [`head_changes`] for `tree` at `prefix` (empty or ending in `/`);
/// `entries` are the index entries below `prefix`, `node` the cache tree
/// node of the directory. `false` when a tree has a name twice (then the
/// result is incomplete).
fn head_walk(
    repo: &Repo,
    tree: &ObjectId,
    prefix: &[u8],
    entries: &[Entry],
    node: Option<&rgit_index::TreeNode>,
    out: &mut Vec<(Vec<u8>, HeadEntry)>,
) -> Result<bool, Die> {
    if let Some(n) = node {
        let count = n.entry_count().and_then(|c| usize::try_from(c).ok());
        if n.id() == Some(tree) && count == Some(entries.len()) {
            return Ok(true);
        }
    }
    let Some(Object::Tree(t)) = repo.read_object(tree) else {
        return Err(Die::Fatal(crate::messages::ls_tree::could_not_read(
            &tree.to_hex(),
        )));
    };
    let mut names: Vec<&[u8]> = t.entries.iter().map(|e| e.name.as_slice()).collect();
    names.sort_unstable();
    if names.windows(2).any(|w| w[0] == w[1]) {
        return Ok(false);
    }
    // Index entries that HEAD has at their path or below a directory.
    let mut covered = vec![false; entries.len()];
    let mut path = prefix.to_vec();
    for e in t.entries {
        path.truncate(prefix.len());
        path.extend_from_slice(&e.name);
        let mode = e.mode.canonical();
        if mode.is_tree() {
            path.push(b'/');
            let start = entries.partition_point(|x| x.path() < path.as_slice());
            let len = entries[start..].partition_point(|x| x.path().starts_with(&path));
            covered[start..start + len].fill(true);
            let child = node.and_then(|n| n.child(&e.name));
            if !head_walk(repo, &e.oid, &path, &entries[start..start + len], child, out)? {
                return Ok(false);
            }
            continue;
        }
        let start = entries.partition_point(|x| x.path() < path.as_slice());
        let len = entries[start..].partition_point(|x| x.path() == path.as_slice());
        covered[start..start + len].fill(true);
        let head = (mode.bits(), e.oid);
        let same = match &entries[start..start + len] {
            [x] => {
                !x.stage().is_conflict()
                    && !x.flags.intent_to_add
                    && x.mode.0 == head.0
                    && x.id == head.1
            }
            _ => false,
        };
        if !same {
            out.push((path.clone(), Some(head)));
        }
    }
    // Index paths HEAD does not have.
    let mut last: Option<&[u8]> = None;
    for (e, covered) in entries.iter().zip(covered) {
        if !covered && last != Some(e.path()) {
            out.push((e.path().to_vec(), None));
            last = Some(e.path());
        }
    }
    Ok(true)
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
    is_short: bool,
) {
    let colors = p.colors.clone();
    let paint_to = |p: &mut Printer, color: fn(&StatusColors) -> &String, text: &[u8]| match &colors {
        Some(c) => paint(&mut p.out, color(c), text),
        None => p.out.extend_from_slice(text),
    };
    for (path, c) in entries {
        if let Some(code) = conflict_code(c) {
            paint_to(p, |c| &c.unmerged, code.as_bytes());
            p.text(" ");
            p.path(path);
            p.end();
            continue;
        }
        let x = index_letter(c);
        let mut y = if c.wt == 0 { b' ' } else { c.wt };
        if x == b' ' && y == b' ' {
            continue;
        }
        // The short format tells what changed in a submodule.
        if let (true, b'M', Some(st)) = (is_short, y, &c.sub) {
            if !st.new_commits {
                y = if st.modified { b'm' } else { b'?' };
            }
        }
        if x == b' ' {
            p.out.push(x);
        } else {
            paint_to(p, |c| &c.added, &[x]);
        }
        if y == b' ' {
            p.out.push(y);
        } else {
            paint_to(p, |c| &c.changed, &[y]);
        }
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
        paint_to(p, |c| &c.untracked, b"??");
        p.text(" ");
        p.path(u);
        p.end();
    }
    for u in ignored {
        paint_to(p, |c| &c.ignored, b"!!");
        p.text(" ");
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
        let sub = sub_field(c, hm, im);
        if let Some((old, letter, score, hm, hid)) = &c.rename {
            p.text(&format!(
                "2 {} {sub} {hm:06o} {im:06o} {:06o} {} {} {}{score} ",
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
            "1 {} {sub} {hm:06o} {im:06o} {:06o} {} {} ",
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

/// The `<sub>` field of porcelain v2: `N...` for files, `S<c><m><u>` for
/// submodules.
fn sub_field(c: &Change, head_mode: u32, index_mode: u32) -> String {
    let gitlink = |m: u32| type_bits(m) == 0o160000;
    if !gitlink(head_mode) && !gitlink(index_mode) && !c.wt_mode.is_some_and(gitlink) {
        return "N...".to_owned();
    }
    let st = c.sub.unwrap_or_default();
    let flag = |on: bool, ch: char| if on { ch } else { '.' };
    format!(
        "S{}{}{}",
        flag(st.new_commits, 'C'),
        flag(st.modified, 'M'),
        flag(st.untracked, 'U')
    )
}

struct LongContext<'a> {
    info: &'a BranchInfo,
    /// For a detached HEAD: the checkout target and whether HEAD is still
    /// there (`None`: no checkout in the reflog).
    detached: Option<Option<(String, bool)>>,
    hints: bool,
    merging: bool,
    sequencing: Option<crate::wt_status::Sequencing>,
    rebasing: Option<crate::wt_status::Rebasing>,
    /// An `am` session: whether its current patch is empty.
    am: Option<bool>,
    bisecting: Option<Option<String>>,
    mode: Untracked,
    /// `--column` / `column.status`: untracked files in columns.
    columns: Option<crate::column::Mode>,
    /// The advice that looking for untracked files took long.
    uf_delay: Option<String>,
    /// `status.submoduleSummary`: the blocks for the staged and the
    /// unstaged submodule changes.
    summary: (Vec<u8>, Vec<u8>),
    /// In a sparse checkout: the share of index entries present in the
    /// work tree, in percent.
    sparse_percent: Option<String>,
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

/// The long format (Documentation/git-status.adoc, "OUTPUT"). Returns
/// where the closing summary line ("nothing to commit", ...) starts.
/// `core.sparseCheckout`: the line about the sparse checkout, with 100
/// minus the share of skip-worktree entries (rounded down), or without a
/// number for a sparse index.
fn sparse_percent(repo: &Repo, index: &rgit_index::Index) -> Option<String> {
    if !crate::sparse::enabled(&repo.config) || index.is_empty() {
        return None;
    }
    if crate::sparse_index::is_sparse() {
        return Some(msg::SPARSE_INDEX.to_owned());
    }
    let skipped = index.entries().iter().filter(|e| e.flags.skip_worktree).count();
    Some(msg::sparse_checkout_percent(100 - skipped * 100 / index.len()))
}

fn long(
    p: &mut Printer,
    ctx: &LongContext,
    entries: &[(&Vec<u8>, &Change)],
    untracked: &[Vec<u8>],
    ignored: &[Vec<u8>],
) -> Option<usize> {
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
        (None, _) if ctx.rebasing.is_some() => p.text(&crate::wt_status::rebasing_head_line(
            ctx.rebasing.as_ref().expect("rebasing"),
        )),
        (None, Some(Some((name, true)))) => p.text(&msg::detached_at(name)),
        (None, Some(Some((name, false)))) => p.text(&msg::detached_from(name)),
        (None, _) => p.text(msg::NOT_ON_BRANCH),
    }
    p.text("\n");
    let unborn = info.head.is_none();
    if !unborn {
        let push_shown = crate::compare_branches::has_push(&info.compare);
        for item in &info.compare {
            match item {
                crate::compare_branches::Item::Upstream => {
                    if let Some((up, state)) = &info.upstream {
                        tracking(p, up, *state, ctx.hints, push_shown);
                        p.text("\n");
                    }
                }
                crate::compare_branches::Item::PushMissing => {}
                crate::compare_branches::Item::Push(push) => {
                    for (text, advice) in crate::compare_branches::push_lines(push) {
                        if advice {
                            hint(p, &text);
                        } else {
                            p.text(&text);
                            p.text("\n");
                        }
                    }
                    p.text("\n");
                }
            }
        }
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
    let changed: Vec<(&Vec<u8>, u8, Option<crate::submodules::WtState>)> = entries
        .iter()
        .filter(|(_, c)| conflict_code(c).is_none() && c.wt != 0 && c.wt != b' ')
        .map(|(path, c)| (*path, c.wt, c.sub))
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
    let picking = ctx
        .sequencing
        .as_ref()
        .is_some_and(|s| s.commit.is_some() && !s.revert);
    if let Some(empty) = ctx.am {
        for l in crate::wt_status::am_lines(empty, ctx.hints) {
            p.text(&l);
            p.text("\n");
        }
        p.text("\n");
    }
    if let Some(seq) = &ctx.sequencing {
        for l in crate::wt_status::sequencing_lines(seq, !unmerged.is_empty(), ctx.hints) {
            p.text(&l);
            p.text("\n");
        }
        p.text("\n");
    }
    if let Some(rb) = &ctx.rebasing {
        let split = !changed.is_empty();
        for l in crate::wt_status::rebasing_lines(rb, !unmerged.is_empty(), split, ctx.hints) {
            p.text(&l);
            p.text("\n");
        }
        p.text("\n");
    }
    if let Some(start) = &ctx.bisecting {
        for l in crate::wt_status::bisecting_lines(start, ctx.hints) {
            p.text(&l);
            p.text("\n");
        }
        p.text("\n");
    }
    if let Some(line) = &ctx.sparse_percent {
        p.text(line);
        p.text("\n\n");
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
        } else if unmerged.iter().all(|(_, c)| *c == "DD") {
            hint(p, msg::HINT_MARK_RESOLUTION_RM_ONLY);
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
        if changed.iter().any(|(_, y, _)| *y == b'D') {
            hint(p, msg::HINT_ADD_RM);
        } else {
            hint(p, msg::HINT_ADD);
        }
        hint(p, msg::HINT_RESTORE);
        if changed.iter().any(|(_, _, s)| s.is_some_and(|s| s.dirty())) {
            hint(p, crate::messages::submodule::HINT_SUBMODULE_CONTENT);
        }
        for (path, y, sub) in &changed {
            p.text(&format!("\t{:<12}", label_of(*y)));
            p.path(path);
            if let (b'M', Some(st)) = (*y, sub) {
                use crate::messages::submodule as sm;
                let parts: Vec<&str> = [
                    (st.new_commits, sm::NEW_COMMITS),
                    (st.modified, sm::MODIFIED_CONTENT),
                    (st.untracked, sm::UNTRACKED_CONTENT),
                ]
                .iter()
                .filter(|(on, _)| *on)
                .map(|(_, t)| *t)
                .collect();
                if !parts.is_empty() {
                    p.text(&format!(" ({})", parts.join(", ")));
                }
            }
            p.text("\n");
        }
        p.text("\n");
    }
    for (header, text) in [
        (msg::SUBMODULE_SUMMARY_STAGED, &ctx.summary.0),
        (msg::SUBMODULE_SUMMARY_UNSTAGED, &ctx.summary.1),
    ] {
        if !text.is_empty() {
            p.text(header);
            p.text("\n\n");
            p.out.extend_from_slice(text);
        }
    }
    if !untracked.is_empty() {
        p.text(msg::UNTRACKED_HEADER);
        p.text("\n");
        hint(p, msg::HINT_INCLUDE);
        match ctx.columns {
            Some(mode) => {
                // The quoted paths as cells, laid out after a tab.
                let mut cells = Vec::new();
                for u in untracked {
                    let at = p.out.len();
                    p.path(u);
                    cells.push(String::from_utf8_lossy(&p.out[at..]).into_owned());
                    p.out.truncate(at);
                }
                let mut opts = crate::column::Options::new(mode);
                opts.indent = "\t".into();
                p.text(&crate::column::format(&cells, &opts));
            }
            None => {
                for u in untracked {
                    p.text("\t");
                    p.path(u);
                    p.text("\n");
                }
            }
        }
        p.text("\n");
    }
    if let Some(text) = &ctx.uf_delay {
        p.text("\n");
        p.text(text);
        p.text("\n\n");
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
            return None;
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
    let at = p.out.len();
    p.text(text);
    if ctx.hints {
        p.text(hint_text);
    }
    p.text("\n");
    Some(at)
}

/// The submodule summary of the long format (`status.submoduleSummary`):
/// staged gitlink changes, then submodules whose `HEAD` moved; nothing for
/// submodules with `ignore=all`.
fn submodule_summary(
    repo: &Repo,
    entries: &[(&Vec<u8>, &Change)],
    rules: Option<&crate::submodules::IgnoreRules>,
) -> (Vec<u8>, Vec<u8>) {
    let (mut staged, mut unstaged) = (Vec::new(), Vec::new());
    let (Some(limit), Some(rules)) = (crate::submodules::summary_limit(&repo.config), rules) else {
        return (staged, unstaged);
    };
    let gitlink = |mode: u32| mode == 0o160000;
    for (path, c) in entries {
        if rules.for_path(path) == rgit_submodule::Ignore::All || conflict_code(c).is_some() {
            continue;
        }
        let head = c.head.filter(|(m, _)| gitlink(*m)).map(|(_, id)| id);
        let index = c.index.filter(|(m, ..)| gitlink(*m)).map(|(_, id, _)| id);
        if (head.is_some() || index.is_some()) && head != index {
            crate::submodules::status_summary(&mut staged, repo, path, head, index, limit);
        }
        if let (Some(i), Some(h)) = (index, c.sub.and_then(|s| s.head)) {
            if i != h {
                crate::submodules::status_summary(&mut unstaged, repo, path, Some(i), Some(h), limit);
            }
        }
    }
    (staged, unstaged)
}

/// The upstream lines of the long format.
/// With `push_shown`, the advice about pushing and `--ahead-behind` goes
/// with the push comparison instead.
fn tracking(p: &mut Printer, up: &str, state: Tracking, hints: bool, push_shown: bool) {
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
            if !push_shown {
                hint(p, msg::HINT_PUSH);
            }
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
            if !push_shown {
                hint(p, msg::HINT_AHEAD_BEHIND);
            }
        }
    }
}

/// The work tree side of the stage-0 entry `e`. Submodules are inspected
/// with `subs` (their `ignore` setting, untracked files); without it only
/// a different `HEAD` counts.
pub(crate) fn check_worktree(
    repo: &Repo,
    checker: &Checker,
    work_tree: &Path,
    e: &Entry,
    c: &mut Change,
    subs: Option<&SubContext<'_>>,
) -> Result<(), Die> {
    let meta = checker.lstat(e.path()).ok().flatten();
    let meta = meta.as_ref().map(FileStat::from_metadata);
    check_worktree_with(repo, work_tree, e, c, subs, meta.as_ref(), || checker.check(e))
}

/// [`check_worktree`] with the `lstat` data `meta` of the file and its
/// state (`status`, asked for regular files only).
fn check_worktree_with(
    repo: &Repo,
    work_tree: &Path,
    e: &Entry,
    c: &mut Change,
    subs: Option<&SubContext<'_>>,
    meta: Option<&FileStat>,
    status: impl FnOnce() -> std::io::Result<Status>,
) -> Result<(), Die> {
    // Outside a sparse checkout the file is missing on purpose: it counts
    // as unchanged.
    if e.flags.skip_worktree && e.stage() == rgit_index::Stage::MERGED {
        c.wt_mode = Some(e.mode.0);
        return Ok(());
    }
    let Some(meta) = meta else {
        c.wt = b'D';
        c.wt_mode = None;
        return Ok(());
    };
    c.wt_mode = Some(wt_mode(meta, e.mode.0));
    if e.flags.intent_to_add {
        c.wt = b'A';
        return Ok(());
    }
    if e.mode.is_gitlink() {
        let dir = crate::diff::collect::os_path(work_tree, e.path());
        match subs {
            Some(s) => {
                let ignore = s.rules.for_path(e.path());
                let st = crate::submodules::worktree_state(&dir, &e.id, repo.hash, ignore, s.untracked);
                if st.changed() {
                    c.wt = b'M';
                }
                c.sub = Some(st);
            }
            None => {
                if repository::submodule_head(&dir, repo.hash).is_some_and(|h| h != e.id) {
                    c.wt = b'M';
                }
            }
        }
        return Ok(());
    }
    c.wt = match status() {
        Ok(Status::Unchanged | Status::StatOnly) => b' ',
        Ok(Status::TypeChanged) => b'T',
        Ok(Status::Deleted) => {
            c.wt_mode = None;
            b'D'
        }
        _ => b'M',
    };
    Ok(())
}

fn wt_mode(meta: &FileStat, index_mode: u32) -> u32 {
    if meta.is_symlink() {
        0o120000
    } else if meta.is_dir() {
        if type_bits(index_mode) == 0o160000 {
            0o160000
        } else {
            0o040000
        }
    } else if meta.executable {
        0o100755
    } else {
        0o100644
    }
}


/// The `-b` header of the short format and the porcelain formats.
pub(crate) fn header(p: &mut Printer, version: u8, info: &BranchInfo) {
    if version == 1 {
        let c = p.colors.clone().unwrap_or_default();
        let out = &mut p.out;
        paint(out, &c.header, b"## ");
        match (&info.branch, info.head) {
            (Some(b), None) => {
                paint(out, &c.header, b"No commits yet on ");
                paint(out, &c.local, b.as_bytes());
            }
            (Some(b), Some(_)) => paint(out, &c.local, b.as_bytes()),
            (None, _) => paint(out, &c.nobranch, b"HEAD (no branch)"),
        }
        if let Some((up, state)) = &info.upstream {
            paint(out, &c.header, b"...");
            paint(out, &c.remote, up.as_bytes());
            let (ahead, behind) = match *state {
                Tracking::Gone => {
                    paint(out, &c.header, b" [gone]");
                    (None, None)
                }
                Tracking::Different => {
                    paint(out, &c.header, b" [different]");
                    (None, None)
                }
                Tracking::Counts(0, 0) => (None, None),
                Tracking::Counts(a, 0) => (Some(a), None),
                Tracking::Counts(0, b) => (None, Some(b)),
                Tracking::Counts(a, b) => (Some(a), Some(b)),
            };
            if ahead.is_some() || behind.is_some() {
                paint(out, &c.header, b" [");
                if let Some(a) = ahead {
                    paint(out, &c.header, b"ahead ");
                    paint(out, &c.local, a.to_string().as_bytes());
                }
                if ahead.is_some() && behind.is_some() {
                    paint(out, &c.header, b", ");
                }
                if let Some(b) = behind {
                    paint(out, &c.header, b"behind ");
                    paint(out, &c.remote, b.to_string().as_bytes());
                }
                paint(out, &c.header, b"]");
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
