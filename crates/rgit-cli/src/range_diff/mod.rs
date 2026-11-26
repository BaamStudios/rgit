// SPDX-License-Identifier: MIT
//! The comparison of two commit ranges behind `git range-diff` and
//! `format-patch --range-diff` (Documentation/git-range-diff.adoc).
//!
//! Both ranges are read as patches through a `log -p` child
//! ([`patches`]). Every pair of commits costs the number of lines of the
//! diff between their diffs (3 lines of context, hunk headers counted);
//! leaving a commit unpaired costs the number of lines of its diff times
//! the creation factor in percent (rounded down per commit). The pairing
//! is the minimum-cost assignment ([`lapjv`]) over the square matrix of
//! the left commits plus one dummy per right commit (rows) against the
//! right commits plus one dummy per left commit (columns). Commits with
//! identical diffs are paired before. In some series with many equally
//! good pairings git picks another one (rgit #615).
//!
//! The pairs are shown in the order of the right range; an unpaired left
//! commit comes as soon as all left commits before it were shown. A pair
//! whose patches differ shows the diff between the whole patches
//! (metadata, message, notes and diff), each line indented by four
//! spaces, with `@@ <section>` hunk headers. The colours were taken from
//! the `git` binary, including the "dual" mode that keeps the colours of
//! the inner diff and marks the outer `-`/`+` in reverse video.

pub mod lapjv;
pub mod patches;

use std::process::{Command, Stdio};

use regex::bytes::Regex;
use rgit_config::ConfigSet;
use rgit_diff::{DiffOptions, FuncName, FuncPattern};
use rgit_hash::ObjectId;
use rgit_object::Object;

use crate::Die;
use crate::color::RESET;
use crate::context::Context;
use crate::messages::range_diff as msg;
use crate::repository::Repo;
use patches::Patch;

/// The default creation factor in percent.
pub const DEFAULT_CREATION_FACTOR: i64 = 60;

/// The default creation factor of `format-patch --range-diff`.
pub const FORMAT_PATCH_CREATION_FACTOR: i64 = 999;

/// The default limit of the cost matrix (`--max-memory`).
pub const DEFAULT_MAX_MEMORY: u64 = 4 << 30;

/// How to compare and what to show.
pub struct Options {
    pub creation_factor: i64,
    pub dual_color: bool,
    /// `--left-only`: no commits that only the second range has.
    pub left_only: bool,
    /// `--right-only`: no commits that only the first range has.
    pub right_only: bool,
    /// Extra arguments for the `log` children (`--notes`,
    /// `--diff-merges`, ...).
    pub log_args: Vec<String>,
    /// A `--diff-merges` or `--remerge-diff` option was given, so that
    /// merges are not left out.
    pub include_merges: bool,
    pub max_memory: u64,
    /// The diff between two patches.
    pub diff: DiffOptions,
    pub patch: bool,
    pub stat: Option<crate::diff::stat::StatLayout>,
    pub color: Option<Colors>,
    /// Digits of the object names; 0 for full names.
    pub abbrev: usize,
}

impl Options {
    pub fn new(abbrev: usize) -> Options {
        Options {
            creation_factor: DEFAULT_CREATION_FACTOR,
            dual_color: true,
            left_only: false,
            right_only: false,
            log_args: Vec::new(),
            include_merges: false,
            max_memory: DEFAULT_MAX_MEMORY,
            diff: DiffOptions::default(),
            patch: true,
            stat: None,
            color: None,
            abbrev,
        }
    }
}

/// The colour slots used (`color.diff.<slot>`).
#[derive(Debug, Clone)]
pub struct Colors {
    pub commit: String,
    pub context: String,
    pub frag: String,
    pub func: String,
    pub old: String,
    pub new: String,
    pub context_dimmed: String,
    pub old_dimmed: String,
    pub new_dimmed: String,
    pub context_bold: String,
    pub old_bold: String,
    pub new_bold: String,
}

const REVERSE: &str = "\x1b[7m";

