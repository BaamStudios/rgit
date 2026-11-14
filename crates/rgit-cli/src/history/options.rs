// SPDX-License-Identifier: MIT
//! The options of `rev-list` and `log` (Documentation/rev-list-options.adoc)
//! and the revision arguments, from the command line and from `--stdin`.

use std::io::Write;

use rgit_config::color::ColorBool;
use rgit_date::{DateContext, DateMode};
use rgit_revwalk::walk::grep::{GrepOptions, Syntax};
use rgit_revwalk::walk::{NoWalk, Order, WalkOptions};

use super::Command;
use super::pretty::Pretty;
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
    pub abbrev: Abbrev,
    pub pretty: Option<Pretty>,
    pub date_mode: DateMode,
    /// `--[no-]mailmap`; `None` = the command's default.
    pub mailmap: Option<bool>,
    /// `--color[=<when>]` / `--no-color` (only `log` colours).
    pub color: Option<ColorBool>,
    /// `--in-commit-order` (takes effect with `--objects`).
    in_commit_order: bool,
    /// State while parsing: an odd number of `--not` so far.
    negate: bool,
    /// State while parsing: `--exclude` patterns for the next ref set.
    exclude: Vec<String>,
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

fn parse_pretty(v: &str) -> Result<Pretty, Die> {
    if v.contains('%') || v.starts_with("format:") || v.starts_with("tformat:") {
        return Err(Die::Fatal(msg::not_supported("--format with placeholders")));
    }
    Pretty::from_name(v).ok_or_else(|| Die::Fatal(msg::invalid_pretty(v)))
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
    fn new(cmd: Command) -> Options {
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
            abbrev: Abbrev::Default,
            pretty: (cmd == Command::Log).then_some(Pretty::Medium),
            date_mode: DateMode::default(),
            mailmap: None,
            color: None,
            in_commit_order: false,
            negate: false,
            exclude: Vec::new(),
        }
    }

    fn push_refs(&mut self, set: RefSet) {
        let exclude = std::mem::take(&mut self.exclude);
        self.tips.push(Tip::Refs {
            set,
            negative: self.negate,
            exclude,
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
            "--branches" | "--tags" | "--remotes" => {
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
            _ if a == "--exclude" || a.starts_with("--exclude=") => {
                let (v, used) = value("--exclude")?;
                self.exclude.push(v);
                return Ok(Pseudo::Done(used));
            }
            _ => {
                for name in ["--branches", "--tags", "--remotes"] {
                    if let Some(p) = a.strip_prefix(name).and_then(|r| r.strip_prefix('=')) {
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

    /// Parse `argv` (without the command name).
    pub fn parse(cmd: Command, argv: &[String]) -> Result<Options, Die> {
        let mut o = Options::new(cmd);
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
                if i + 1 < argv.len() {
                    return Err(Die::Fatal(msg::not_supported("path limiting")));
                }
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
                "--topo-order" => o.walk.order = Order::Topo,
                "--date-order" => o.walk.order = Order::Date,
                "--author-date-order" => o.walk.order = Order::AuthorDate,
                "--reverse" => o.walk.reverse = true,
                "--first-parent" => o.walk.first_parent = true,
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
                "--object-names" if cmd == Command::RevList => o.object_names = true,
                "--no-object-names" if cmd == Command::RevList => o.object_names = false,
                "--stdin" => {
                    o.stdin = true;
                    o.stdin_at = o.tips.len();
                }
                "--boundary" => o.walk.boundary = true,
                "--left-right" => o.left_right = true,
                "--left-only" => o.walk.left_only = true,
                "--right-only" => o.walk.right_only = true,
                "--ancestry-path" => o.walk.ancestry_path = true,
                "--merges" => o.walk.min_parents = Some(2),
                "--no-merges" => o.walk.max_parents = Some(1),
                "--no-min-parents" => o.walk.min_parents = None,
                "--no-max-parents" => o.walk.max_parents = None,
                "-i" | "--regexp-ignore-case" => o.grep.ignore_case = true,
                "-E" | "--extended-regexp" => o.grep.syntax = Syntax::Extended,
                "-F" | "--fixed-strings" => o.grep.syntax = Syntax::Fixed,
                "-P" | "--perl-regexp" => o.grep.syntax = Syntax::Perl,
                "--basic-regexp" => o.grep.syntax = Syntax::Basic,
                "--all-match" => o.grep.all_match = true,
                "--invert-grep" => o.grep.invert = true,
                "--cherry-mark" | "--cherry-pick" | "--cherry" => {
                    return Err(Die::Fatal(msg::not_supported(a)));
                }
                "--abbrev-commit" => o.abbrev_commit = true,
                "--no-abbrev-commit" => o.abbrev_commit = false,
                "--abbrev" => o.abbrev = Abbrev::Default,
                "--no-abbrev" => o.abbrev = Abbrev::Full,
                "--oneline" => {
                    o.pretty = Some(Pretty::Oneline);
                    o.abbrev_commit = true;
                }
                "--pretty" => o.pretty = Some(Pretty::Medium),
                "--no-decorate" | "--no-show-signature" => {}
                "--mailmap" | "--use-mailmap" => o.mailmap = Some(true),
                "--no-mailmap" | "--no-use-mailmap" => o.mailmap = Some(false),
                "-n" | "--max-count" => {
                    o.walk.max_count = Some(parse_count(&value(&mut i, a)?)?);
                }
                "--skip" => o.walk.skip = parse_count(&value(&mut i, a)?)?,
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
                    } else if let Some(v) = a.strip_prefix("--ancestry-path=") {
                        o.walk.ancestry_path = true;
                        o.ancestry.push(v.to_string());
                    } else if let Some(v) = a.strip_prefix("--max-count=") {
                        o.walk.max_count = Some(parse_count(v)?);
                    } else if let Some(v) = a.strip_prefix("--skip=") {
                        o.walk.skip = parse_count(v)?;
                    } else if let Some(v) = a.strip_prefix("-n") {
                        o.walk.max_count = Some(parse_count(v)?);
                    } else if a.len() > 1 && a[1..].bytes().all(|b| b.is_ascii_digit()) {
                        o.walk.max_count = Some(parse_count(&a[1..])?);
                    } else if let Some(v) = a.strip_prefix("--abbrev=") {
                        let n = parse_count(v)?;
                        o.abbrev = if n == 0 {
                            Abbrev::Full
                        } else {
                            Abbrev::Digits(n.max(MIN_ABBREV))
                        };
                    } else if let Some(v) = a
                        .strip_prefix("--pretty=")
                        .or_else(|| a.strip_prefix("--format="))
                    {
                        o.pretty = Some(parse_pretty(v)?);
                    } else if let Some(v) = a.strip_prefix("--date=") {
                        o.date_mode = rgit_date::parse_date_mode(v, false)
                            .map_err(|e| Die::Fatal(e.to_string()))?;
                    } else if a == "--decorate=no" {
                    } else {
                        return Err(usage(cmd, a));
                    }
                }
            }
            i += 1;
        }
        o.check()?;
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
    fn check(&mut self) -> Result<(), Die> {
        if let Some(m) = &mut self.objects {
            m.in_commit_order = self.in_commit_order;
        }
        if self.parents && self.children {
            return Err(Die::Fatal(msg::cannot_be_used_together(
                "--parents",
                "--children",
            )));
        }
        if self.count && self.left_right && self.objects.is_some() {
            return Err(Die::Fatal(msg::MARKED_COUNT_OBJECTS.to_string()));
        }
        self.walk.children = self.children;
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
                if lines[i..].iter().any(|l| !l.is_empty()) {
                    return Err(Die::Fatal(msg::not_supported("path limiting")));
                }
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

    /// Start at `HEAD` (log without revisions).
    pub fn push_head(&mut self) {
        self.tips.push(Tip::Rev {
            arg: "HEAD".into(),
            negate: false,
        });
    }
}

fn ref_prefix(option: &str) -> &'static str {
    match option {
        "--branches" => "refs/heads/",
        "--tags" => "refs/tags/",
        _ => "refs/remotes/",
    }
}
