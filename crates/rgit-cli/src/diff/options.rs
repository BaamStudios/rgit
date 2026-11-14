// SPDX-License-Identifier: MIT
//! The options all diff commands share (Documentation/diff-options.adoc),
//! parsed one argument at a time so that each command can mix in its own
//! options and arguments.

use rgit_diff::diffcore::{self, Detect, filter::DiffFilter, parse_score};

use crate::Die;
use crate::messages::diff as msg;

/// What the command line asked for.
#[derive(Debug, Clone, Default)]
pub struct DiffOpts {
    pub raw: bool,
    pub patch: bool,
    pub name_only: bool,
    pub name_status: bool,
    pub numstat: bool,
    pub stat: bool,
    pub shortstat: bool,
    pub summary: bool,
    /// `-s`: no output at all.
    pub no_output: bool,
    pub nul: bool,
    pub recursive: bool,
    pub show_trees: bool,
    pub reverse: bool,
    pub exit_code: bool,
    pub quiet: bool,
    /// `--abbrev[=<n>]`: `Some(None)` for the default length, `Some(Some(0))`
    /// for full names.
    pub abbrev: Option<Option<usize>>,
    pub full_index: bool,
    pub context: Option<usize>,
    pub interhunk: Option<usize>,
    pub indent_heuristic: Option<bool>,
    /// `--minimal` (it also selects Myers); the other algorithm options
    /// reset it.
    pub minimal: Option<bool>,
    /// `--patience`, `--histogram`, `--anchored`, `--diff-algorithm`.
    pub algorithm: Option<rgit_diff::Algorithm>,
    /// `--anchored=<text>`, in order.
    pub anchors: Vec<Vec<u8>>,
    pub src_prefix: Option<Vec<u8>>,
    pub dst_prefix: Option<Vec<u8>>,
    pub no_prefix: bool,
    pub default_prefix: bool,
    pub stat_width: Option<usize>,
    pub stat_name_width: Option<usize>,
    pub stat_graph_width: Option<usize>,
    pub stat_count: Option<usize>,
    /// `--color[=<when>]` / `--no-color`.
    pub color: Option<rgit_config::color::ColorBool>,
    /// `-M`, `-C` or `--no-renames`, when given.
    pub detect: Option<Detect>,
    /// The score of `-M<n>` / `-C<n>`.
    pub rename_score: Option<u32>,
    /// `--find-copies-harder` (or `-C` twice).
    pub copies_harder: bool,
    /// `-B[<n>][/<m>]`: break and merge score.
    pub break_rewrites: Option<(u32, u32)>,
    /// `-l<n>`.
    pub rename_limit: Option<usize>,
    /// `--[no-]rename-empty`.
    pub rename_empty: Option<bool>,
    /// `-S<string>` (`false`) or `-G<regex>` (`true`).
    pub pickaxe: Option<(bool, String)>,
    pub pickaxe_all: bool,
    pub pickaxe_regex: bool,
    /// `--find-object=<object>`, unresolved.
    pub find_object: Vec<String>,
    /// The objects of `find_object` (see [`DiffOpts::resolve_objects`]).
    pub found_objects: Vec<rgit_hash::ObjectId>,
    /// `-D` / `--irreversible-delete`.
    pub irreversible_delete: bool,
    /// `--diff-filter=...`.
    pub diff_filter: DiffFilter,
}

/// A score option's value (`-M<n>`, `--find-copies=<n>`): the whole value
/// must be a score.
fn score_value(value: &str, option: &str) -> Result<u32, Die> {
    match parse_score(value) {
        (Some(s), "") => Ok(s),
        _ => Err(usage_exit(msg::invalid_score(option))),
    }
}

/// `-B` values: `[<n>][/<m>]`; 0 or nothing means the default.
fn break_value(value: &str) -> Result<(u32, u32), Die> {
    let bad = || usage_exit(msg::BREAK_FORM.to_string());
    let (n, rest) = parse_score(value);
    let (m, rest) = match rest.strip_prefix('/') {
        Some(r) => parse_score(r),
        None if rest.is_empty() => (None, ""),
        None => return Err(bad()),
    };
    if !rest.is_empty() {
        return Err(bad());
    }
    let or = |v: Option<u32>, d: u32| v.filter(|&v| v != 0).unwrap_or(d);
    Ok((
        or(n, diffcore::DEFAULT_BREAK_SCORE),
        or(m, diffcore::DEFAULT_MERGE_SCORE),
    ))
}

