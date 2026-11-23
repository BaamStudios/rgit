// SPDX-License-Identifier: MIT
//! The options of `rev-list` and `log` (Documentation/rev-list-options.adoc)
//! and the revision arguments, from the command line and from `--stdin`.

use std::io::Write;

use rgit_config::ConfigSet;
use rgit_config::color::ColorBool;
use rgit_date::{DateContext, DateMode};
use rgit_revwalk::walk::grep::{GrepOptions, Syntax};
use rgit_revwalk::walk::{NoWalk, Order, WalkOptions};

use super::Command;
use super::decorate::Filter;
use super::pretty::{Pretty, PrettyError};
use crate::Die;
use crate::messages::rev_list as msg;

/// The shortest `--abbrev` (smaller values are raised to it).
const MIN_ABBREV: usize = 4;

/// `--abbrev` settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Abbrev {
    /// `core.abbrev` or the automatic length.
    Default,
    /// `--abbrev=<n>`.
    Digits(usize),
    /// `--no-abbrev`.
    Full,
}

/// The default `--show-linear-break` barrier.
const LINEAR_BREAK: &str = "                    ..........";

/// A set of references (`--all`, `--branches[=<pattern>]`, `--glob=...`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefSet {
    /// `--all`: every ref, `HEAD` and the other work trees' `HEAD`s.
    All,
    /// `--branches`, `--tags`, `--remotes` with an optional pattern below
    /// the prefix.
    Prefix {
        prefix: &'static str,
        pattern: Option<String>,
    },
    /// `--glob=<pattern>` (already starting with `refs/`).
    Glob(String),
    /// `--reflog`: every object the reflogs mention.
    Reflog,
    /// `--indexed-objects`: the trees and blobs of the index.
    Indexed,
    /// `--alternate-refs`: the ref tips of the alternate repositories.
    AlternateRefs,
}

/// A starting point as given on the command line or on standard input.
#[derive(Debug, Clone)]
pub enum Tip {
    /// A revision argument (`A`, `^A`, `A..B`, `A...B`, `A^!`, ...);
    /// `negate` is set after an odd number of `--not`.
    Rev { arg: String, negate: bool },
    /// A set of refs, minus the `--exclude` patterns given before it.
    Refs {
        set: RefSet,
        negative: bool,
        exclude: Vec<String>,
        /// `--exclude-hidden=<section>` given before it.
        hidden: Option<String>,
    },
}

/// `--objects` and its variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectsMode {
    /// `--objects-edge`: print the excluded parents (`-<id>`) first.
    pub edge: bool,
    /// `--in-commit-order`: list each commit's objects after it.
    pub in_commit_order: bool,
}

/// How merges get a diff (`--diff-merges`, `-m`, `-c`, `--cc`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeDiff {
    /// No diff (the default of `log`).
    Off,
    /// Against the first parent.
    FirstParent,
    /// Against every parent (`-m`).
    Separate,
    /// Combined (`-c`).
    Combined,
    /// Dense combined (`--cc`, the default of `show`).
    Dense,
    /// Against the tree of merging the parents again (`--remerge-diff`).
    Remerge,
}

/// Which commits a merge-diff option gives a patch when no diff format
/// was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MergesPatch {
    /// None (`-m`, `--no-diff-merges`).
    #[default]
    None,
    /// Merges only (`--diff-merges=<format>`).
    Merges,
    /// Every commit (`-c`, `--cc`, `--dd`, `--remerge-diff`).
    All,
}