impl Colors {
    pub fn from_config(config: &ConfigSet) -> Result<Colors, Die> {
        // `diff.color.<slot>` is the old name of `color.diff.<slot>`,
        // and `plain` the old name of `context`.
        let get = |names: &[&str], default: &str| -> Result<String, Die> {
            for name in names {
                for key in [format!("color.diff.{name}"), format!("diff.color.{name}")] {
                    if config.get(&key).is_some() {
                        return crate::color::slot(config, &key, default);
                    }
                }
            }
            Ok(default.to_owned())
        };
        Ok(Colors {
            commit: get(&["commit"], "\x1b[33m")?,
            context: get(&["context", "plain"], "")?,
            frag: get(&["frag"], "\x1b[36m")?,
            func: get(&["func"], "")?,
            old: get(&["old"], "\x1b[31m")?,
            new: get(&["new"], "\x1b[32m")?,
            context_dimmed: get(&["contextdimmed"], "\x1b[2m")?,
            old_dimmed: get(&["olddimmed"], "\x1b[2;31m")?,
            new_dimmed: get(&["newdimmed"], "\x1b[2;32m")?,
            context_bold: get(&["contextbold"], "\x1b[1m")?,
            old_bold: get(&["oldbold"], "\x1b[1;31m")?,
            new_bold: get(&["newbold"], "\x1b[1;32m")?,
        })
    }
}

/// Reads the commits of `range` (limited to `paths`) as patches.
fn read_patches(
    ctx: &Context,
    range: &str,
    paths: &[String],
    opts: &Options,
) -> Result<Vec<Patch>, Die> {
    let exe = std::env::current_exe().map_err(|e| Die::Fatal(e.to_string()))?;
    let mut args: Vec<String> = [
        "log",
        "--no-color",
        "-p",
        "--reverse",
        "--date-order",
        "--decorate=no",
        "--no-prefix",
        "--submodule=short",
        "--output-indicator-new=>",
        "--output-indicator-old=<",
        "--output-indicator-context=#",
        "--no-abbrev-commit",
        "--pretty=medium",
        "--show-notes-by-default",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if !opts.include_merges {
        args.push("--no-merges".to_string());
    }
    args.push(range.to_string());
    args.extend(opts.log_args.iter().cloned());
    args.extend(paths.iter().cloned());
    let out = Command::new(exe)
        .args(&args)
        .envs(ctx.child_env_with(false))
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let parsed = if out.status.success() {
        patches::parse(&out.stdout)
    } else {
        None
    };
    parsed.ok_or_else(|| Die::Error(msg::could_not_parse_log(range), 255))
}

/// The pairing: `left[i]` is the right commit of left commit `i`.
struct Pairing {
    left: Vec<Option<usize>>,
    right: Vec<Option<usize>>,
}

/// The number of lines of the diff between two diffs.
fn diff_size(a: &[u8], b: &[u8]) -> i64 {
    if a == b {
        return 0;
    }
    let d = rgit_diff::diff_text(a, b, &DiffOptions::default());
    d.hunks.iter().filter(|&&c| c == b'\n').count() as i64
}

/// A cost no assignment takes when it has a choice.
const FORBIDDEN: i64 = 1 << 40;

fn pair(a: &[Patch], b: &[Patch], factor: i64) -> Pairing {
    let (n, m) = (a.len(), b.len());
    let size = n + m;
    // Commits with the same diff are paired first, each right commit
    // with the first unpaired left commit.
    let mut exact_left: Vec<Option<usize>> = vec![None; n];
    let mut exact_right: Vec<Option<usize>> = vec![None; m];
    for (j, pb) in b.iter().enumerate() {
        if let Some(i) = (0..n).find(|&i| exact_left[i].is_none() && a[i].diff() == pb.diff()) {
            exact_left[i] = Some(j);
            exact_right[j] = Some(i);
        }
    }
    let mut cost = vec![0i64; size * size];
    for (i, pa) in a.iter().enumerate() {
        let delete = pa.diff_lines as i64 * factor / 100;
        for (j, pb) in b.iter().enumerate() {
            cost[i * size + j] = match (exact_left[i], exact_right[j]) {
                (Some(x), _) if x == j => 0,
                (None, None) => diff_size(pa.diff(), pb.diff()),
                _ => FORBIDDEN,
            };
        }
        for j in m..size {
            cost[i * size + j] = if exact_left[i].is_some() {
                FORBIDDEN
            } else {
                delete
            };
        }
    }
    for (j, pb) in b.iter().enumerate() {
        let create = pb.diff_lines as i64 * factor / 100;
        for i in n..size {
            cost[i * size + j] = if exact_right[j].is_some() {
                FORBIDDEN
            } else {
                create
            };
        }
    }
    let x = lapjv::assign(size, &cost);
    let mut left = vec![None; n];
    let mut right = vec![None; m];
    for i in 0..n {
        if x[i] < m {
            left[i] = Some(x[i]);
            right[x[i]] = Some(i);
        }
    }
    Pairing { left, right }
}

/// The subject of a commit as `%s` shows it: the first paragraph, its
/// lines without trailing whitespace joined by spaces.
fn subject(repo: &Repo, id: &ObjectId) -> Vec<u8> {
    let Some(Object::Commit(c)) = repo.read_object(id) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in c.message().split(|&b| b == b'\n') {
        let line = line.trim_ascii_end();
        if line.is_empty() {
            if out.is_empty() {
                continue;
            }
            break;
        }
        if !out.is_empty() {
            out.push(b' ');
        }
        out.extend_from_slice(line);
    }
    out
}

/// What a line of the output is about.
enum Status {
    Deleted,
    Added,
    Same,
    Changed,
}

struct Writer<'a> {
    repo: &'a Repo,
    opts: &'a Options,
    width: usize,
    funcname: FuncName,
    out: Vec<u8>,
}