/// An integer with an optional `k`/`m`/`g` suffix (`-l`).
fn magnitude(value: &str) -> Option<usize> {
    let (num, factor) = match value.as_bytes().last() {
        Some(b'k' | b'K') => (&value[..value.len() - 1], 1024),
        Some(b'm' | b'M') => (&value[..value.len() - 1], 1024 * 1024),
        Some(b'g' | b'G') => (&value[..value.len() - 1], 1024 * 1024 * 1024),
        _ => (value, 1),
    };
    let n: i64 = num.parse().ok()?;
    usize::try_from(n.max(0)).ok()?.checked_mul(factor)
}

/// The output formats after resolving overrides and the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Formats {
    pub raw: bool,
    pub patch: bool,
    pub name_only: bool,
    pub name_status: bool,
    pub numstat: bool,
    pub stat: bool,
    pub shortstat: bool,
    pub summary: bool,
}

impl Formats {
    /// Whether any format needs file contents.
    pub fn needs_content(&self) -> bool {
        self.patch || self.numstat || self.stat || self.shortstat
    }

    /// Any output at all.
    pub fn any(&self) -> bool {
        self.raw
            || self.patch
            || self.name_only
            || self.name_status
            || self.numstat
            || self.stat
            || self.shortstat
            || self.summary
    }
}

/// An option error: text for stderr and the exit code.
pub fn usage_exit(text: String) -> Die {
    let _ = std::io::Write::write_all(&mut std::io::stderr(), text.as_bytes());
    Die::Exit(rgit_options::EXIT_USAGE)
}

fn number(value: &str, option: &str) -> Result<usize, Die> {
    value
        .parse::<usize>()
        .map_err(|_| usage_exit(msg::expects_numerical(option)))
}

impl DiffOpts {
    fn format(&mut self) -> &mut Self {
        self.no_output = false;
        self
    }

    /// Parse one argument. Returns `Ok(false)` when it is not a diff option.
    pub fn parse_arg(&mut self, arg: &str) -> Result<bool, Die> {
        match arg {
            "-p" | "-u" | "--patch" => self.format().patch = true,
            "-s" | "--no-patch" => {
                *self = DiffOpts {
                    no_output: true,
                    raw: false,
                    patch: false,
                    name_only: false,
                    name_status: false,
                    numstat: false,
                    stat: false,
                    shortstat: false,
                    summary: false,
                    ..std::mem::take(self)
                };
            }
            "--raw" => self.format().raw = true,
            "--patch-with-raw" => {
                self.format().raw = true;
                self.patch = true;
            }
            "--patch-with-stat" => {
                self.format().stat = true;
                self.patch = true;
            }
            "--numstat" => self.format().numstat = true,
            "--shortstat" => self.format().shortstat = true,
            "--summary" => self.format().summary = true,
            "--name-only" => self.name_only = true,
            "--name-status" => self.name_status = true,
            "-z" => self.nul = true,
            "-r" => self.recursive = true,
            "-t" => self.show_trees = true,
            "-R" => self.reverse = true,
            "--exit-code" => self.exit_code = true,
            "--no-exit-code" => self.exit_code = false,
            "--quiet" => self.quiet = true,
            "--full-index" => self.full_index = true,
            "--abbrev" => self.abbrev = Some(None),
            "--no-abbrev" => self.abbrev = Some(Some(0)),
            "--minimal" => self.set_algorithm(rgit_diff::Algorithm::Myers, true),
            "--no-minimal" => self.minimal = Some(false),
            "--patience" => self.set_algorithm(rgit_diff::Algorithm::Patience, false),
            "--histogram" => self.set_algorithm(rgit_diff::Algorithm::Histogram, false),
            "--indent-heuristic" => self.indent_heuristic = Some(true),
            "--no-indent-heuristic" => self.indent_heuristic = Some(false),
            "--no-prefix" => self.no_prefix = true,
            "--default-prefix" => self.default_prefix = true,
            "--no-ext-diff" | "--no-textconv" | "--myers" | "--default" => {}
            "--no-renames" => self.detect = Some(Detect::Off),
            "-M" | "--find-renames" => self.detect = Some(Detect::Renames),
            "-C" | "--find-copies" => {
                if self.detect == Some(Detect::Copies) {
                    self.copies_harder = true;
                }
                self.detect = Some(Detect::Copies);
            }
            "--find-copies-harder" => self.copies_harder = true,
            "-B" | "--break-rewrites" => {
                self.break_rewrites =
                    Some((diffcore::DEFAULT_BREAK_SCORE, diffcore::DEFAULT_MERGE_SCORE));
            }
            "--rename-empty" => self.rename_empty = Some(true),
            "--no-rename-empty" => self.rename_empty = Some(false),
            "--pickaxe-all" => self.pickaxe_all = true,
            "--pickaxe-regex" => self.pickaxe_regex = true,
            "-D" | "--irreversible-delete" => self.irreversible_delete = true,
            _ => {
                if let Some(c) = crate::color::parse_arg(arg) {
                    self.color = Some(c?);
                    return Ok(true);
                }
                return self.parse_valued(arg);
            }
        }
        Ok(true)
    }