/// Parsed options.
#[derive(Debug)]
pub struct Options {
    pub walk: WalkOptions,
    pub tips: Vec<Tip>,
    pub count: bool,
    /// `--left-right`.
    pub left_right: bool,
    pub parents: bool,
    pub children: bool,
    pub timestamp: bool,
    pub header: bool,
    pub quiet: bool,
    pub objects: Option<ObjectsMode>,
    /// `--[no-]object-names`.
    pub object_names: bool,
    pub stdin: bool,
    /// Where the revisions from `--stdin` go among the others.
    stdin_at: usize,
    pub grep: GrepOptions,
    /// `--ancestry-path=<commit>` arguments.
    pub ancestry: Vec<String>,
    pub abbrev_commit: bool,
    /// `--[no-]expand-tabs[=<n>]`.
    pub tab_width: Option<usize>,
    /// `--[no-]abbrev-commit` was given (else `log.abbrevCommit`).
    abbrev_commit_given: bool,
    pub abbrev: Abbrev,
    /// `--pretty`/`--format`/`--oneline` as given (resolved with the
    /// configuration into `pretty`).
    pub pretty_arg: Option<String>,
    pub pretty: Option<Pretty>,
    pub date_mode: DateMode,
    /// `--date` was given.
    pub date_given: bool,
    /// `--encoding=<encoding>`: the output encoding of commit messages.
    pub encoding: Option<String>,
    /// `-E`, `-F`, `-P` or `--basic-regexp` was given (else
    /// `grep.patternType` decides).
    pub syntax_given: bool,
    /// `--decorate[=<how>]` / `--no-decorate`.
    pub decorate: Option<Decorate>,
    /// `--decorate-refs*` and `--clear-decorations`.
    pub decorate_filter: Filter,
    /// `--no-notes` turns `%N` off.
    pub notes: bool,
    /// `--[no-]show-signature`.
    pub show_signature: Option<bool>,
    /// `-z` (log).
    pub zero: bool,
    /// `--[no-]commit-header` (rev-list with a user format).
    pub commit_header: bool,
    /// `--[no-]mailmap`; `None` = the command's default.
    pub mailmap: Option<bool>,
    /// `--color[=<when>]` / `--no-color` (only `log` colours).
    pub color: Option<ColorBool>,
    /// `--in-commit-order` (takes effect with `--objects`).
    in_commit_order: bool,
    /// `--graph`.
    pub graph: bool,
    /// `--[no-]graph-indent`; `None`: `log.graphIndent`.
    pub graph_indent: Option<bool>,
    /// `--graph-lane-limit=<n>` (0: no limit).
    pub graph_lane_limit: usize,
    /// An ordering option was given (`--graph` implies `--topo-order`).
    order_given: bool,
    /// Diff options (`log -p`, `--stat`, ...).
    pub diff: crate::diff::options::DiffOpts,
    /// `--diff-merges` and friends, when given.
    pub merges: Option<MergeDiff>,
    /// The merge format came from `-m` or `--diff-merges=on` (it is
    /// `log.diffMerges`).
    merges_on: bool,
    /// The patch the merge-diff options imply.
    pub merges_patch: MergesPatch,
    /// `--combined-all-paths`.
    pub combined_all_paths: bool,
    /// `git show`: objects one by one, with diffs by default.
    pub show: bool,
    /// `diff-tree` with a pretty format: plumbing diffs (full raw ids,
    /// recursive only when asked for).
    pub plumbing: bool,
    /// `-g`, `--walk-reflogs`: walk the reflogs of the tips instead of
    /// the history.
    pub walk_reflogs: bool,
    /// `--show-linear-break[=<barrier>]`.
    pub linear_break: Option<String>,
    /// `--source`: show the revision each commit was reached from.
    pub source: bool,
    /// `--root`: root commits get a diff (else `log.showRoot`).
    pub root: bool,
    /// `--max-count-oldest`: the count limits the last commits.
    pub max_count_oldest: bool,
    /// `--max-count` was given (it conflicts with `--max-count-oldest`).
    max_count_given: bool,
    /// `--bisect`: add `refs/bisect/bad` and `^refs/bisect/good-*` (and,
    /// for rev-list, show only the midpoint).
    pub bisect: bool,
    /// `--bisect-vars` (rev-list).
    pub bisect_vars: bool,
    /// `--bisect-all` (rev-list).
    pub bisect_all: bool,
    /// State while parsing: an odd number of `--not` so far.
    negate: bool,
    /// State while parsing: `--exclude` patterns for the next ref set.
    exclude: Vec<String>,
    /// The paths after `--` (and after `--` on standard input).
    pub paths: Vec<String>,
    /// `--merge`: `HEAD...<other>` limited to the conflicted paths.
    pub merge: bool,
    /// `--` was given: the arguments before it are revisions.
    pub dashdash: bool,
    /// The only tip is the implicit `HEAD` of a command without revisions;
    /// a file named `HEAD` does not make it ambiguous.
    pub default_head: bool,
    /// `--simplify-by-decoration`.
    pub simplify_by_decoration: bool,
    /// `--full-diff`: the diffs of path-limited commits show every path.
    pub full_diff: bool,
    /// `--follow` / `--no-follow` (log); `None`: `log.follow`.
    pub follow: Option<bool>,
    /// State while parsing: `--exclude-hidden=<section>` for the next
    /// `--all` or `--glob`.
    hidden: Option<String>,
    /// `--single-worktree`: `--all`, `--reflog` and `--indexed-objects`
    /// look at the current work tree only.
    pub single_worktree: bool,
    /// `--ignore-missing`: revisions that name nothing are left out.
    pub ignore_missing: bool,
    /// `--unpacked`: leave out objects (and commits) found in packs.
    pub unpacked: bool,
    /// `--disk-usage[=human]`: `Some(human)`.
    pub disk_usage: Option<bool>,
    /// `--maximal-only`.
    pub maximal_only: bool,
}

fn parse_count(s: &str) -> Result<usize, Die> {
    match s.parse::<i64>() {
        Ok(n) => Ok(usize::try_from(n).unwrap_or(usize::MAX)),
        Err(_) => Err(Die::Fatal(msg::not_an_integer(s))),
    }
}

/// A whole number for `--min-parents=` and `--max-parents=`.
fn parse_int(s: &str) -> Result<i64, Die> {
    s.parse::<i64>()
        .map_err(|_| Die::Fatal(msg::not_an_integer(s)))
}

/// Seconds since the epoch for `--max-age=` and `--min-age=`.
fn parse_epoch(s: &str) -> Result<i64, Die> {
    s.parse::<i64>()
        .map_err(|_| Die::Fatal(msg::not_epoch_seconds(s)))
}

fn usage(cmd: Command, arg: &str) -> Die {
    match cmd {
        Command::RevList => {
            let _ = std::io::stderr().write_all(msg::REV_LIST_USAGE.as_bytes());
            Die::Exit(rgit_options::EXIT_USAGE)
        }
        Command::Log => Die::Fatal(msg::unrecognized_argument(arg)),
    }
}

/// `--decorate[=<how>]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decorate {
    No,
    Short,
    Full,
    /// Decorate when standard output is a terminal.
    Auto,
}