impl Writer<'_> {
    fn name(&self, id: &ObjectId) -> String {
        if self.opts.abbrev == 0 {
            id.to_hex()
        } else {
            self.repo.abbrev(id, self.opts.abbrev)
        }
    }

    fn dashes(&self, id: &ObjectId) -> String {
        let len = if self.opts.abbrev == 0 {
            id.to_hex().len()
        } else {
            self.opts.abbrev.min(id.to_hex().len())
        };
        "-".repeat(len)
    }

    fn header(&mut self, a: Option<(usize, &Patch)>, b: Option<(usize, &Patch)>, status: &Status) {
        let w = self.width;
        let left = match a {
            Some((i, p)) => format!("{:>w$}:  {}", i + 1, self.name(&p.id)),
            None => {
                let id = b.expect("one side").1.id;
                format!("{:>w$}:  {}", "-", self.dashes(&id))
            }
        };
        let right = match b {
            Some((j, p)) => format!("{:>w$}:  {}", j + 1, self.name(&p.id)),
            None => {
                let id = a.expect("one side").1.id;
                format!("{:>w$}:  {}", "-", self.dashes(&id))
            }
        };
        let subject = subject(self.repo, &a.or(b).expect("one side").1.id);
        let sign = match status {
            Status::Deleted => '<',
            Status::Added => '>',
            Status::Same => '=',
            Status::Changed => '!',
        };
        let out = &mut self.out;
        match &self.opts.color {
            None => {
                out.extend_from_slice(format!("{left} {sign} {right} ").as_bytes());
                out.extend_from_slice(&subject);
            }
            Some(c) => {
                let whole = match status {
                    Status::Deleted => Some(&c.old),
                    Status::Added => Some(&c.new),
                    Status::Same => Some(&c.commit),
                    Status::Changed => None,
                };
                match whole {
                    Some(color) => {
                        out.extend_from_slice(color.as_bytes());
                        out.extend_from_slice(format!("{left} {sign} {right} ").as_bytes());
                        out.extend_from_slice(&subject);
                        out.extend_from_slice(RESET.as_bytes());
                    }
                    None => {
                        let parts = [
                            (&c.old, format!("{left} ").into_bytes()),
                            (&c.commit, format!("{sign}").into_bytes()),
                            (&c.new, format!(" {right}").into_bytes()),
                            (&c.commit, [b" ".as_slice(), &subject].concat()),
                        ];
                        for (color, text) in parts {
                            out.extend_from_slice(color.as_bytes());
                            out.extend_from_slice(&text);
                            out.extend_from_slice(RESET.as_bytes());
                        }
                    }
                }
            }
        }
        out.push(b'\n');
    }

    /// The diff between the patches of a pair.
    fn patch_diff(&mut self, a: &Patch, b: &Patch) {
        let mut opts = self.opts.diff.clone();
        opts.funcname = Some(self.funcname.clone());
        let d = rgit_diff::diff_text(&a.text, &b.text, &opts);
        if let Some(layout) = &self.opts.stat {
            use crate::diff::stat::{FileStat, StatKind, write_stat};
            let files = [FileStat {
                name: b"a => b".to_vec(),
                kind: StatKind::Text {
                    added: d.stats.insertions,
                    deleted: d.stats.deletions,
                },
            }];
            let mut stat = Vec::new();
            let colors = self
                .opts
                .color
                .as_ref()
                .map(|c| (c.new.as_str(), c.old.as_str()));
            write_stat(&mut stat, &files, layout, colors);
            for line in stat.split_inclusive(|&b| b == b'\n') {
                self.out.extend_from_slice(b"    ");
                self.out.extend_from_slice(line);
            }
        }
        if !self.opts.patch {
            return;
        }
        for line in d.hunks.split(|&b| b == b'\n') {
            if line.is_empty() {
                continue;
            }
            self.out.extend_from_slice(b"    ");
            if line.starts_with(b"@@ -") {
                // `@@ -a,b +c,d @@ <section>`.
                let func = line[4..]
                    .windows(3)
                    .position(|w| w == b" @@")
                    .map(|i| &line[4 + i + 3..])
                    .unwrap_or_default();
                let func = func.strip_prefix(b" ").unwrap_or(func);
                self.hunk_header(func);
            } else {
                self.diff_line(line[0], &line[1..]);
            }
            self.out.push(b'\n');
        }
    }

    fn hunk_header(&mut self, func: &[u8]) {
        let out = &mut self.out;
        let Some(c) = &self.opts.color else {
            out.extend_from_slice(b"@@");
            if !func.is_empty() {
                out.push(b' ');
                out.extend_from_slice(func);
            }
            return;
        };
        if self.opts.dual_color {
            out.extend_from_slice(REVERSE.as_bytes());
        }
        paint(out, &c.frag, b"@@");
        if !func.is_empty() {
            paint(out, &c.context, b" ");
            paint(out, &c.func, func);
        }
    }

    fn diff_line(&mut self, sign: u8, content: &[u8]) {
        let out = &mut self.out;
        let Some(c) = &self.opts.color else {
            out.push(sign);
            out.extend_from_slice(content);
            return;
        };
        if !self.opts.dual_color {
            match sign {
                b'-' => {
                    out.extend_from_slice(c.old.as_bytes());
                    out.push(b'-');
                    out.extend_from_slice(content);
                    out.extend_from_slice(RESET.as_bytes());
                }
                b'+' => {
                    paint(out, &c.new, b"+");
                    if !content.is_empty() {
                        paint(out, &c.new, content);
                    }
                }
                _ => {
                    out.extend_from_slice(c.context.as_bytes());
                    out.push(sign);
                    out.extend_from_slice(content);
                    out.extend_from_slice(RESET.as_bytes());
                }
            }
            return;
        }
        // Dual colour: the inner diff keeps its colours.
        let inner = |old: &str, new: &str, context: &str| -> String {
            match content.first() {
                Some(b'-') => old.to_string(),
                Some(b'+') => new.to_string(),
                Some(b'@') => c.frag.clone(),
                _ => context.to_string(),
            }
        };
        match sign {
            b'-' | b'+' => {
                out.extend_from_slice(REVERSE.as_bytes());
                let (outer, color) = if sign == b'-' {
                    (
                        &c.old,
                        inner(&c.old_dimmed, &c.new_dimmed, &c.context_dimmed),
                    )
                } else {
                    (&c.new, inner(&c.old_bold, &c.new_bold, &c.context_bold))
                };
                paint(out, outer, &[sign]);
                if !content.is_empty() {
                    paint(out, &color, content);
                }
            }
            _ => {
                let color = inner(&c.old, &c.new, &c.context);
                out.extend_from_slice(color.as_bytes());
                out.push(sign);
                out.extend_from_slice(content);
                out.extend_from_slice(RESET.as_bytes());
            }
        }
    }
}