    /// An algorithm option; it also ends earlier `--anchored` options.
    fn set_algorithm(&mut self, algorithm: rgit_diff::Algorithm, minimal: bool) {
        self.algorithm = Some(algorithm);
        self.minimal = Some(minimal);
        self.anchors.clear();
    }

    fn parse_valued(&mut self, arg: &str) -> Result<bool, Die> {
        if let Some(v) = arg.strip_prefix("--diff-algorithm=") {
            let (algorithm, minimal) = rgit_diff::Algorithm::parse(v)
                .ok_or_else(|| usage_exit(msg::BAD_DIFF_ALGORITHM.to_string()))?;
            // Unlike `--patience`, this keeps earlier anchors (they apply
            // when the algorithm is patience).
            self.algorithm = Some(algorithm);
            self.minimal = Some(minimal);
        } else if let Some(v) = arg.strip_prefix("--anchored=") {
            self.algorithm = Some(rgit_diff::Algorithm::Patience);
            self.minimal = Some(false);
            self.anchors.push(v.as_bytes().to_vec());
        } else if let Some(v) = arg.strip_prefix("--abbrev=") {
            let n = v
                .parse::<i64>()
                .map_err(|_| usage_exit(msg::expects_numerical("--abbrev")))?;
            self.abbrev = Some(Some(usize::try_from(n.max(0)).unwrap_or(0)));
        } else if let Some(v) = arg.strip_prefix("--unified=") {
            self.context = Some(number(v, "--unified")?);
            self.format().patch = true;
        } else if let Some(v) = arg.strip_prefix("-U") {
            if !v.is_empty() {
                self.context = Some(number(v, "--unified")?);
            }
            self.format().patch = true;
        } else if arg == "--unified" {
            self.format().patch = true;
        } else if let Some(v) = arg.strip_prefix("--inter-hunk-context=") {
            self.interhunk = Some(number(v, "--inter-hunk-context")?);
        } else if let Some(v) = arg.strip_prefix("--src-prefix=") {
            self.src_prefix = Some(v.as_bytes().to_vec());
        } else if let Some(v) = arg.strip_prefix("--dst-prefix=") {
            self.dst_prefix = Some(v.as_bytes().to_vec());
        } else if arg == "--stat" {
            self.format().stat = true;
        } else if let Some(v) = arg.strip_prefix("--stat=") {
            let mut parts = v.splitn(3, ',');
            let bad = || usage_exit(msg::invalid_stat_value(v));
            let mut field = |slot: &mut Option<usize>| -> Result<(), Die> {
                if let Some(p) = parts.next() {
                    *slot = Some(p.parse::<usize>().map_err(|_| bad())?);
                }
                Ok(())
            };
            field(&mut self.stat_width)?;
            field(&mut self.stat_name_width)?;
            field(&mut self.stat_count)?;
            self.format().stat = true;
        } else if let Some(v) = arg.strip_prefix("--stat-width=") {
            self.stat_width = Some(number(v, "--stat-width")?);
            self.format().stat = true;
        } else if let Some(v) = arg.strip_prefix("--stat-name-width=") {
            self.stat_name_width = Some(number(v, "--stat-name-width")?);
            self.format().stat = true;
        } else if let Some(v) = arg.strip_prefix("--stat-graph-width=") {
            self.stat_graph_width = Some(number(v, "--stat-graph-width")?);
            self.format().stat = true;
        } else if let Some(v) = arg.strip_prefix("--stat-count=") {
            self.stat_count = Some(number(v, "--stat-count")?);
            self.format().stat = true;
        } else if let Some(v) = arg
            .strip_prefix("--find-renames=")
            .or_else(|| arg.strip_prefix("-M"))
        {
            self.rename_score = Some(score_value(v, "find-renames")?);
            self.detect = Some(Detect::Renames);
        } else if let Some(v) = arg
            .strip_prefix("--find-copies=")
            .or_else(|| arg.strip_prefix("-C"))
        {
            self.rename_score = Some(score_value(v, "find-copies")?);
            if self.detect == Some(Detect::Copies) {
                self.copies_harder = true;
            }
            self.detect = Some(Detect::Copies);
        } else if let Some(v) = arg
            .strip_prefix("--break-rewrites=")
            .or_else(|| arg.strip_prefix("-B"))
        {
            self.break_rewrites = Some(break_value(v)?);
        } else if let Some(v) = arg.strip_prefix("-l") {
            self.rename_limit =
                Some(magnitude(v).ok_or_else(|| usage_exit(msg::switch_expects_integer('l')))?);
        } else if let Some(v) = arg.strip_prefix("-S") {
            self.pickaxe = Some((false, v.to_string()));
        } else if let Some(v) = arg.strip_prefix("-G") {
            self.pickaxe = Some((true, v.to_string()));
        } else if let Some(v) = arg.strip_prefix("--find-object=") {
            self.find_object.push(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--diff-filter=") {
            self.diff_filter
                .parse_into(v)
                .map_err(|c| usage_exit(msg::unknown_change_class(c, v)))?;
        } else {
            return Ok(false);
        }
        Ok(true)
    }

    /// The formats to produce; `default_patch` for `git diff`, raw
    /// otherwise. Fails when exclusive formats were combined.
    pub fn formats(&self, default_patch: bool) -> Result<Formats, Die> {
        let exclusive = [self.name_only, self.name_status, self.no_output]
            .iter()
            .filter(|&&b| b)
            .count();
        if exclusive > 1 {
            return Err(Die::Fatal(msg::EXCLUSIVE_FORMATS.to_string()));
        }
        if self.no_output || self.quiet {
            return Ok(Formats::default());
        }
        if self.name_only {
            return Ok(Formats {
                name_only: true,
                ..Formats::default()
            });
        }
        if self.name_status {
            return Ok(Formats {
                name_status: true,
                ..Formats::default()
            });
        }
        let mut f = Formats {
            raw: self.raw,
            patch: self.patch,
            numstat: self.numstat,
            stat: self.stat,
            shortstat: self.shortstat,
            summary: self.summary,
            ..Formats::default()
        };
        if !f.any() {
            if default_patch {
                f.patch = true;
            } else {
                f.raw = true;
            }
        }
        Ok(f)
    }

    /// Resolve the `--find-object` names.
    pub fn resolve_objects(&mut self, repo: &mut crate::repository::Repo) -> Result<(), Die> {
        self.found_objects.clear();
        for name in &self.find_object {
            let id = repo
                .resolve(name)
                .ok_or_else(|| usage_exit(msg::unable_to_resolve(name)))?;
            self.found_objects.push(id);
        }
        Ok(())
    }

    /// Whether `--exit-code` semantics apply (also implied by `--quiet`).
    pub fn wants_exit_code(&self) -> bool {
        self.exit_code || self.quiet
    }
}