/// The format `value` selects, with `pretty.<name>` aliases from `config`.
fn parse_pretty(value: &str, config: &ConfigSet) -> Result<Pretty, Die> {
    let alias = |name: &str| -> Option<String> {
        let v = config.string(format!("pretty.{name}")).ok()??;
        Some(v.to_string())
    };
    Pretty::parse(value, &alias).map_err(|e| match e {
        PrettyError::Invalid => Die::Fatal(msg::invalid_pretty(value)),
        PrettyError::Loop => Die::Fatal(msg::pretty_alias_loop(value)),
        PrettyError::Unsupported(name) => {
            Die::Fatal(msg::not_supported(&format!("--pretty={name}")))
        }
    })
}

fn date(v: &str) -> i64 {
    let t = rgit_date::approxidate(v, &DateContext::from_env());
    i64::try_from(t).unwrap_or(i64::MAX)
}

/// What a pseudo-option did.
enum Pseudo {
    /// It was one; consumed this many extra arguments.
    Done(usize),
    /// Not a pseudo-option.
    No,
    /// A pseudo-option with an invalid value (the error is printed).
    Invalid,
}

impl Options {
    fn new() -> Options {
        Options {
            walk: WalkOptions::default(),
            tips: Vec::new(),
            count: false,
            left_right: false,
            parents: false,
            children: false,
            timestamp: false,
            header: false,
            quiet: false,
            objects: None,
            object_names: true,
            stdin: false,
            stdin_at: 0,
            grep: GrepOptions::default(),
            ancestry: Vec::new(),
            abbrev_commit: false,
            tab_width: None,
            abbrev_commit_given: false,
            abbrev: Abbrev::Default,
            pretty_arg: None,
            pretty: None,
            date_mode: DateMode::default(),
            date_given: false,
            encoding: None,
            syntax_given: false,
            decorate: None,
            decorate_filter: Filter::default(),
            zero: false,
            notes: true,
            show_signature: None,
            commit_header: true,
            mailmap: None,
            color: None,
            in_commit_order: false,
            graph: false,
            graph_indent: None,
            graph_lane_limit: 0,
            order_given: false,
            diff: crate::diff::options::DiffOpts::default(),
            merges: None,
            merges_on: false,
            merges_patch: MergesPatch::None,
            combined_all_paths: false,
            show: false,
            plumbing: false,
            walk_reflogs: false,
            linear_break: None,
            source: false,
            root: false,
            max_count_oldest: false,
            max_count_given: false,
            bisect: false,
            merge: false,
            bisect_vars: false,
            bisect_all: false,
            negate: false,
            exclude: Vec::new(),
            paths: Vec::new(),
            dashdash: false,
            default_head: false,
            simplify_by_decoration: false,
            full_diff: false,
            follow: None,
            hidden: None,
            single_worktree: false,
            ignore_missing: false,
            unpacked: false,
            disk_usage: None,
            maximal_only: false,
        }
    }

    fn push_refs(&mut self, set: RefSet) {
        // `--exclude` patterns apply to the ref sets, hidden refs only to
        // `--all` and `--glob`.
        let (exclude, hidden) = match set {
            RefSet::Reflog | RefSet::Indexed | RefSet::AlternateRefs => (Vec::new(), None),
            _ => (
                std::mem::take(&mut self.exclude),
                if matches!(set, RefSet::All | RefSet::Glob(_)) {
                    self.hidden.take()
                } else {
                    None
                },
            ),
        };
        self.tips.push(Tip::Refs {
            set,
            negative: self.negate,
            exclude,
            hidden,
        });
    }

    /// The options that stand for revisions and may also come from
    /// `--stdin`: `--all`, `--branches[=]`, `--tags[=]`, `--remotes[=]`,
    /// `--glob=`, `--exclude=`, `--not`, `--no-walk[=]`, `--do-walk`.
    fn pseudo(&mut self, a: &str, next: Option<&str>) -> Result<Pseudo, Die> {
        let value = |name: &str| -> Result<(String, usize), Die> {
            if let Some(v) = a.strip_prefix(name).and_then(|r| r.strip_prefix('=')) {
                return Ok((v.to_string(), 0));
            }
            next.map(|v| (v.to_string(), 1))
                .ok_or_else(|| Die::Fatal(msg::requires_value(name)))
        };
        let pattern = |rest: Option<&str>| rest.map(str::to_string);
        match a {
            "--all" => self.push_refs(RefSet::All),
            "--reflog" => self.push_refs(RefSet::Reflog),
            "--indexed-objects" => self.push_refs(RefSet::Indexed),
            "--alternate-refs" => self.push_refs(RefSet::AlternateRefs),
            "--branches" | "--tags" | "--remotes" => {
                self.check_hidden(a)?;
                let prefix = ref_prefix(a);
                self.push_refs(RefSet::Prefix {
                    prefix,
                    pattern: None,
                });
            }
            "--not" => self.negate = !self.negate,
            "--do-walk" => self.walk.no_walk = None,
            "--no-walk" => self.walk.no_walk = Some(NoWalk::Sorted),
            _ if a == "--glob" || a.starts_with("--glob=") => {
                let (v, used) = value("--glob")?;
                let full = if v.starts_with("refs/") {
                    v
                } else {
                    format!("refs/{v}")
                };
                self.push_refs(RefSet::Glob(full));
                return Ok(Pseudo::Done(used));
            }
            _ if a.starts_with("--exclude-hidden=") => {
                let section = &a["--exclude-hidden=".len()..];
                if !matches!(section, "fetch" | "receive" | "uploadpack") {
                    return Err(Die::Fatal(msg::unsupported_hidden_section(section)));
                }
                if self.hidden.is_some() {
                    return Err(Die::Fatal(msg::EXCLUDE_HIDDEN_TWICE.to_string()));
                }
                self.hidden = Some(section.to_string());
            }
            _ if a == "--exclude" || a.starts_with("--exclude=") => {
                let (v, used) = value("--exclude")?;
                self.exclude.push(v);
                return Ok(Pseudo::Done(used));
            }
            _ => {
                for name in ["--branches", "--tags", "--remotes"] {
                    if let Some(p) = a.strip_prefix(name).and_then(|r| r.strip_prefix('=')) {
                        self.check_hidden(name)?;
                        self.push_refs(RefSet::Prefix {
                            prefix: ref_prefix(name),
                            pattern: pattern(Some(p)),
                        });
                        return Ok(Pseudo::Done(0));
                    }
                }
                if let Some(v) = a.strip_prefix("--no-walk=") {
                    self.walk.no_walk = Some(match v {
                        "sorted" => NoWalk::Sorted,
                        "unsorted" => NoWalk::Unsorted,
                        _ => {
                            let _ = writeln!(std::io::stderr(), "error: {}", msg::INVALID_NO_WALK);
                            return Ok(Pseudo::Invalid);
                        }
                    });
                    return Ok(Pseudo::Done(0));
                }
                return Ok(Pseudo::No);
            }
        }
        Ok(Pseudo::Done(0))
    }