fn paint(out: &mut Vec<u8>, color: &str, text: &[u8]) {
    out.extend_from_slice(color.as_bytes());
    out.extend_from_slice(text);
    out.extend_from_slice(RESET.as_bytes());
}

/// The function names of the diff between patches: the section of a
/// `## <section> ##` line, the text after `@@ ` of a hunk header.
fn section_funcname() -> FuncName {
    let pattern = |re: &str| FuncPattern {
        negate: false,
        regex: Regex::new(re).expect("valid pattern"),
    };
    FuncName::new(vec![pattern(r"^ ## (.*) ##$"), pattern(r"^@@ (.+)$")])
}

/// Whether `arg` names a range of commits for the two-range form.
pub fn is_range(arg: &str) -> bool {
    if arg.ends_with("^!") || arg.contains("^-") {
        return true;
    }
    // `..` inside `^{...}` (e.g. `HEAD^{/..}`) does not count.
    let mut plain = String::new();
    let mut depth = 0;
    let mut chars = arg.chars().peekable();
    while let Some(c) = chars.next() {
        if depth == 0 && c == '^' && chars.peek() == Some(&'{') {
            depth = 1;
            chars.next();
            plain.push('\0');
            continue;
        }
        if depth > 0 {
            if c == '}' {
                depth -= 1;
            } else if c == '{' {
                depth += 1;
            }
            continue;
        }
        plain.push(c);
    }
    match plain.find("..") {
        None => false,
        Some(i) => {
            let rest = plain[i + 2..].strip_prefix('.').unwrap_or(&plain[i + 2..]);
            i > 0 || !rest.is_empty()
        }
    }
}

pub fn is_symmetric(arg: &str) -> bool {
    arg.contains("...")
}

/// Whether `name` resolves to a commit.
pub fn is_commit(repo: &mut Repo, name: &str) -> bool {
    let name = if name.is_empty() { "HEAD" } else { name };
    repo.resolve(&format!("{name}^{{commit}}")).is_some()
}

/// Whether `arg` is a revision or a range whose ends are revisions.
pub fn is_revision_arg(repo: &mut Repo, arg: &str) -> bool {
    let arg = arg.strip_prefix('^').unwrap_or(arg);
    if let Some(base) = arg.strip_suffix("^!").or_else(|| arg.strip_suffix("^@")) {
        return is_commit(repo, base);
    }
    if let Some(i) = arg.rfind("^-") {
        let (base, n) = (&arg[..i], &arg[i + 2..]);
        let n = if n.is_empty() { "1" } else { n };
        return n.bytes().all(|b| b.is_ascii_digit()) && is_commit(repo, &format!("{base}^{n}"));
    }
    if !is_commit(repo, arg) {
        for sep in ["...", ".."] {
            if let Some(i) = arg.find(sep) {
                return is_commit(repo, &arg[..i]) && is_commit(repo, &arg[i + sep.len()..]);
            }
        }
        return false;
    }
    true
}

/// Compares the commits of `range1` with those of `range2` and returns
/// the output.
pub fn compare(
    ctx: &Context,
    repo: &Repo,
    range1: &str,
    range2: &str,
    paths: &[String],
    opts: &Options,
) -> Result<Vec<u8>, Die> {
    let a = read_patches(ctx, range1, paths, opts)?;
    let b = read_patches(ctx, range2, paths, opts)?;
    let size = (a.len() + b.len()) as u64;
    let needed = size * size * 4;
    if needed > opts.max_memory {
        return Err(Die::Fatal(msg::exceeds_max_memory(needed, opts.max_memory)));
    }
    let pairing = pair(&a, &b, opts.creation_factor);
    let width = a.len().max(b.len()).to_string().len();
    let mut w = Writer {
        repo,
        opts,
        width,
        funcname: section_funcname(),
        out: Vec::new(),
    };
    let (n, m) = (a.len(), b.len());
    let mut shown = vec![false; n];
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        while i < n && shown[i] {
            i += 1;
        }
        if i < n && pairing.left[i].is_none() {
            if !opts.right_only {
                w.header(Some((i, &a[i])), None, &Status::Deleted);
            }
            i += 1;
            continue;
        }
        while j < m && pairing.right[j].is_none() {
            if !opts.left_only {
                w.header(None, Some((j, &b[j])), &Status::Added);
            }
            j += 1;
        }
        if j < m {
            let li = pairing.right[j].expect("paired");
            let same = a[li].text == b[j].text;
            let status = if same { Status::Same } else { Status::Changed };
            w.header(Some((li, &a[li])), Some((j, &b[j])), &status);
            if !same {
                w.patch_diff(&a[li], &b[j]);
            }
            shown[li] = true;
            j += 1;
        }
    }
    Ok(w.out)
}