    /// `--exclude-hidden` does not combine with `--branches`, `--tags`
    /// and `--remotes`.
    fn check_hidden(&self, option: &str) -> Result<(), Die> {
        if self.hidden.is_some() {
            let _ = writeln!(
                std::io::stderr(),
                "error: {}",
                msg::cannot_be_used_together("--exclude-hidden", option)
            );
            let _ = std::io::stderr().write_all(msg::REV_LIST_USAGE.as_bytes());
            return Err(Die::Exit(rgit_options::EXIT_USAGE));
        }
        Ok(())
    }

    /// Parse `argv` (without the command name).
    pub fn parse(cmd: Command, argv: &[String]) -> Result<Options, Die> {
        let mut o = Options::new();
        let mut i = 0;
        let mut end_of_options = false;
        let value = |i: &mut usize, name: &str| -> Result<String, Die> {
            *i += 1;
            argv.get(*i)
                .cloned()
                .ok_or_else(|| Die::Fatal(format!("{name} requires an argument")))
        };
        // `--name=<v>` or `--name <v>`.
        let long = |a: &str, name: &str, i: &mut usize| -> Result<Option<String>, Die> {
            if a == name {
                return value(i, name).map(Some);
            }
            Ok(a.strip_prefix(name)
                .and_then(|r| r.strip_prefix('='))
                .map(str::to_string))
        };
        while i < argv.len() {
            let a = argv[i].as_str();
            if a == "--" {
                o.dashdash = true;
                o.paths.extend(argv[i + 1..].iter().cloned());
                break;
            }
            if end_of_options || !a.starts_with('-') || a == "-" {
                o.tips.push(Tip::Rev {
                    arg: a.to_string(),
                    negate: o.negate,
                });
                i += 1;
                continue;
            }
            if a == "--end-of-options" {
                end_of_options = true;
                i += 1;
                continue;
            }
            match o.pseudo(a, argv.get(i + 1).map(String::as_str))? {
                Pseudo::Done(used) => {
                    i += 1 + used;
                    continue;
                }
                Pseudo::Invalid => return Err(usage(cmd, a)),
                Pseudo::No => {}
            }
            match a {
                "--topo-order" => {
                    o.walk.order = Order::Topo;
                    o.order_given = true;
                }
                "--date-order" => {
                    o.walk.order = Order::Date;
                    o.order_given = true;
                }
                "--author-date-order" => {
                    o.walk.order = Order::AuthorDate;
                    o.order_given = true;
                }
                "--graph" => o.graph = true,
                "-g" | "--walk-reflogs" => o.walk_reflogs = true,
                "--root" if cmd == Command::Log => o.root = true,
                "--no-graph" => o.graph = false,
                "--expand-tabs" => o.tab_width = Some(8),
                "--no-expand-tabs" => o.tab_width = Some(0),
                "--source" if cmd == Command::Log => o.source = true,
                "--show-linear-break" => o.linear_break = Some(LINEAR_BREAK.to_string()),
                "--graph-indent" => o.graph_indent = Some(true),
                "--no-graph-indent" => o.graph_indent = Some(false),
                "--reverse" => o.walk.reverse = true,
                "--first-parent" => o.walk.first_parent = true,
                "--exclude-first-parent-only" => o.walk.exclude_first_parent_only = true,
                "--parents" => o.parents = true,
                "--children" => o.children = true,
                "--timestamp" if cmd == Command::RevList => o.timestamp = true,
                "--header" if cmd == Command::RevList => o.header = true,
                "--quiet" if cmd == Command::RevList => o.quiet = true,
                "--count" if cmd == Command::RevList => o.count = true,
                "--objects" if cmd == Command::RevList => o.set_objects(false),
                "--objects-edge" | "--objects-edge-aggressive" if cmd == Command::RevList => {
                    o.set_objects(true);
                }
                "--in-commit-order" if cmd == Command::RevList => o.in_commit_order = true,
                "--single-worktree" => o.single_worktree = true,
                "--ignore-missing" => o.ignore_missing = true,
                "--unpacked" => o.unpacked = true,
                "--maximal-only" => o.maximal_only = true,
                "--disk-usage" if cmd == Command::RevList => o.disk_usage = Some(false),
                "--bisect" => o.bisect = true,
                "--merge" => o.merge = true,
                "--bisect-vars" if cmd == Command::RevList => o.bisect_vars = true,
                "--bisect-all" if cmd == Command::RevList => o.bisect_all = true,
                "--object-names" if cmd == Command::RevList => o.object_names = true,
                "--no-object-names" if cmd == Command::RevList => o.object_names = false,
                "--stdin" => {
                    o.stdin = true;
                    o.stdin_at = o.tips.len();
                }
                "--boundary" => o.walk.boundary = true,
                "--left-right" => o.left_right = true,
                "--left-only" => {
                    if o.walk.right_only {
                        return Err(Die::Fatal(msg::cannot_be_used_together(
                            "--left-only",
                            "--right-only/--cherry",
                        )));
                    }
                    o.walk.left_only = true;
                }
                "--right-only" => {
                    if o.walk.left_only {
                        return Err(Die::Fatal(msg::cannot_be_used_together(
                            "--right-only",
                            "--left-only",
                        )));
                    }
                    o.walk.right_only = true;
                }
                "--ancestry-path" => o.walk.ancestry_path = true,
                "--full-history" => o.walk.full_history = true,
                "--dense" => o.walk.sparse = false,
                "--sparse" => o.walk.sparse = true,
                "--simplify-merges" => o.walk.simplify_merges = true,
                "--show-pulls" => o.walk.show_pulls = true,
                "--remove-empty" => o.walk.remove_empty = true,
                "--full-diff" if cmd == Command::Log => o.full_diff = true,
                "--simplify-by-decoration" => {
                    // It works like `--simplify-merges` with decorations.
                    o.simplify_by_decoration = true;
                    o.walk.simplify_merges = true;
                }
                "--follow" if cmd == Command::Log => o.follow = Some(true),
                "--no-follow" if cmd == Command::Log => o.follow = Some(false),
                "--merges" => o.walk.min_parents = Some(2),
                "--no-merges" => o.walk.max_parents = Some(1),
                "--no-min-parents" => o.walk.min_parents = None,
                "--no-max-parents" => o.walk.max_parents = None,
                "-i" | "--regexp-ignore-case" => {
                    o.grep.ignore_case = true;
                    o.diff.pickaxe_icase = true;
                }
                "-E" | "--extended-regexp" => {
                    o.grep.syntax = Syntax::Extended;
                    o.syntax_given = true;
                }
                "-F" | "--fixed-strings" => {
                    o.grep.syntax = Syntax::Fixed;
                    o.syntax_given = true;
                }
                "-P" | "--perl-regexp" => {
                    o.grep.syntax = Syntax::Perl;
                    o.syntax_given = true;
                }
                "--basic-regexp" => {
                    o.grep.syntax = Syntax::Basic;
                    o.syntax_given = true;
                }
                "--all-match" => o.grep.all_match = true,
                "--invert-grep" => o.grep.invert = true,
                "--cherry-mark" => {
                    if o.walk.cherry_pick {
                        return Err(Die::Fatal(msg::cannot_be_used_together(
                            "--cherry-mark",
                            "--cherry-pick",
                        )));
                    }
                    o.walk.cherry_mark = true;
                }
                // `--right-only --cherry-mark --no-merges`.
                "--cherry" => {
                    if o.walk.left_only {
                        return Err(Die::Fatal(msg::cannot_be_used_together(
                            "--cherry",
                            "--left-only",
                        )));
                    }
                    o.walk.right_only = true;
                    o.walk.cherry_mark = true;
                    o.walk.cherry_pick = false;
                    o.walk.max_parents = Some(1);
                }
                "--cherry-pick" => {
                    if o.walk.cherry_mark {
                        return Err(Die::Fatal(msg::cannot_be_used_together(
                            "--cherry-pick",
                            "--cherry-mark",
                        )));
                    }
                    o.walk.cherry_pick = true;
                }
                "--abbrev-commit" | "--no-abbrev-commit" => {
                    o.abbrev_commit = a == "--abbrev-commit";
                    o.abbrev_commit_given = true;
                }
                "--abbrev" => o.abbrev = Abbrev::Default,
                "--no-abbrev" => o.abbrev = Abbrev::Full,
                "--oneline" => {
                    o.pretty_arg = Some("oneline".into());
                    o.abbrev_commit = true;
                }
                "--pretty" => o.pretty_arg = Some("medium".into()),
                "--show-signature" => o.show_signature = Some(true),
                "--no-show-signature" => o.show_signature = Some(false),
                "--no-notes" => o.notes = false,
                "--no-decorate" => o.decorate = Some(Decorate::No),
                "--decorate" => o.decorate = Some(Decorate::Short),
                "--clear-decorations" => {
                    o.decorate_filter.include.clear();
                    o.decorate_filter.exclude.clear();
                    o.decorate_filter.clear = true;
                }
                "-z" => o.zero = true,
                "--no-commit-header" if cmd == Command::RevList => o.commit_header = false,
                "--commit-header" if cmd == Command::RevList => o.commit_header = true,
                "--mailmap" | "--use-mailmap" => o.mailmap = Some(true),
                "--no-mailmap" | "--no-use-mailmap" => o.mailmap = Some(false),
                "-n" | "--max-count" => {
                    o.walk.max_count = Some(parse_count(&value(&mut i, a)?)?);
                    o.max_count_given |= a == "--max-count";
                }
                "--skip" => o.walk.skip = parse_count(&value(&mut i, a)?)?,
                // `-S <string>`, `-G <regex>`: the value may be separate.
                "-S" | "-G" if cmd == Command::Log => {
                    i += 1;
                    let Some(v) = argv.get(i) else {
                        return Err(crate::diff::options::usage_exit(
                            crate::messages::diff::switch_requires_value(&a[1..]),
                        ));
                    };
                    o.diff.parse_arg(&format!("{a}{v}"))?;
                }
                _ if crate::color::parse_arg(a).is_some() => {
                    if let Some(c) = crate::color::parse_arg(a) {
                        o.color = Some(c?);
                    }
                }
                _ => {
                    if let Some(v) = long(a, "--since-as-filter", &mut i)? {
                        o.walk.max_age_filter = Some(date(&v));
                    } else if let Some(v) =
                        long(a, "--since", &mut i)?.or(long(a, "--after", &mut i)?)
                    {
                        o.walk.max_age = Some(date(&v));
                    } else if let Some(v) =
                        long(a, "--until", &mut i)?.or(long(a, "--before", &mut i)?)
                    {
                        o.walk.min_age = Some(date(&v));
                    } else if let Some(v) = a.strip_prefix("--max-age=") {
                        o.walk.max_age = Some(parse_epoch(v)?);
                    } else if let Some(v) = a.strip_prefix("--min-age=") {
                        o.walk.min_age = Some(parse_epoch(v)?);
                    } else if let Some(v) = a.strip_prefix("--min-parents=") {
                        let n = parse_int(v)?;
                        o.walk.min_parents = usize::try_from(n).ok().filter(|&n| n > 0);
                    } else if let Some(v) = a.strip_prefix("--max-parents=") {
                        let n = parse_int(v)?;
                        o.walk.max_parents = usize::try_from(n).ok();
                    } else if let Some(v) = long(a, "--author", &mut i)? {
                        o.grep.author.push(v);
                    } else if let Some(v) = long(a, "--committer", &mut i)? {
                        o.grep.committer.push(v);
                    } else if let Some(v) = long(a, "--grep", &mut i)? {
                        o.grep.message.push(v);
                    } else if let Some(v) = a
                        .strip_prefix("--disk-usage=")
                        .filter(|_| cmd == Command::RevList)
                    {
                        if v != "human" {
                            return Err(Die::Fatal(msg::invalid_disk_usage(v)));
                        }
                        o.disk_usage = Some(true);
                    } else if let Some(v) = a.strip_prefix("--ancestry-path=") {
                        o.walk.ancestry_path = true;
                        o.ancestry.push(v.to_string());
                    } else if let Some(v) = a.strip_prefix("--expand-tabs=") {
                        o.tab_width = Some(parse_count(v)?);
                    } else if let Some(v) = a.strip_prefix("--show-linear-break=") {
                        o.linear_break = Some(v.to_string());
                    } else if let Some(v) = a.strip_prefix("--max-count-oldest=") {
                        o.walk.max_count = Some(parse_count(v)?);
                        o.max_count_oldest = true;
                    } else if let Some(v) = a.strip_prefix("--max-count=") {
                        o.walk.max_count = Some(parse_count(v)?);
                        o.max_count_given = true;
                    } else if let Some(v) = a.strip_prefix("--skip=") {
                        o.walk.skip = parse_count(v)?;
                    } else if let Some(v) = a.strip_prefix("-n") {
                        o.walk.max_count = Some(parse_count(v)?);
                    } else if a.len() > 1 && a[1..].bytes().all(|b| b.is_ascii_digit()) {
                        o.walk.max_count = Some(parse_count(&a[1..])?);
                    } else if let Some(v) = a.strip_prefix("--graph-lane-limit=") {
                        o.graph_lane_limit = v
                            .parse::<i64>()
                            .map_err(|_| Die::Fatal(msg::not_an_integer(v)))?
                            .max(0) as usize;
                    } else if let Some(v) = a.strip_prefix("--abbrev=") {
                        let n = parse_count(v)?;
                        o.abbrev = Abbrev::Digits(n.max(MIN_ABBREV));
                    } else if let Some(v) = a
                        .strip_prefix("--pretty=")
                        .or_else(|| a.strip_prefix("--format="))
                    {
                        o.pretty_arg = Some(v.to_string());
                    } else if let Some(v) = a.strip_prefix("--encoding=") {
                        o.encoding = Some(v.to_string());
                    } else if let Some(v) = a.strip_prefix("--date=") {
                        o.date_mode = rgit_date::parse_date_mode(v, false)
                            .map_err(|e| Die::Fatal(e.to_string()))?;
                        o.date_given = true;
                    } else if let Some(v) = a.strip_prefix("--decorate=") {
                        o.decorate = Some(match v {
                            "short" => Decorate::Short,
                            "full" => Decorate::Full,
                            "auto" => Decorate::Auto,
                            "no" => Decorate::No,
                            _ => return Err(Die::Fatal(msg::invalid_decorate(v))),
                        });
                    } else if let Some(v) = long(a, "--decorate-refs-exclude", &mut i)? {
                        o.decorate_filter.exclude.push(v);
                    } else if let Some(v) = long(a, "--decorate-refs", &mut i)? {
                        o.decorate_filter.include.push(v);
                    } else if cmd == Command::RevList {
                        return Err(usage(cmd, a));
                    } else if a == "--combined-all-paths" {
                        o.combined_all_paths = true;
                    } else if let Some((m, on, patch)) = merge_option(a)? {
                        o.merges = Some(m);
                        o.merges_on = on;
                        o.merges_patch = match (o.merges_patch, patch) {
                            (MergesPatch::All, _) | (_, MergesPatch::All) => MergesPatch::All,
                            (MergesPatch::Merges, _) | (_, MergesPatch::Merges) => {
                                MergesPatch::Merges
                            }
                            _ => MergesPatch::None,
                        };
                    } else if !o.diff.parse_arg(a)? {
                        return Err(usage(cmd, a));
                    }
                }
            }
            i += 1;
        }
        o.check(cmd)?;
        Ok(o)
    }

    fn set_objects(&mut self, edge: bool) {
        let m = self.objects.get_or_insert(ObjectsMode {
            edge: false,
            in_commit_order: false,
        });
        m.edge |= edge;
    }

    /// Options that cannot be combined.
    fn check(&mut self, cmd: Command) -> Result<(), Die> {
        if cmd == Command::RevList
            && self.zero
            && (self.left_right
                || self.walk.cherry_mark
                || self.timestamp
                || self.header
                || self.pretty_arg.is_some()
                || self.objects.is_some_and(|m| m.edge)
                || self.disk_usage.is_some()
                || self.bisect_vars
                || self.bisect_all)
        {
            return Err(Die::Fatal(msg::Z_UNSUPPORTED.to_string()));
        }
        if let Some(m) = &mut self.objects {
            m.in_commit_order = self.in_commit_order;
        }
        if self.walk.boundary && self.maximal_only {
            return Err(Die::Fatal(msg::cannot_be_used_together(
                "--boundary",
                "--maximal-only",
            )));
        }
        // `--simplify-merges` asks for parent rewriting like `--parents`.
        if (self.parents || self.walk.simplify_merges) && self.children {
            return Err(Die::Fatal(msg::cannot_be_used_together(
                "--parents",
                "--children",
            )));
        }
        if self.count && self.left_right && self.objects.is_some() {
            return Err(Die::Fatal(msg::MARKED_COUNT_OBJECTS.to_string()));
        }
        self.walk.children = self.children;
        if self.max_count_oldest {
            if self.max_count_given {
                return Err(Die::Fatal(msg::cannot_be_used_together(
                    "--max-count",
                    "--max-count-oldest",
                )));
            }
            if self.walk.skip > 0 {
                return Err(Die::Fatal(msg::cannot_be_used_together(
                    "--skip",
                    "--max-count-oldest",
                )));
            }
        }
        if self.graph {
            if self.linear_break.is_some() {
                return Err(Die::Fatal(msg::cannot_be_used_together(
                    "--show-linear-break",
                    "--graph",
                )));
            }
            if self.walk.reverse {
                return Err(Die::Fatal(msg::cannot_be_used_together(
                    "--graph",
                    "--reverse",
                )));
            }
            if self.walk.no_walk == Some(NoWalk::Sorted) {
                return Err(Die::Fatal(msg::cannot_be_used_together(
                    "--no-walk",
                    "--graph",
                )));
            }
            if !self.order_given {
                self.walk.order = Order::Topo;
            }
        }
        Ok(())
    }

    /// Read revisions and pseudo-options from `input` (`--stdin`). `--not`
    /// read here does not affect the command line and vice versa.
    pub fn read_stdin(&mut self, input: &[u8]) -> Result<(), Die> {
        let saved_negate = std::mem::replace(&mut self.negate, false);
        let after = self.tips.split_off(self.stdin_at.min(self.tips.len()));
        let mut end_of_options = false;
        let text = String::from_utf8_lossy(input);
        let lines: Vec<&str> = text.lines().collect();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            i += 1;
            if line.is_empty() {
                // An empty line ends the input.
                break;
            }
            if line == "--" {
                self.dashdash = true;
                self.paths.extend(
                    lines[i..]
                        .iter()
                        .filter(|l| !l.is_empty())
                        .map(|l| l.to_string()),
                );
                break;
            }
            if end_of_options || !line.starts_with('-') {
                self.tips.push(Tip::Rev {
                    arg: line.to_string(),
                    negate: self.negate,
                });
                continue;
            }
            if line == "--end-of-options" {
                end_of_options = true;
                continue;
            }
            // Values of pseudo-options must be on the same line.
            match self.pseudo(line, None) {
                Ok(Pseudo::Done(_)) => {}
                Ok(Pseudo::No | Pseudo::Invalid) => {
                    return Err(Die::Fatal(msg::invalid_stdin_option(line)));
                }
                Err(e) => return Err(e),
            }
        }
        self.negate = saved_negate;
        self.tips.extend(after);
        Ok(())
    }

    /// No revision arguments and no ref options.
    pub fn has_no_tips(&self) -> bool {
        self.tips.is_empty()
    }

    /// Only `--alternate-refs` (which may find nothing) gave revisions.
    pub fn only_alternate_refs(&self) -> bool {
        !self.tips.is_empty()
            && self.tips.iter().all(|t| {
                matches!(
                    t,
                    Tip::Refs {
                        set: RefSet::AlternateRefs,
                        ..
                    }
                )
            })
    }

    /// Settle the options that depend on the configuration: the format
    /// (`--pretty`, aliases, `format.pretty` for `log`) and `log.date`.
    pub fn apply_config(&mut self, cmd: Command, config: &ConfigSet) -> Result<(), Die> {
        self.apply_grep_config(config);
        let arg = match (&self.pretty_arg, cmd) {
            (Some(a), _) => Some(a.clone()),
            (None, Command::Log) => Some(
                config
                    .string("format.pretty")
                    .ok()
                    .flatten()
                    .map_or_else(|| "medium".to_string(), |v| v.to_string()),
            ),
            (None, Command::RevList) => None,
        };
        self.pretty = arg.map(|a| parse_pretty(&a, config)).transpose()?;
        if cmd == Command::Log {
            self.apply_diff_merges_config(config)?;
        }
        // `log.abbrevCommit`, not for the raw format.
        if cmd == Command::Log
            && !self.abbrev_commit_given
            && self.pretty != Some(Pretty::Raw)
            && config.bool("log.abbrevCommit").ok().flatten() == Some(true)
        {
            self.abbrev_commit = true;
        }
        if cmd == Command::Log
            && !self.date_given
            && let Ok(Some(v)) = config.string("log.date")
        {
            self.date_mode = rgit_date::parse_date_mode(&v.to_string(), false)
                .map_err(|e| Die::Fatal(e.to_string()))?;
        }
        Ok(())
    }

    /// `log.diffMerges`: the format of `-m` and `--diff-merges=on`
    /// (`separate` by default; `on` itself means that). An unknown value
    /// is fatal even when no merge diff is asked for.
    fn apply_diff_merges_config(&mut self, config: &ConfigSet) -> Result<(), Die> {
        let Some(e) = config.get("log.diffmerges") else {
            return Ok(());
        };
        let value = e.string().map(|v| v.to_string()).unwrap_or_default();
        let format = match value.as_str() {
            "on" | "m" | "separate" => MergeDiff::Separate,
            v => merge_format(v).ok_or_else(|| {
                Die::Fatal(crate::revisions::unable_to_parse("log.diffmerges", e))
            })?,
        };
        if self.merges_on {
            self.merges = Some(format);
        }
        Ok(())
    }

    /// The pattern syntax of `--grep` and friends from `grep.patternType`
    /// (`basic`, `extended`, `fixed`, `perl`; `default` defers to
    /// `grep.extendedRegexp`), unless the command line chose one.
    pub fn apply_grep_config(&mut self, config: &ConfigSet) {
        if self.syntax_given {
            return;
        }
        let pattern_type = config
            .string("grep.patterntype")
            .ok()
            .flatten()
            .map(|v| v.to_string().to_ascii_lowercase());
        self.grep.syntax = match pattern_type.as_deref() {
            Some("basic") => Syntax::Basic,
            Some("extended") => Syntax::Extended,
            Some("fixed") => Syntax::Fixed,
            Some("perl") => Syntax::Perl,
            _ if config.bool("grep.extendedregexp").ok().flatten() == Some(true) => {
                Syntax::Extended
            }
            _ => Syntax::Basic,
        };
    }

    /// `--bisect`: start at the refs `refs/bisect/<new>*` (when `has_new`,
    /// some exist) and exclude `refs/bisect/<old>*` (when `has_old`), as
    /// the reference git matches them: by prefix, with the current terms.
    pub fn push_bisect_refs(&mut self, new: &str, old: &str, has_new: bool, has_old: bool) {
        for (term, negative, has) in [(new, false, has_new), (old, true, has_old)] {
            if has {
                self.tips.push(Tip::Refs {
                    set: RefSet::Glob(format!("refs/bisect/{term}*")),
                    negative,
                    exclude: Vec::new(),
                    hidden: None,
                });
            }
        }
    }

    /// Start at `HEAD` (log without revisions).
    pub fn push_head(&mut self) {
        self.default_head = true;
        self.tips.push(Tip::Rev {
            arg: "HEAD".into(),
            negate: false,
        });
    }
}

/// `-m`, `-c`, `--cc`, `--dd`, `--remerge-diff`, `--diff-merges=<mode>`,
/// `--no-diff-merges`: the format, whether it is `log.diffMerges`
/// (`on`), and the patch it implies.
fn merge_option(arg: &str) -> Result<Option<(MergeDiff, bool, MergesPatch)>, Die> {
    let (mode, patch) = match arg {
        "-m" => ("on", MergesPatch::None),
        "-c" => ("combined", MergesPatch::All),
        "--cc" => ("dense-combined", MergesPatch::All),
        "--dd" => ("first-parent", MergesPatch::All),
        "--remerge-diff" => ("remerge", MergesPatch::All),
        "--no-diff-merges" => ("off", MergesPatch::None),
        _ => match arg.strip_prefix("--diff-merges=") {
            Some(v) => (v, MergesPatch::Merges),
            None => return Ok(None),
        },
    };
    let on = matches!(mode, "on" | "m");
    let format = match mode {
        "on" | "m" => MergeDiff::Separate,
        _ => merge_format(mode).ok_or_else(|| Die::Fatal(msg::unknown_diff_merges(mode)))?,
    };
    let patch = if format == MergeDiff::Off {
        MergesPatch::None
    } else {
        patch
    };
    Ok(Some((format, on, patch)))
}

/// A merge-diff format by name (`--diff-merges=<format>`, `log.diffMerges`).
fn merge_format(name: &str) -> Option<MergeDiff> {
    Some(match name {
        "off" | "none" => MergeDiff::Off,
        "first-parent" | "1" => MergeDiff::FirstParent,
        "separate" => MergeDiff::Separate,
        "combined" | "c" => MergeDiff::Combined,
        "dense-combined" | "cc" => MergeDiff::Dense,
        "remerge" | "r" => MergeDiff::Remerge,
        _ => return None,
    })
}

fn ref_prefix(option: &str) -> &'static str {
    match option {
        "--branches" => "refs/heads/",
        "--tags" => "refs/tags/",
        _ => "refs/remotes/",
    }
}
