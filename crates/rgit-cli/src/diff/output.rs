// SPDX-License-Identifier: MIT
//! Writing a list of file pairs in the requested formats
//! (Documentation/diff-format.adoc, diff-generate-patch.adoc): raw,
//! `--name-only`, `--name-status`, `--numstat`, `--stat`, `--shortstat`,
//! `--summary` and patches, in that order.

use std::io::Write;

use rgit_diff::diffcore::{self, Detect, filter::DiffFilter, pickaxe::Criterion};
use rgit_diff::{
    DiffOptions, PatchExtra, PatchFile, PatchOptions, diff_text, is_binary, quote_path,
    write_file_patch_extra,
};
use rgit_hash::{HashKind, ObjectId};
use rgit_object::Object;

use rgit_config::ConfigSet;

use super::collect::{GITLINK_MODE, Pair, Side, TREE_MODE, type_bits};
use super::color::{DiffColors, WsContext};
use super::driver::{DiffAttr, Drivers};
use super::external;
use super::options::SubmoduleFormat;
use super::options::{DiffOpts, Formats, usage_exit};
use super::stat::{self, FileStat, StatKind, StatLayout};
use crate::Die;
use crate::messages::diff as msg;
use crate::repository::Repo;
use rgit_diff::ws_rule::{self, WsRule};

/// A patch with its empty context lines (`" \n"` in a hunk) written as
/// empty lines (`diff.suppressBlankEmpty`).
fn suppress_blank_empty(patch: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(patch.len());
    let mut in_hunk = false;
    for line in patch.split_inclusive(|&b| b == b'\n') {
        if line.starts_with(b"@@") {
            in_hunk = true;
        } else if line.starts_with(b"diff ") {
            in_hunk = false;
        }
        if in_hunk && line == b" \n" {
            out.push(b'\n');
        } else {
            out.extend_from_slice(line);
        }
    }
    out
}

/// An abbreviated object id of a raw line, followed by "..." when
/// `GIT_PRINT_SHA1_ELLIPSIS` is `yes` (git(1), ENVIRONMENT).
pub fn raw_ellipsis(mut id: String, hash: HashKind) -> String {
    if id.len() < hash.hex_len()
        && std::env::var("GIT_PRINT_SHA1_ELLIPSIS").is_ok_and(|v| v.eq_ignore_ascii_case("yes"))
    {
        id.push_str("...");
    }
    id
}

/// Output settings, resolved from options and configuration.
pub struct Output<'a> {
    pub repo: Option<&'a Repo>,
    /// Whether work tree files are converted to their blob form (not with
    /// `--no-index`).
    pub convert: bool,
    pub hash: HashKind,
    pub formats: Formats,
    pub nul: bool,
    pub reverse: bool,
    pub quote_high: bool,
    /// Digits of object ids in raw output; 0 for full ids.
    pub raw_abbrev: usize,
    /// Digits of object ids on `index` lines; 0 for full ids.
    pub index_abbrev: usize,
    pub patch: PatchOptions,
    pub stat: StatLayout,
    /// `git diff`: pairs whose sides turn out to be identical are dropped.
    pub porcelain: bool,
    /// Unmerged paths would need a combined diff in patch output.
    pub combined_unmerged: bool,
    /// Colours, when colouring.
    pub colors: Option<DiffColors>,
    /// `--compact-summary`: creations, deletions and mode changes go into
    /// the stat names ("(new)", "(gone)", "(mode +x)") instead of summary
    /// lines.
    pub compact_summary: bool,
    /// Break, rename and copy detection.
    pub core: diffcore::Options,
    /// `-S` / `-G` / `--find-object`.
    pub pickaxe: Option<Pickaxe>,
    pub pickaxe_all: bool,
    pub filter: DiffFilter,
    pub irreversible_delete: bool,
    /// `diff.suppressBlankEmpty`: empty context lines without the space.
    pub suppress_blank_empty: bool,
    /// `--submodule=<format>` (or `diff.submodule` for the porcelain).
    pub submodule_format: SubmoduleFormat,
    /// The rename limit that would have been needed (warned about after
    /// the output).
    limit_needed: std::cell::Cell<Option<(usize, bool)>>,
    /// `core.whitespace`.
    ws_rule: WsRule,
    /// `--ws-error-highlight` / `diff.wsErrorHighlight`.
    pub ws_highlight: u8,
    /// `--check` found whitespace errors.
    ws_errors: std::cell::Cell<bool>,
    /// `--word-diff` and the word regex (`--word-diff-regex`,
    /// `diff.wordRegex`).
    word_diff: Option<(super::words::Mode, Option<regex::bytes::Regex>)>,
    /// The word regex came from the command line (it beats the diff
    /// driver's `wordRegex`).
    word_regex_cli: bool,
    /// `--line-prefix`.
    pub line_prefix: Option<Vec<u8>>,
    /// The signs of added, removed and context lines
    /// (`--output-indicator-*`).
    pub indicators: [u8; 3],
    /// The patterns of `-O<orderfile>` / `diff.orderFile`.
    pub order: Vec<Vec<u8>>,
    /// `--rotate-to` / `--skip-to` (`true`).
    rotate_to: Option<(bool, Vec<u8>)>,
    /// `--output=<file>`, resolved.
    output: Option<std::path::PathBuf>,
    /// Standard input (`-`) and named pipes of `--no-index`, read once;
    /// they have no object id.
    pub streams: Vec<(std::path::PathBuf, Vec<u8>)>,
    /// `--relative[=<path>]` / `diff.relative`: the directory prefix.
    pub relative: Option<Vec<u8>>,
    /// `--dirstat` parameters (with `diff.dirstat`).
    dirstat: super::dirstat::Params,
    /// `--color-moved` / `diff.colorMoved` and `--color-moved-ws` /
    /// `diff.colorMovedWS`.
    color_moved: (super::moved::Mode, super::moved::MovedWs),
    /// Diff drivers (the `diff` attribute).
    drivers: Drivers,
    /// `--attr-source` / `GIT_ATTR_SOURCE` / `attr.tree`: the tree the
    /// `diff` attribute is read from (see [`crate::attributes::tree_source`]).
    pub attr_tree: Option<Option<ObjectId>>,
    /// Run textconv filters (`--textconv`).
    pub textconv: bool,
    /// External diff programs are allowed (`--ext-diff`): with the
    /// program of `GIT_EXTERNAL_DIFF` / `diff.external`, if any.
    pub external: Option<Option<external::Program>>,
    /// `-a`: no file is binary.
    pub text: bool,
    /// `--quiet`.
    pub quiet: bool,
    /// The command line chose the diff algorithm (it beats the driver's).
    cli_algorithm: bool,
    /// Output was written to `--output` already (by external diff
    /// programs): the rest is appended.
    output_started: std::cell::Cell<bool>,
}

/// `diff.submodule` (an unknown value is warned about and ignored).
fn submodule_format_config(repo: Option<&Repo>) -> SubmoduleFormat {
    let Some(v) = repo.and_then(|r| r.config.get("diff.submodule")) else {
        return SubmoduleFormat::Short;
    };
    let v = v.value.as_ref().map(|v| v.to_string()).unwrap_or_default();
    SubmoduleFormat::parse(&v).unwrap_or_else(|| {
        eprintln!(
            "warning: {}",
            crate::messages::submodule::unknown_diff_submodule(&v)
        );
        SubmoduleFormat::Short
    })
}

/// A compiled pickaxe criterion.
pub enum Pickaxe {
    Literal(Vec<u8>),
    Count(regex::bytes::Regex),
    Grep(regex::bytes::Regex),
    Objects(Vec<ObjectId>),
}

/// Compile a pickaxe pattern (an extended POSIX regular expression).
fn pickaxe_regex(pattern: &str, icase: bool) -> Result<regex::bytes::Regex, Die> {
    let translated = rgit_revwalk::walk::grep::translate(pattern, true)
        .map_err(|reason| Die::Fatal(msg::invalid_regex(pattern, reason)))?;
    regex::bytes::RegexBuilder::new(&translated)
        .unicode(false)
        .case_insensitive(icase)
        .build()
        .map_err(|e| {
            let reason = rgit_revwalk::walk::grep::reason(&e.to_string());
            Die::Fatal(msg::invalid_regex(pattern, reason))
        })
}

/// The diffcore settings: `diff.renames` (porcelain only),
/// `diff.renameLimit` and the command line.
fn core_options(
    repo: Option<&Repo>,
    opts: &DiffOpts,
    porcelain: bool,
) -> Result<diffcore::Options, Die> {
    let mut core = diffcore::Options::default();
    if porcelain {
        if let Some(e) = repo.and_then(|r| r.config.get("diff.renames")) {
            let v = e.value.as_ref().map(|v| v.to_string()).unwrap_or_default();
            core.detect = match v.to_ascii_lowercase().as_str() {
                "copies" | "copy" => Detect::Copies,
                _ => {
                    if e.bool().map_err(|e| Die::Fatal(e.to_string()))? {
                        Detect::Renames
                    } else {
                        Detect::Off
                    }
                }
            };
        } else {
            core.detect = Detect::Renames;
        }
    }
    if let Some(r) = repo {
        if let Some(e) = r.config.get("diff.renamelimit") {
            let n = e
                .int(rgit_config::IntWidth::I32)
                .map_err(|e| Die::Fatal(e.to_string()))?;
            core.rename_limit = usize::try_from(n.max(0)).unwrap_or(0);
        }
    }
    if let Some(d) = opts.detect {
        core.detect = d;
    }
    if opts.copies_harder && opts.detect != Some(Detect::Off) {
        core.detect = Detect::Copies;
    }
    core.find_copies_harder = opts.copies_harder;
    if let Some(s) = opts.rename_score {
        core.rename_score = s;
    }
    core.break_rewrites = opts.break_rewrites;
    if let Some(l) = opts.rename_limit {
        core.rename_limit = l;
    }
    if let Some(e) = opts.rename_empty {
        core.rename_empty = e;
    }
    Ok(core)
}

/// Columns a prefix takes on the terminal (colour sequences take none).
fn visible_width(text: &[u8]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < text.len() {
        if text[i] == 0x1b && text.get(i + 1) == Some(&b'[') {
            i += 2;
            while i < text.len() && !text[i].is_ascii_alphabetic() {
                i += 1;
            }
            i += 1;
            continue;
        }
        n += 1;
        i += 1;
    }
    n
}

/// The width of the terminal for `--stat`: `$COLUMNS`, else 80.
fn terminal_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(80)
}

/// The `--compact-summary` note of a pair, if any.
fn compact_note(old: Option<&Side>, new: Option<&Side>) -> Option<String> {
    let flags = |mode: u32| match mode & 0o170000 {
        0o120000 => " +l",
        _ if mode & 0o111 != 0 => " +x",
        _ => "",
    };
    match (old, new) {
        (None, Some(n)) => Some(format!("new{}", flags(n.mode))),
        (Some(o), None) => Some(format!("gone{}", flags(o.mode))),
        (Some(o), Some(n)) if o.mode != n.mode && (o.mode & 0o170000) == (n.mode & 0o170000) => {
            Some(
                if n.mode & 0o111 != 0 {
                    "mode +x"
                } else {
                    "mode -x"
                }
                .to_owned(),
            )
        }
        _ => None,
    }
}

/// A side with its contents loaded.
struct Loaded {
    mode: u32,
    oid: ObjectId,
    data: Vec<u8>,
    /// The output of the textconv filter, if one ran.
    conv: Option<Vec<u8>>,
    /// The textconv filter failed (fatal when the pair is shown).
    conv_failed: bool,
}

impl Loaded {
    /// The contents shown in patches.
    fn shown(&self) -> &[u8] {
        self.conv.as_deref().unwrap_or(&self.data)
    }
}

/// A coloured patch that waits for --color-moved.
struct Pending {
    rule: WsRule,
    highlight: u8,
    old_data: Vec<u8>,
    new_data: Vec<u8>,
    new_symlink: bool,
    old_symlink: bool,
}

/// The top of the work tree (the current directory) for relative
/// configured paths.
fn r_top(repo: Option<&Repo>) -> std::path::PathBuf {
    repo.and_then(|r| r.setup.work_tree.clone())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
}

/// Whether two loaded sides have the same contents (by id; standard
/// input and pipes have none, their data decides).
fn same_contents(o: &Loaded, n: &Loaded) -> bool {
    if o.oid.is_null() || n.oid.is_null() {
        o.data == n.data
    } else {
        o.oid == n.oid
    }
}

fn is_file_mode(mode: u32) -> bool {
    mode != TREE_MODE && type_bits(mode) != GITLINK_MODE
}

impl<'a> Output<'a> {
    /// Settings from the options and the repository configuration.
    pub fn new(
        repo: Option<&'a Repo>,
        hash: HashKind,
        opts: &DiffOpts,
        formats: Formats,
        porcelain: bool,
    ) -> Result<Output<'a>, Die> {
        let config_bool = |key: &str| repo.and_then(|r| r.config.bool(key).ok().flatten());
        let config_int = |key: &str| {
            repo.and_then(|r| r.config.int(key).ok().flatten())
                .and_then(|v| usize::try_from(v).ok())
        };
        let default_abbrev = repo.map_or(7, |r| r.default_abbrev());
        let abbrev_len = |a: Option<usize>| match a {
            None => default_abbrev,
            Some(0) => 0,
            Some(n) => n.max(crate::repository::MIN_ABBREV),
        };
        let raw_abbrev = match opts.abbrev {
            None if porcelain => default_abbrev,
            None => 0,
            Some(a) => abbrev_len(a),
        };
        let index_abbrev = if opts.full_index {
            0
        } else {
            match opts.abbrev {
                Some(Some(n)) if n > 0 => n.max(crate::repository::MIN_ABBREV),
                _ => default_abbrev,
            }
        };
        let mut diff = DiffOptions::default();
        if porcelain {
            // A value that is no integer or is negative is fatal.
            let strict_int = |key: &str| -> Result<Option<usize>, Die> {
                let Some(e) = repo.and_then(|r| r.config.get(key)) else {
                    return Ok(None);
                };
                let n = e
                    .int(rgit_config::IntWidth::I32)
                    .map_err(|err| Die::Fatal(err.to_string()))?;
                usize::try_from(n)
                    .map(Some)
                    .map_err(|_| Die::Fatal(crate::revisions::unable_to_parse(key, e)))
            };
            if let Some(c) = strict_int("diff.context")? {
                diff.context = c;
            }
            if let Some(c) = strict_int("diff.interhunkcontext")? {
                diff.interhunk_context = c;
            }
        }
        if let Some(h) = config_bool("diff.indentheuristic") {
            diff.indent_heuristic = h;
        }
        if let Some(c) = opts.context {
            diff.context = c;
        }
        if let Some(c) = opts.interhunk {
            diff.interhunk_context = c;
        }
        // GIT_DIFF_OPTS=--unified=<n> or -u<n> wins over the command line
        // (Documentation/git.adoc).
        if let Ok(v) = std::env::var("GIT_DIFF_OPTS") {
            let n = v
                .strip_prefix("--unified=")
                .or_else(|| v.strip_prefix("-u"));
            if let Some(n) = n.and_then(|n| n.parse::<usize>().ok()) {
                diff.context = n;
            }
        }
        if let Some(h) = opts.indent_heuristic {
            diff.indent_heuristic = h;
        }
        // diff.algorithm applies to the porcelain commands only; the
        // command line wins (`--minimal` means minimal Myers).
        if porcelain {
            if let Some(e) = repo.and_then(|r| r.config.get("diff.algorithm")) {
                let v = e.value.as_ref().map(|v| v.to_string()).unwrap_or_default();
                let Some((algorithm, minimal)) = rgit_diff::Algorithm::parse(&v) else {
                    crate::messages::print_error(&msg::unknown_config_value("diff.algorithm", &v));
                    return Err(Die::Fatal(crate::revisions::unable_to_parse(
                        "diff.algorithm",
                        e,
                    )));
                };
                diff.algorithm = algorithm;
                diff.minimal = minimal;
            }
        }
        if let Some(a) = opts.algorithm {
            diff.algorithm = a;
        }
        if let Some(m) = opts.minimal {
            diff.minimal = m;
        }
        diff.anchors = opts.anchors.clone();
        diff.whitespace = opts.whitespace;
        diff.function_context = opts.function_context.unwrap_or(false);
        diff.ignore_blank_lines = opts.ignore_blank_lines;
        diff.ignore_regex = rgit_diff::IgnoreRegexes(opts.ignore_regex.clone());
        let quote_high = repo.is_none_or(|r| r.quote_path());
        let mut src = b"a/".to_vec();
        let mut dst = b"b/".to_vec();
        if porcelain && !opts.default_prefix && config_bool("diff.noprefix") == Some(true) {
            src.clear();
            dst.clear();
        } else if porcelain && !opts.default_prefix {
            // diff.srcPrefix / diff.dstPrefix (config/diff.adoc).
            let config_str = |key: &str| {
                repo.and_then(|r| r.config.string(key).ok().flatten())
                    .map(|v| v.to_vec())
            };
            if let Some(p) = config_str("diff.srcprefix") {
                src = p;
            }
            if let Some(p) = config_str("diff.dstprefix") {
                dst = p;
            }
        }
        if opts.no_prefix {
            src.clear();
            dst.clear();
        }
        if let Some(p) = &opts.src_prefix {
            src = p.clone();
        }
        if let Some(p) = &opts.dst_prefix {
            dst = p.clone();
        }
        if opts.reverse {
            std::mem::swap(&mut src, &mut dst);
        }
        // diff.statNameWidth and diff.statGraphWidth apply to the
        // porcelain commands (not format-patch).
        let (stat_name_width, stat_graph_width) = if porcelain {
            (
                config_int("diff.statnamewidth"),
                config_int("diff.statgraphwidth"),
            )
        } else {
            (None, None)
        };
        let core = core_options(repo, opts, porcelain)?;
        // diff.colorMoved / diff.colorMovedWS apply to the porcelain only.
        let config_value = |key: &str| {
            repo.filter(|_| porcelain)
                .and_then(|r| r.config.get(key))
                .map(|e| {
                    (
                        e.value.as_ref().map(|v| v.to_string()).unwrap_or_default(),
                        e,
                    )
                })
        };
        let configured_mode = match config_value("diff.colormoved") {
            Some((v, e)) => match super::moved::Mode::parse(&v, true) {
                Some(m) => Some(m),
                None => {
                    crate::messages::print_error(msg::COLOR_MOVED_MUST_BE);
                    return Err(Die::Fatal(crate::revisions::unable_to_parse(
                        "diff.colormoved",
                        e,
                    )));
                }
            },
            None => None,
        };
        let moved_mode = match opts.color_moved {
            Some(Some(m)) => m,
            Some(None) => configured_mode
                .filter(|m| *m != super::moved::Mode::No)
                .unwrap_or(super::moved::Mode::Zebra),
            None => configured_mode.unwrap_or(super::moved::Mode::No),
        };
        let moved_ws = match opts.color_moved_ws {
            Some(w) => w,
            None => match config_value("diff.colormovedws") {
                Some((v, e)) => match super::moved::MovedWs::parse(&v) {
                    Ok(w) => w,
                    Err(reason) => {
                        crate::messages::print_error(&reason);
                        return Err(Die::Fatal(crate::revisions::unable_to_parse(
                            "diff.colormovedws",
                            e,
                        )));
                    }
                },
                None => super::moved::MovedWs::default(),
            },
        };
        let color_moved = (moved_mode, moved_ws);
        // File names on the command line are relative to the directory the
        // command started in.
        let cwd = repo.map_or_else(
            || std::env::current_dir().unwrap_or_default(),
            |r| r.setup.original_cwd.clone(),
        );
        let orderfile = match &opts.orderfile {
            Some(o) => Some(cwd.join(o)),
            None if porcelain => repo
                .and_then(|r| {
                    r.config
                        .path("diff.orderfile", &rgit_config::SystemEnv::default())
                        .ok()
                })
                .flatten()
                .map(|p| r_top(repo).join(p)),
            None => None,
        };
        let order = match orderfile {
            Some(p) if p != std::path::Path::new("/dev/null") => {
                let data = std::fs::read(&p).map_err(|_| {
                    Die::Fatal(msg::orderfile_failed(
                        &opts
                            .orderfile
                            .clone()
                            .unwrap_or_else(|| p.to_string_lossy().into_owned()),
                    ))
                })?;
                super::order::parse(&data)
            }
            _ => Vec::new(),
        };
        // diff.relative applies to the porcelain and format-patch.
        let relative_on = match &opts.relative {
            Some(r) => r.is_some(),
            None => (porcelain || opts.format_patch) && config_bool("diff.relative") == Some(true),
        };
        let relative = match (&opts.relative, relative_on) {
            (_, false) => None,
            (Some(Some(Some(dir))), true) => Some(dir.as_bytes().to_vec()),
            _ => Some(repo.map(|r| r.prefix()).unwrap_or_default()),
        }
        .filter(|p| !p.is_empty());
        let mut dirstat = super::dirstat::Params::default();
        if formats.dirstat {
            if let Some(e) = repo.and_then(|r| r.config.get("diff.dirstat")) {
                let v = e.value.as_ref().map(|v| v.to_string()).unwrap_or_default();
                let errors = dirstat.apply(&v);
                if !errors.is_empty() {
                    eprint!("{}", msg::dirstat_config_errors(&errors.concat()));
                }
            }
            for v in opts.dirstat.iter().flatten() {
                dirstat.apply(v);
            }
        }
        let word_diff = match opts.word_diff {
            None => None,
            Some(mode) => {
                let regex = match &opts.word_regex {
                    Some(r) => Some(r.clone()),
                    None => match repo.and_then(|r| r.config.get("diff.wordregex")) {
                        Some(e) => {
                            let v = e.value.as_ref().map(|v| v.to_string()).unwrap_or_default();
                            Some(super::options::word_regex(&v)?)
                        }
                        None => None,
                    },
                };
                Some((mode, regex))
            }
        };
        let ws_rule = match repo.and_then(|r| r.config.get("core.whitespace")) {
            Some(e) => {
                let v = e.value.as_ref().map(|v| v.to_string()).unwrap_or_default();
                for w in ws_rule::invalid_tab_widths(&v) {
                    eprintln!("warning: {}", msg::tab_width_out_of_range(w));
                }
                WsRule::parse(&v).map_err(|_| Die::Fatal(msg::WS_RULE_CONFLICT.to_string()))?
            }
            None => WsRule::default(),
        };
        let ws_highlight = match opts.ws_error_highlight {
            Some(k) => k,
            None => match repo.and_then(|r| r.config.get("diff.wserrorhighlight")) {
                Some(e) => {
                    let v = e.value.as_ref().map(|v| v.to_string()).unwrap_or_default();
                    let Some(k) = super::color::parse_highlight(&v) else {
                        crate::messages::print_error(&msg::unknown_config_value(
                            "diff.wserrorhighlight",
                            &v,
                        ));
                        return Err(Die::Fatal(crate::revisions::unable_to_parse(
                            "diff.wserrorhighlight",
                            e,
                        )));
                    };
                    k
                }
                None => super::color::HIGHLIGHT_NEW,
            },
        };
        if opts.pickaxe_both || (opts.pickaxe.is_some() && !opts.find_object.is_empty()) {
            return Err(Die::Fatal(msg::PICKAXE_EXCLUSIVE.to_string()));
        }
        if opts.pickaxe_all && !opts.find_object.is_empty() {
            return Err(Die::Fatal(msg::PICKAXE_ALL_WITH_FIND_OBJECT.to_string()));
        }
        let icase = opts.pickaxe_icase;
        let pickaxe = if !opts.find_object.is_empty() {
            if opts.found_objects.len() != opts.find_object.len() {
                return Err(usage_exit(msg::unable_to_resolve(&opts.find_object[0])));
            }
            Some(Pickaxe::Objects(opts.found_objects.clone()))
        } else {
            match &opts.pickaxe {
                None => None,
                Some((true, _)) if opts.pickaxe_regex => {
                    return Err(Die::Fatal(msg::G_WITH_PICKAXE_REGEX.to_string()));
                }
                Some((true, p)) => Some(Pickaxe::Grep(pickaxe_regex(p, icase)?)),
                Some((false, p)) if opts.pickaxe_regex => {
                    Some(Pickaxe::Count(pickaxe_regex(p, icase)?))
                }
                // A literal string ignoring case: the escaped pattern.
                Some((false, p)) if icase => Some(Pickaxe::Count(
                    regex::bytes::RegexBuilder::new(&regex::escape(p))
                        .unicode(false)
                        .case_insensitive(true)
                        .build()
                        .map_err(|e| Die::Fatal(msg::invalid_regex(p, &e.to_string())))?,
                )),
                Some((false, p)) => Some(Pickaxe::Literal(p.as_bytes().to_vec())),
            }
        };
        Ok(Output {
            core,
            pickaxe,
            pickaxe_all: opts.pickaxe_all,
            filter: opts.diff_filter.clone(),
            irreversible_delete: opts.irreversible_delete,
            suppress_blank_empty: config_bool("diff.suppressblankempty") == Some(true),
            submodule_format: match opts.submodule_format {
                Some(f) => f,
                None if porcelain => submodule_format_config(repo),
                None => SubmoduleFormat::Short,
            },
            limit_needed: std::cell::Cell::new(None),
            ws_rule,
            ws_highlight,
            ws_errors: std::cell::Cell::new(false),
            word_diff,
            word_regex_cli: opts.word_regex.is_some(),
            color_moved,
            dirstat,
            relative,
            streams: Vec::new(),
            line_prefix: opts.line_prefix.clone(),
            indicators: [
                opts.indicators[0].unwrap_or(b'+'),
                opts.indicators[1].unwrap_or(b'-'),
                opts.indicators[2].unwrap_or(b' '),
            ],
            order,
            rotate_to: opts
                .rotate_to
                .as_ref()
                .map(|(s, p)| (*s, p.as_bytes().to_vec())),
            output: opts.output.as_ref().map(|o| cwd.join(o)),
            drivers: Drivers::default(),
            attr_tree: None,
            textconv: opts.textconv.unwrap_or(false),
            external: opts
                .ext_diff
                .unwrap_or(false)
                .then(|| external::global(repo.map(|r| &r.config))),
            text: opts.text,
            quiet: opts.quiet,
            cli_algorithm: opts.algorithm.is_some() || opts.minimal.is_some(),
            output_started: std::cell::Cell::new(false),
            repo,
            convert: repo.is_some(),
            hash,
            formats,
            nul: opts.nul,
            reverse: opts.reverse,
            quote_high,
            raw_abbrev,
            index_abbrev,
            patch: PatchOptions {
                diff,
                abbrev: index_abbrev,
                quote_path: quote_high,
                src_prefix: src,
                dst_prefix: dst,
                text: opts.text,
                binary: opts.binary,
            },
            stat: StatLayout {
                // The terminal width (porcelain) or 80, less the
                // --line-prefix.
                width: opts.stat_width.unwrap_or_else(|| {
                    let w = if porcelain { terminal_width() } else { 80 };
                    let prefix = opts.line_prefix.as_deref().map_or(0, visible_width);
                    w.saturating_sub(prefix).max(1)
                }),
                name_width: opts.stat_name_width.or(stat_name_width),
                graph_width: opts.stat_graph_width.or(stat_graph_width),
                count: opts.stat_count,
            },
            porcelain,
            combined_unmerged: false,
            compact_summary: opts.compact_summary,
            colors: None,
        })
    }

    /// Decide about colours: `--color` (`opts.color`), and for `git diff`
    /// (`porcelain`) also `color.diff` and `color.ui`.
    pub fn set_color(&mut self, opts: &DiffOpts) -> Result<(), Die> {
        let empty = ConfigSet::new();
        let config = self.repo.map_or(&empty, |r| &r.config);
        let on = if self.porcelain {
            // `diff.color` is the old name of `color.diff`.
            let var = if config.get("color.diff").is_none() && config.get("diff.color").is_some() {
                "diff.color"
            } else {
                "color.diff"
            };
            crate::color::want(config, Some(var), opts.color)?
        } else {
            match opts.color {
                Some(c) => crate::color::want(config, None, Some(c))?,
                None => false,
            }
        };
        self.colors = if on {
            Some(DiffColors::from_config(config)?)
        } else {
            None
        };
        Ok(())
    }

    /// Set the mnemonic prefixes (`diff.mnemonicPrefix`) for `git diff`.
    pub fn mnemonic(&mut self, opts: &DiffOpts, src: &str, dst: &str) {
        let on = self
            .repo
            .and_then(|r| r.config.bool("diff.mnemonicprefix").ok().flatten())
            == Some(true);
        if !on
            || opts.no_prefix
            || opts.default_prefix
            || opts.src_prefix.is_some()
            || opts.dst_prefix.is_some()
        {
            return;
        }
        if self.patch.src_prefix.is_empty() {
            return;
        }
        let (mut s, mut d) = (
            format!("{src}/").into_bytes(),
            format!("{dst}/").into_bytes(),
        );
        if self.reverse {
            std::mem::swap(&mut s, &mut d);
        }
        self.patch.src_prefix = s;
        self.patch.dst_prefix = d;
    }

    fn abbrev(&self, id: &ObjectId, len: usize) -> String {
        if len == 0 {
            return id.to_hex();
        }
        if id.is_null() {
            return "0".repeat(len.min(id.to_hex().len()));
        }
        match self.repo {
            Some(r) => r.abbrev(id, len),
            None => id.to_hex_with_len(len),
        }
    }

    fn read_file(&self, side: &Side) -> Result<Vec<u8>, Die> {
        let path = side.file.as_ref().expect("file side");
        if let Some((_, data)) = self.streams.iter().find(|(p, _)| p == path) {
            return Ok(data.clone());
        }
        let res = if type_bits(side.mode) == 0o120000 {
            std::fs::read_link(path).map(|t| t.into_os_string().into_encoded_bytes())
        } else {
            std::fs::read(path)
        };
        res.map_err(|_| Die::Error(msg::read_failed(&path.to_string_lossy()), 1))
    }

    /// The contents of `side` at `path` (work tree files in their blob
    /// form).
    pub fn contents(&self, side: &Side, path: &[u8]) -> Result<Vec<u8>, Die> {
        self.load(side, path).map(|l| l.data)
    }

    fn load(&self, side: &Side, path: &[u8]) -> Result<Loaded, Die> {
        if let (Some(dir), GITLINK_MODE) = (&side.file, type_bits(side.mode)) {
            // A submodule in the work tree: the commit checked out there.
            let oid = crate::repository::submodule_head(dir, self.hash).unwrap_or(side.oid);
            return Ok(Loaded {
                mode: side.mode,
                oid,
                data: format!("Subproject commit {}\n", oid.to_hex()).into_bytes(),
                conv: None,
                conv_failed: false,
            });
        }
        if side.file.is_some() {
            let mut data = self.read_file(side)?;
            if let (Some(repo), true) = (self.repo, self.convert && is_file_mode(side.mode)) {
                let conv = repo.converter()?;
                let opts = rgit_convert::Options::inspect(conv.config());
                data = conv.to_git(path, data, opts)?;
            }
            // Standard input and pipes have no object id.
            let stream = side
                .file
                .as_ref()
                .is_some_and(|f| self.streams.iter().any(|(p, _)| p == f));
            let oid = if stream {
                ObjectId::null(self.hash)
            } else {
                rgit_hash::hash_object(self.hash, "blob", &data)
                    .map_err(|e| Die::Fatal(e.to_string()))?
            };
            return Ok(Loaded {
                mode: side.mode,
                oid,
                data,
                conv: None,
                conv_failed: false,
            });
        }
        let data = if is_file_mode(side.mode) {
            match self.repo.map(|r| r.read_object(&side.oid)) {
                Some(Some(Object::Blob(b))) => b.data,
                // A missing object (e.g. the null id in a bogus tree).
                Some(None) => return Err(Die::Fatal(msg::unable_to_read_blob(&side.oid.to_hex()))),
                _ => Vec::new(),
            }
        } else if type_bits(side.mode) == GITLINK_MODE {
            format!("Subproject commit {}\n", side.oid.to_hex()).into_bytes()
        } else {
            Vec::new()
        };
        Ok(Loaded {
            mode: side.mode,
            oid: side.oid,
            data,
            conv: None,
            conv_failed: false,
        })
    }

    fn write_path(&self, out: &mut Vec<u8>, path: &[u8]) {
        if self.nul {
            out.extend_from_slice(path);
            out.push(0);
        } else {
            quote_path(out, path, self.quote_high);
            out.push(b'\n');
        }
    }

    /// The status letter (with the score of renames, copies and
    /// rewrites) and the path(s) of a raw or `--name-status` line.
    fn write_status(&self, out: &mut Vec<u8>, p: &Pair) {
        out.push(p.status());
        if p.is_rename() || p.is_rewrite() {
            write!(out, "{:03}", p.percent()).unwrap();
        }
        out.push(if self.nul { 0 } else { b'\t' });
        if p.is_rename() {
            if self.nul {
                out.extend_from_slice(&p.old_path);
                out.push(0);
            } else {
                quote_path(out, &p.old_path, self.quote_high);
                out.push(b'\t');
            }
        }
        self.write_path(out, p.shown_path());
    }

    fn display_name(&self, p: &Pair) -> Vec<u8> {
        let q = |path: &[u8]| {
            let mut o = Vec::new();
            quote_path(&mut o, path, self.quote_high);
            o
        };
        if p.old_path == p.path {
            return q(&p.path);
        }
        let (a, b) = (&p.old_path, &p.path);
        let (pre, suf) = crate::tree_changes::rename_split(a, b);
        let mut out = Vec::new();
        if pre > 0 || suf > 0 {
            let mid = |x: &[u8]| q(&x[pre.min(x.len() - suf)..x.len() - suf]);
            out.extend_from_slice(&q(&a[..pre]));
            out.push(b'{');
            out.extend_from_slice(&mid(a));
            out.extend_from_slice(b" => ");
            out.extend_from_slice(&mid(b));
            out.push(b'}');
            out.extend_from_slice(&q(&a[a.len() - suf..]));
        } else {
            out.extend_from_slice(&q(a));
            out.extend_from_slice(b" => ");
            out.extend_from_slice(&q(b));
        }
        out
    }

    /// The contents of a side for diffcore (unreadable contents are empty;
    /// the output reports the error later).
    fn core_contents(&self, side: &diffcore::Side<(usize, Option<std::path::PathBuf>)>) -> Vec<u8> {
        let s = Side {
            mode: side.mode,
            oid: side.oid,
            file: side.extra.1.clone(),
        };
        self.load(&s, &side.path)
            .map(|l| l.data)
            .unwrap_or_default()
    }

    /// Run the diffcore transformations (break, rename/copy detection,
    /// pickaxe, `--diff-filter`) on the pairs.
    pub fn diffcore(&self, pairs: Vec<Pair>) -> Result<Vec<Pair>, Die> {
        self.diffcore_with(pairs, true)
    }

    /// Whether a pickaxe (`-S`, `-G`, `--find-object`) is set.
    pub fn has_pickaxe(&self) -> bool {
        self.pickaxe.is_some()
    }

    /// [`Output::diffcore`], with the pickaxe only when `pickaxe`.
    pub fn diffcore_with(&self, pairs: Vec<Pair>, pickaxe: bool) -> Result<Vec<Pair>, Die> {
        let pick = self.pickaxe.as_ref().filter(|_| pickaxe);
        if !self.core.active() && pick.is_none() && !self.filter.is_active() {
            return Ok(pairs);
        }
        type Extra = (usize, Option<std::path::PathBuf>);
        let side = |i: usize, path: &[u8], s: &Option<Side>| {
            s.as_ref().map(|s| diffcore::Side {
                path: path.to_vec(),
                mode: s.mode,
                oid: s.oid,
                extra: (i, s.file.clone()),
            })
        };
        let mut unmerged: Vec<(usize, Pair)> = Vec::new();
        let mut input: Vec<diffcore::FilePair<Extra>> = Vec::new();
        // The command's pairs, to keep their names (`--no-index` shows
        // `/dev/null` for a missing side).
        let mut originals: Vec<Option<Pair>> = Vec::with_capacity(pairs.len());
        // With copy detection the old side of an unmerged path is a copy
        // source like a modified file's; the pair itself is not shown.
        let unmerged_sources = self.core.detect == Detect::Copies;
        let mut source_only = std::collections::HashSet::new();
        for (i, p) in pairs.into_iter().enumerate() {
            if p.unmerged {
                if unmerged_sources && let Some(old) = &p.old {
                    let one = side(i, &p.old_path, &p.old);
                    let two = diffcore::Side {
                        path: p.path.clone(),
                        mode: old.mode,
                        oid: ObjectId::null(self.hash),
                        extra: (i, None),
                    };
                    input.push(diffcore::FilePair::new(one, Some(two)));
                    source_only.insert(i);
                }
                unmerged.push((i, p));
                originals.push(None);
                continue;
            }
            let one = side(i, &p.old_path, &p.old);
            let mut two = side(i, &p.path, &p.new);
            if p.dirty_submodule != 0 {
                // A dirty submodule keeps the recorded commit; diffcore
                // must not drop it as unmodified.
                if let Some(t) = two.as_mut() {
                    t.oid = ObjectId::null(self.hash);
                }
            }
            input.push(diffcore::FilePair::new(one, two));
            originals.push(Some(p));
        }
        let contents = |s: &diffcore::Side<Extra>| self.core_contents(s);
        let (mut result, report) = diffcore::run(input, &self.core, &contents);
        if let Some(n) = report.rename_limit_needed {
            self.limit_needed
                .set(Some((n, report.copies_from_modified_only)));
        }
        let mut pickaxe_hit = true;
        if let Some(how) = pick {
            // With textconv the pickaxe looks at the converted text (and a
            // failing filter is fatal).
            let conv_failed = std::cell::Cell::new(false);
            let converted = |s: &diffcore::Side<Extra>| {
                let data = self.core_contents(s);
                if !self.textconv || !is_file_mode(s.mode) || type_bits(s.mode) == 0o120000 {
                    return data;
                }
                match self
                    .diff_attr(&s.path)
                    .ok()
                    .and_then(|a| a.textconv().map(str::to_owned))
                {
                    Some(cmd) => super::driver::run_textconv(&cmd, &data).unwrap_or_else(|()| {
                        conv_failed.set(true);
                        Vec::new()
                    }),
                    None => data,
                }
            };
            let literal;
            let count_re;
            let grep_re;
            let criterion = match how {
                Pickaxe::Literal(s) => {
                    literal = move |d: &[u8]| diffcore::pickaxe::count_literal(d, s);
                    Criterion::Count(&literal)
                }
                Pickaxe::Count(re) => {
                    count_re = move |d: &[u8]| re.find_iter(d).count();
                    Criterion::Count(&count_re)
                }
                Pickaxe::Grep(re) => {
                    grep_re = move |l: &[u8]| re.is_match(l);
                    Criterion::Grep {
                        matches: &grep_re,
                        // `-a`, and textconv output, count as text.
                        text: self.text || (self.textconv && self.any_textconv()),
                    }
                }
                Pickaxe::Objects(ids) => Criterion::Objects(ids),
            };
            result = diffcore::pickaxe::pickaxe(result, &criterion, self.pickaxe_all, &converted);
            if conv_failed.get() {
                return Err(Die::Fatal(
                    crate::messages::userdiff::UNABLE_TO_READ_FILES.into(),
                ));
            }
            pickaxe_hit = !result.is_empty();
        }
        // Back to the command's pairs, in order (unmerged paths where they
        // were).
        let mut out: Vec<(usize, Pair)> = Vec::with_capacity(result.len() + unmerged.len());
        for fp in result {
            let anchor = fp.two.as_ref().or(fp.one.as_ref()).map_or(0, |s| s.extra.0);
            let back = |s: &Option<diffcore::Side<Extra>>| {
                s.as_ref().map(|s| Side {
                    mode: s.mode,
                    oid: s.oid,
                    file: s.extra.1.clone(),
                })
            };
            let path = fp
                .two
                .as_ref()
                .or(fp.one.as_ref())
                .map(|s| s.path.clone())
                .unwrap_or_default();
            let old_path = fp
                .one
                .as_ref()
                .map_or_else(|| path.clone(), |s| s.path.clone());
            let renamed = match fp.status {
                diffcore::Status::Renamed => Some(b'R'),
                diffcore::Status::Copied => Some(b'C'),
                _ => None,
            };
            if renamed.is_none() && source_only.contains(&anchor) {
                continue;
            }
            if renamed.is_none() {
                if let Some(mut p) = originals.get_mut(anchor).and_then(Option::take) {
                    p.score = fp.score;
                    out.push((anchor, p));
                    continue;
                }
            }
            out.push((
                anchor,
                Pair {
                    old_path,
                    path,
                    old: back(&fp.one),
                    new: back(&fp.two),
                    unmerged: false,
                    renamed,
                    score: fp.score,
                    dirty_submodule: 0,
                },
            ));
        }
        // Unmerged paths never match a pickaxe.
        if pick.is_none() || (self.pickaxe_all && pickaxe_hit) {
            for (i, p) in unmerged {
                let pos = out.partition_point(|(a, _)| *a < i);
                out.insert(pos, (i, p));
            }
        }
        let pairs = out.into_iter().map(|(_, p)| p).collect();
        Ok(self
            .filter
            .apply_by(pairs, |p: &Pair| (p.status(), p.is_rewrite())))
    }

    /// Write everything; returns the output and whether there were changes.
    pub fn render(&self, mut pairs: Vec<Pair>) -> Result<(Vec<u8>, bool), Die> {
        if self.reverse {
            for p in &mut pairs {
                if !p.unmerged {
                    std::mem::swap(&mut p.old, &mut p.new);
                    std::mem::swap(&mut p.old_path, &mut p.path);
                }
            }
        }
        // --no-index: files that are only on one side get their ids
        // (hashed for rename detection) when both added and deleted files
        // make rename detection run.
        let renames_ran = self.core.detect != Detect::Off
            && pairs.iter().any(|p| p.old.is_none())
            && pairs.iter().any(|p| p.new.is_none());
        // --relative: only paths below the directory, shown without it.
        if let Some(prefix) = &self.relative {
            pairs.retain(|p| p.path.starts_with(prefix) || p.old_path.starts_with(prefix));
        }
        let mut pairs = self.diffcore(pairs)?;
        if !self.order.is_empty() {
            pairs.sort_by_cached_key(|p| super::order::rank(&self.order, p.shown_path()));
        }
        if let Some((skip, path)) = &self.rotate_to {
            let Some(at) = pairs.iter().position(|p| p.shown_path() == &path[..]) else {
                return Err(Die::Fatal(msg::no_such_path_in_diff(
                    &String::from_utf8_lossy(path),
                )));
            };
            let mut rest = pairs.split_off(at);
            if !skip {
                rest.append(&mut pairs);
            }
            pairs = rest;
        }
        if self.combined_unmerged && self.formats.patch && pairs.iter().any(|p| p.unmerged) {
            return Err(Die::Fatal(msg::not_supported(
                "combined diff of unmerged paths",
            )));
        }
        let f = self.formats;
        // Load contents where needed.
        // `-w`, `-I` and friends drop pairs whose differences are all
        // ignored, from every format (not `--ignore-blank-lines` alone).
        let d = &self.patch.diff;
        let check = d.whitespace.any() || !d.ignore_regex.0.is_empty();
        let textconv = self.textconv && (f.patch || self.quiet);
        let need = f.needs_content()
            || f.summary
            || check
            || textconv
            || (self.quiet && self.external.is_some());
        let mut loaded: Vec<(Option<Loaded>, Option<Loaded>)> = Vec::with_capacity(pairs.len());
        let mut keep = Vec::with_capacity(pairs.len());
        for p in &pairs {
            let has_file = p.old.iter().chain(p.new.iter()).any(|s| s.file.is_some());
            let load = !p.unmerged && (need || (self.porcelain && has_file));
            let (mut o, mut n) = if load {
                (
                    p.old
                        .as_ref()
                        .map(|s| self.load(s, &p.old_path))
                        .transpose()?,
                    p.new.as_ref().map(|s| self.load(s, &p.path)).transpose()?,
                )
            } else {
                (None, None)
            };
            if textconv && load {
                if let Some(l) = o.as_mut() {
                    self.apply_textconv(&p.old_path, l)?;
                }
                if let Some(l) = n.as_mut() {
                    self.apply_textconv(&p.path, l)?;
                }
            }
            // (Standard input has no id: its contents decide.)
            let same = matches!((&o, &n), (Some(o), Some(n)) if o.mode == n.mode
                && same_contents(o, n));
            let ignored = check
                && !p.is_rename()
                && matches!((&o, &n), (Some(o), Some(n)) if o.mode == n.mode
                    && is_file_mode(o.mode)
                    && !is_binary(&o.data)
                    && !is_binary(&n.data)
                    && diff_text(&o.data, &n.data, d).hunks.is_empty());
            keep.push(
                !(self.porcelain && same && !p.is_rename() && p.dirty_submodule == 0) && !ignored,
            );
            loaded.push((o, n));
        }
        let mut items: Vec<(Pair, Option<Loaded>, Option<Loaded>)> = Vec::new();
        // The full paths (for attributes) before --relative cuts them.
        let mut full_paths: Vec<Vec<u8>> = Vec::new();
        let mut full_old_paths: Vec<Vec<u8>> = Vec::new();
        for ((mut p, (o, n)), k) in pairs.into_iter().zip(loaded).zip(keep) {
            if k {
                full_paths.push(p.path.clone());
                full_old_paths.push(p.old_path.clone());
                if let Some(prefix) = &self.relative {
                    let strip = |path: &mut Vec<u8>| {
                        if let Some(rest) = path.strip_prefix(&prefix[..]) {
                            *path = rest.strip_prefix(b"/").unwrap_or(rest).to_vec();
                        }
                    };
                    strip(&mut p.path);
                    strip(&mut p.old_path);
                }
                items.push((p, o, n));
            }
        }
        // Whether each pair counts as a change: textconv output that is
        // the same on both sides shows no patch and no change.
        let mut item_changed: Vec<bool> = items
            .iter()
            .map(|(p, o, n)| !textconv_equal(p, o, n))
            .collect();
        if self.quiet {
            self.external_quiet(&items, &mut item_changed)?;
        }
        let mut out = Vec::new();

        if f.raw {
            // After the content check git shows the hashed work tree files.
            let hashed = check;
            for (p, o, n) in &items {
                let null = ObjectId::null(self.hash);
                let one_sided = p.old.is_none() || p.new.is_none() || p.is_rename();
                let id = |s: &Side, l: &Option<Loaded>| match l {
                    Some(l) if hashed && s.file.is_some() => l.oid,
                    // --no-index files have no ids in raw output, except
                    // after rename detection.
                    _ if !self.convert && s.file.is_some() && !(one_sided && renames_ran) => null,
                    _ => s.oid,
                };
                let (om, oo) = p.old.as_ref().map_or((0, null), |s| (s.mode, id(s, o)));
                let (nm, no) = p.new.as_ref().map_or((0, null), |s| (s.mode, id(s, n)));
                write!(
                    out,
                    ":{om:06o} {nm:06o} {} {} ",
                    raw_ellipsis(self.abbrev(&oo, self.raw_abbrev), self.hash),
                    raw_ellipsis(self.abbrev(&no, self.raw_abbrev), self.hash)
                )
                .unwrap();
                self.write_status(&mut out, p);
            }
        }
        if f.name_only || f.name_status {
            for (p, _, _) in &items {
                if f.name_status {
                    self.write_status(&mut out, p);
                } else {
                    // The new side's name (they differ for --no-index and
                    // blob pairs).
                    self.write_path(&mut out, &p.path);
                }
            }
        }

        // Pairs with a visible content or mode difference.
        let differs = |p: &Pair, o: &Option<Loaded>, n: &Option<Loaded>| -> bool {
            if p.unmerged || p.is_rename() || p.dirty_submodule != 0 {
                return true;
            }
            !matches!((o, n), (Some(o), Some(n)) if same_contents(o, n) && o.mode == n.mode)
        };
        let mut stats: Vec<FileStat> = Vec::new();
        let mut names: Vec<Vec<u8>> = Vec::new();
        if f.numstat || f.stat || f.shortstat {
            for (p, o, n) in &items {
                if !differs(p, o, n) {
                    continue;
                }
                let is_file = |s: &Option<Side>| s.as_ref().is_none_or(|s| s.mode != TREE_MODE);
                if !p.unmerged && (!is_file(&p.old) || !is_file(&p.new)) {
                    continue;
                }
                let kind = if p.unmerged {
                    StatKind::Unmerged
                } else {
                    let od = o.as_ref().map_or(&[][..], |l| &l.data[..]);
                    let nd = n.as_ref().map_or(&[][..], |l| &l.data[..]);
                    let same = matches!((o, n), (Some(o), Some(n)) if same_contents(o, n));
                    let binary = self.pair_binary(p, o.as_ref(), n.as_ref(), false)?;
                    if binary && same {
                        StatKind::BinarySame
                    } else if binary {
                        StatKind::Binary {
                            old_size: od.len(),
                            new_size: nd.len(),
                        }
                    } else {
                        let same = matches!((o, n), (Some(o), Some(n)) if same_contents(o, n));
                        if p.is_rewrite() {
                            StatKind::Text {
                                added: rgit_diff::split_lines(nd).len(),
                                deleted: rgit_diff::split_lines(od).len(),
                            }
                        } else if same {
                            StatKind::Text {
                                added: 0,
                                deleted: 0,
                            }
                        } else {
                            let d = diff_text(od, nd, &self.patch.diff);
                            StatKind::Text {
                                added: d.stats.insertions,
                                deleted: d.stats.deletions,
                            }
                        }
                    }
                };
                // Only ignored differences (`-w`, `--ignore-blank-lines`).
                let same_mode = matches!((&p.old, &p.new), (Some(a), Some(b)) if a.mode == b.mode);
                if matches!(
                    kind,
                    StatKind::Text {
                        added: 0,
                        deleted: 0
                    }
                ) && same_mode
                    && !p.is_rename()
                    && self.patch.diff.ignores_changes()
                {
                    continue;
                }
                let mut name = self.display_name(p);
                names.push(if self.nul && p.is_rename() {
                    let mut n = vec![0];
                    n.extend_from_slice(&p.old_path);
                    n.push(0);
                    n.extend_from_slice(&p.path);
                    n
                } else if self.nul {
                    p.path.clone()
                } else {
                    name.clone()
                });
                if self.compact_summary {
                    if let Some(note) = compact_note(p.old.as_ref(), p.new.as_ref()) {
                        name.extend_from_slice(format!(" ({note})").as_bytes());
                    }
                }
                stats.push(FileStat { name, kind });
            }
        }
        if f.numstat {
            stat::write_numstat(&mut out, &stats, &names, self.nul);
        }
        if f.stat {
            let colors = self
                .colors
                .as_ref()
                .map(|c| (c.new.as_str(), c.old.as_str()));
            stat::write_stat(&mut out, &stats, &self.stat, colors);
        } else if f.shortstat && !stats.is_empty() {
            stat::write_summary_line(&mut out, &stats);
        }
        if f.dirstat {
            let mut files: Vec<(Vec<u8>, u64)> = Vec::new();
            for (p, o, n) in &items {
                if p.unmerged || !differs(p, o, n) {
                    continue;
                }
                let od = o.as_ref().map_or(&[][..], |l| &l.data[..]);
                let nd = n.as_ref().map_or(&[][..], |l| &l.data[..]);
                let same = matches!((o, n), (Some(o), Some(n)) if same_contents(o, n));
                let dmg = if same {
                    0
                } else {
                    let lines = if is_binary(od) || is_binary(nd) {
                        None
                    } else if self.dirstat.measure == super::dirstat::Measure::Lines {
                        let d = diff_text(od, nd, &self.patch.diff);
                        Some((d.stats.insertions, d.stats.deletions))
                    } else {
                        Some((0, 0))
                    };
                    super::dirstat::damage(self.dirstat.measure, od, nd, lines)
                };
                files.push((p.shown_path().to_vec(), dmg));
            }
            super::dirstat::write(&mut out, &mut files, &self.dirstat);
        }
        if f.summary && !self.compact_summary {
            for (k, (p, o, n)) in items.iter().enumerate() {
                if p.unmerged || !differs(p, o, n) {
                    continue;
                }
                // (--relative does not cut the names here.)
                let mut full = p.clone();
                full.path = full_paths[k].clone();
                full.old_path = full_old_paths[k].clone();
                let name = self.display_name(&full);
                if let Some(c) = p.renamed {
                    let word = if c == b'C' { "copy" } else { "rename" };
                    write!(out, " {word} ").unwrap();
                    out.extend_from_slice(&name);
                    writeln!(out, " ({}%)", p.percent()).unwrap();
                } else if p.is_rewrite() {
                    out.extend_from_slice(b" rewrite ");
                    out.extend_from_slice(&name);
                    writeln!(out, " ({}%)", p.percent()).unwrap();
                }
                let mut line = |what: &str, mode: u32| {
                    write!(out, " {what} mode {mode:06o} ").unwrap();
                    out.extend_from_slice(&name);
                    out.push(b'\n');
                };
                match (&p.old, &p.new) {
                    (None, Some(s)) => line("create", s.mode),
                    (Some(s), None) => line("delete", s.mode),
                    (Some(a), Some(b)) if a.mode != b.mode && p.is_rename() => {
                        writeln!(out, " mode change {:06o} => {:06o}", a.mode, b.mode).unwrap();
                    }
                    (Some(a), Some(b)) if a.mode != b.mode => {
                        write!(out, " mode change {:06o} => {:06o} ", a.mode, b.mode).unwrap();
                        out.extend_from_slice(&name);
                        out.push(b'\n');
                    }
                    _ => {}
                }
            }
        }
        if f.patch || f.check {
            if f.patch
                && !items.is_empty()
                && (f.raw || f.numstat || f.stat || f.shortstat || f.summary)
            {
                out.push(b'\n');
            }
            let abbrev = |id: &ObjectId| self.abbrev(id, self.index_abbrev);
            // The whitespace rules of each path (`core.whitespace`, the
            // `whitespace` attribute), for colours and `--check`.
            let need_rules = f.check || self.colors.is_some();
            let need_driver = self.word_diff.is_some() && !self.word_regex_cli;
            let base_rule = self.ws_rule;
            let index = match (need_rules || need_driver, self.repo) {
                (true, Some(r)) => r.read_index().ok().map(|(i, _)| i),
                _ => None,
            };
            let mut attrs = match (self.repo, &index) {
                (Some(r), Some(i)) => Some(crate::attributes::open(r, i, None, false)),
                _ => None,
            };
            let ws_id = attrs.as_mut().map(|a| a.id(b"whitespace"));
            let marker_id = attrs.as_mut().map(|a| a.id(b"conflict-marker-size"));
            let diff_id = attrs.as_mut().map(|a| a.id(b"diff"));
            // `diff.<driver>.wordRegex` of the path's `diff` attribute.
            let mut driver_regex = |path: &[u8]| -> Result<Option<regex::bytes::Regex>, Die> {
                let (Some(a), Some(id), Some(repo)) = (attrs.as_mut(), diff_id, self.repo) else {
                    return Ok(None);
                };
                let rgit_attr::State::Value(drv) = a.lookup(path, false).get(id).clone() else {
                    return Ok(None);
                };
                let attr = self
                    .drivers
                    .resolve(&repo.config, &rgit_attr::State::Value(drv))?;
                Ok(attr.driver().and_then(|d| d.word_regex.clone()))
            };
            let mut driver_regexes = Vec::new();
            if need_driver {
                for path in &full_paths {
                    driver_regexes.push(driver_regex(path)?);
                }
            }
            // The whitespace rule and the `conflict-marker-size` (for
            // `--check`) of a path.
            let mut rule_for = |path: &[u8]| -> Result<(WsRule, usize), Die> {
                let (Some(a), Some(id), Some(marker_id)) = (attrs.as_mut(), ws_id, marker_id)
                else {
                    return Ok((base_rule, super::check::DEFAULT_MARKER_SIZE));
                };
                let found = a.lookup(path, false);
                let marker_size = match found.get(marker_id) {
                    rgit_attr::State::Value(v) => std::str::from_utf8(v)
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(super::check::DEFAULT_MARKER_SIZE),
                    _ => super::check::DEFAULT_MARKER_SIZE,
                };
                let rule = match found.get(id).clone() {
                    rgit_attr::State::Set => Ok(WsRule {
                        bits: ws_rule::ATTRIBUTE_SET_RULES,
                        tab_width: base_rule.tab_width,
                    }),
                    rgit_attr::State::Unset => Ok(WsRule::none(base_rule.tab_width)),
                    rgit_attr::State::Value(v) => WsRule::parse(&String::from_utf8_lossy(&v))
                        .map_err(|_| Die::Fatal(msg::WS_RULE_CONFLICT.to_string())),
                    rgit_attr::State::Unspecified => Ok(base_rule),
                };
                Ok((rule?, marker_size))
            };
            // With --color-moved the coloured patches wait until all
            // patches are known: (offset in `out`, plain patch, settings).
            let defer_moved = self.colors.is_some() && self.color_moved.0 != super::moved::Mode::No;
            // (The contents are copied: a dirty submodule's text lives
            // only for one pair.)
            let mut pending: Vec<(usize, Vec<u8>, Pending)> = Vec::new();
            for (k, (p, o, n)) in items.iter().enumerate() {
                let idx = k;
                let attr = if p.unmerged || self.external.is_some() || o.is_some() || n.is_some() {
                    self.diff_attr(&p.old_path)?
                } else {
                    DiffAttr::Unspecified
                };
                if let Some(prog) = self.external_program(&attr) {
                    // The program writes to our output: what we have so
                    // far goes first (patches waiting for --color-moved
                    // keep their place).
                    if pending.is_empty() {
                        self.write_out(&out);
                        out.clear();
                    }
                    item_changed[idx] =
                        self.run_external(&prog, p, o, n, idx, items.len(), false)?;
                    continue;
                }
                if !item_changed[idx] {
                    continue;
                }
                if o.iter().chain(n.iter()).any(|l| l.conv_failed) {
                    self.write_out(&out);
                    return Err(Die::Fatal(
                        crate::messages::userdiff::UNABLE_TO_READ_FILES.into(),
                    ));
                }
                if p.unmerged {
                    out.extend_from_slice(msg::UNMERGED_PATH.as_bytes());
                    let mut q = Vec::new();
                    quote_path(&mut q, &p.path, self.quote_high);
                    out.extend_from_slice(&q);
                    out.push(b'\n');
                    continue;
                }
                if !differs(p, o, n) {
                    continue;
                }
                let is_file = |s: &Option<Side>| s.as_ref().is_none_or(|s| s.mode != TREE_MODE);
                if !is_file(&p.old) || !is_file(&p.new) {
                    continue;
                }
                let dirty_data;
                let n = match n {
                    Some(l) if p.dirty_submodule != 0 => {
                        let mut d = l.data.clone();
                        if d.last() == Some(&b'\n') {
                            d.pop();
                        }
                        d.extend_from_slice(b"-dirty\n");
                        dirty_data = Loaded {
                            mode: l.mode,
                            oid: l.oid,
                            data: d,
                            conv: None,
                            conv_failed: false,
                        };
                        Some(&dirty_data)
                    }
                    n => n.as_ref(),
                };
                let of = o.as_ref().map(|l| PatchFile {
                    path: &p.old_path,
                    mode: l.mode,
                    oid: l.oid,
                    data: l.shown(),
                });
                let nf = n.as_ref().map(|l| PatchFile {
                    path: &p.path,
                    mode: l.mode,
                    oid: l.oid,
                    data: l.shown(),
                });
                let new_data = nf.as_ref().map_or(&[][..], |f| f.data);
                let old_data = of.as_ref().map_or(&[][..], |f| f.data);
                let (rule, marker_size) = if need_rules {
                    rule_for(&full_paths[k])?
                } else {
                    (base_rule, super::check::DEFAULT_MARKER_SIZE)
                };
                let extra = PatchExtra {
                    rename: p.renamed.map(|c| (c == b'C', p.percent())),
                    rewrite: p.is_rewrite().then(|| p.percent()),
                    irreversible_delete: self.irreversible_delete,
                    same_id_differs: p.dirty_submodule != 0,
                    binary: Some(self.pair_binary(p, o.as_ref(), n, true)?),
                };
                let pair_opts = self.pair_options(&attr);
                let popts = pair_opts.as_ref().unwrap_or(&self.patch);
                let mut emit = |out: &mut Vec<u8>,
                                old: Option<&PatchFile<'_>>,
                                new: Option<&PatchFile<'_>>| {
                    let mut plain = Vec::new();
                    write_file_patch_extra(&mut plain, old, new, popts, &abbrev, &extra);
                    if self.suppress_blank_empty {
                        plain = suppress_blank_empty(&plain);
                    }
                    let ws = WsContext {
                        rule,
                        highlight: if f.check { 0 } else { self.ws_highlight },
                        old_data: if old.is_some() { old_data } else { &[][..] },
                        new_data: if new.is_some() { new_data } else { &[][..] },
                        new_symlink: new.is_some_and(|f| type_bits(f.mode) == 0o120000),
                        old_symlink: old.is_some_and(|f| type_bits(f.mode) == 0o120000),
                        indicators: self.indicators,
                    };
                    if f.check {
                        let name = String::from_utf8_lossy(&p.path).into_owned();
                        if super::check::check_patch(out, &plain, &ws, &name, marker_size) {
                            self.ws_errors.set(true);
                        }
                    } else if let Some((mode, regex)) = &self.word_diff {
                        let regex = driver_regexes
                            .get(k)
                            .and_then(Option::as_ref)
                            .or(regex.as_ref());
                        let w = super::words::Words {
                            mode: *mode,
                            regex,
                            colors: self.colors.as_ref(),
                            diff: &self.patch.diff,
                        };
                        w.patch(out, &plain);
                    } else if let Some(c) = &self.colors {
                        if defer_moved {
                            let owned = Pending {
                                rule: ws.rule,
                                highlight: ws.highlight,
                                old_data: ws.old_data.to_vec(),
                                new_data: ws.new_data.to_vec(),
                                new_symlink: ws.new_symlink,
                                old_symlink: ws.old_symlink,
                            };
                            pending.push((out.len(), plain, owned));
                        } else {
                            super::color::patch(out, c, &plain, &ws, &[]);
                        }
                    } else {
                        out.extend_from_slice(&self.with_indicators(plain));
                    }
                };
                let link = |f: &Option<PatchFile<'_>>| {
                    f.as_ref()
                        .is_some_and(|f| type_bits(f.mode) == GITLINK_MODE)
                };
                let new_in_worktree =
                    p.new.as_ref().is_some_and(|s| s.file.is_some()) && p.dirty_submodule != 0;
                let summary = |out: &mut Vec<u8>, old: Option<ObjectId>, new: Option<ObjectId>| {
                    if let Some(repo) = self.repo {
                        let dirty = if new.is_some() { p.dirty_submodule } else { 0 };
                        if self.submodule_format == SubmoduleFormat::Diff {
                            let wt = new.is_some() && new_in_worktree;
                            crate::submodules::summary_diff(
                                out,
                                repo,
                                &p.path,
                                old,
                                new,
                                dirty,
                                wt,
                                (&self.patch.src_prefix, &self.patch.dst_prefix),
                            );
                        } else {
                            crate::submodules::summary_log(out, repo, &p.path, old, new, dirty);
                        }
                    }
                };
                let log = self.submodule_format != SubmoduleFormat::Short && self.repo.is_some();
                if log && p.status() == b'T' {
                    // The submodule side as a summary, the file side as a
                    // patch, old side first.
                    if link(&of) {
                        summary(&mut out, of.as_ref().map(|f| f.oid), None);
                    } else {
                        emit(&mut out, of.as_ref(), None);
                    }
                    if link(&nf) {
                        summary(&mut out, None, nf.as_ref().map(|f| f.oid));
                    } else {
                        emit(&mut out, None, nf.as_ref());
                    }
                } else if log && (link(&of) || link(&nf)) {
                    summary(
                        &mut out,
                        of.as_ref().map(|f| f.oid),
                        nf.as_ref().map(|f| f.oid),
                    );
                } else if p.status() == b'T' {
                    emit(&mut out, of.as_ref(), None);
                    emit(&mut out, None, nf.as_ref());
                } else {
                    emit(&mut out, of.as_ref(), nf.as_ref());
                }
            }
            if let (Some(c), false) = (&self.colors, pending.is_empty()) {
                let plains: Vec<Vec<u8>> = pending.iter().map(|(_, p, _)| p.clone()).collect();
                let slots = super::moved::mark(&plains, self.color_moved.0, &self.color_moved.1);
                let rest = std::mem::take(&mut out);
                let mut at = 0;
                for ((offset, plain, w), moved) in pending.iter().zip(&slots) {
                    out.extend_from_slice(&rest[at..*offset]);
                    at = *offset;
                    let ws = WsContext {
                        rule: w.rule,
                        highlight: w.highlight,
                        old_data: &w.old_data,
                        new_data: &w.new_data,
                        new_symlink: w.new_symlink,
                        old_symlink: w.old_symlink,
                        indicators: self.indicators,
                    };
                    super::color::patch(&mut out, c, plain, &ws, moved);
                }
                out.extend_from_slice(&rest[at..]);
            }
        }
        let changed = item_changed.iter().any(|&c| c);
        let out = match &self.line_prefix {
            Some(prefix) if !out.is_empty() => {
                let mut prefixed = Vec::with_capacity(out.len() + prefix.len() * 16);
                for line in out.split_inclusive(|&c| c == b'\n') {
                    prefixed.extend_from_slice(prefix);
                    prefixed.extend_from_slice(line);
                }
                prefixed
            }
            _ => out,
        };
        Ok((out, changed))
    }

    /// `--output-indicator-*`: the signs of the lines of a plain patch.
    fn with_indicators(&self, mut plain: Vec<u8>) -> Vec<u8> {
        if self.indicators == *b"+- " {
            return plain;
        }
        let mut in_header = true;
        let mut at = 0;
        while at < plain.len() {
            let end = plain[at..]
                .iter()
                .position(|&c| c == b'\n')
                .map_or(plain.len(), |p| at + p + 1);
            let first = plain[at];
            if first == b'@' {
                in_header = false;
            } else if !in_header {
                match first {
                    b'+' => plain[at] = self.indicators[0],
                    b'-' => plain[at] = self.indicators[1],
                    b' ' => plain[at] = self.indicators[2],
                    _ => {}
                }
            }
            at = end;
        }
        plain
    }
}

/// Both sides went through textconv (or one did) and show the same text:
/// no patch and no change.
fn textconv_equal(p: &Pair, o: &Option<Loaded>, n: &Option<Loaded>) -> bool {
    match (o, n) {
        (Some(o), Some(n)) => {
            (o.conv.is_some() || n.conv.is_some())
                && o.mode == n.mode
                && !p.is_rename()
                && o.shown() == n.shown()
        }
        _ => false,
    }
}

/// Diff drivers: binary decisions, textconv, hunk-header patterns and
/// external programs.
impl Output<'_> {
    /// What the `diff` attribute says about `path`.
    fn diff_attr(&self, path: &[u8]) -> Result<DiffAttr, Die> {
        let Some(repo) = self.repo else {
            return Ok(DiffAttr::Unspecified);
        };
        let state = match self.attr_tree {
            Some(tree) => {
                let (index, _) = repo.read_index()?;
                let mut attrs = crate::attributes::open(repo, &index, Some(tree), false);
                let id = attrs.id(b"diff");
                attrs.lookup(path, false).get(id).clone()
            }
            None => repo.converter()?.attr(path, b"diff"),
        };
        self.drivers.resolve(&repo.config, &state)
    }

    /// `path`'s contents `data` as a combined diff shows them: through
    /// the textconv filter (with `--textconv`), and whether the `diff`
    /// attribute makes the path binary (`Some(true)`) or text.
    pub fn combined_view(
        &self,
        path: &[u8],
        data: Vec<u8>,
    ) -> Result<(Vec<u8>, Option<bool>), Die> {
        let attr = self.diff_attr(path)?;
        if self.textconv {
            if let Some(cmd) = attr.textconv() {
                if let Ok(conv) = super::driver::run_textconv(cmd, &data) {
                    return Ok((conv, Some(false)));
                }
            }
        }
        Ok((data, attr.binary()))
    }

    /// Whether any diff driver has a textconv filter configured.
    fn any_textconv(&self) -> bool {
        self.repo.is_some_and(|r| {
            r.config.entries().iter().any(|e| {
                let n = e.name.as_slice();
                n.starts_with(b"diff.") && n.ends_with(b".textconv")
            })
        })
    }

    /// Run the textconv filter of `path`'s driver on `l`.
    fn apply_textconv(&self, path: &[u8], l: &mut Loaded) -> Result<(), Die> {
        if !is_file_mode(l.mode) || type_bits(l.mode) == 0o120000 {
            return Ok(());
        }
        let attr = self.diff_attr(path)?;
        if let Some(cmd) = attr.textconv() {
            match super::driver::run_textconv(cmd, &l.data) {
                Ok(conv) => l.conv = Some(conv),
                Err(()) => l.conv_failed = true,
            }
        }
        Ok(())
    }

    /// Whether a pair is binary: by the `diff` attribute or the contents
    /// of either side. In patches (`patch`) `-a` makes nothing binary, nor
    /// is a side after textconv; the diffstat ignores both.
    fn pair_binary(
        &self,
        p: &Pair,
        o: Option<&Loaded>,
        n: Option<&Loaded>,
        patch: bool,
    ) -> Result<bool, Die> {
        if patch && self.text {
            return Ok(false);
        }
        for (path, side) in [(&p.old_path, o), (&p.path, n)] {
            let Some(l) = side else { continue };
            if patch && l.conv.is_some() {
                continue;
            }
            // Symbolic links ignore diff drivers (t4011).
            let forced = if self.repo.is_some() && type_bits(l.mode) != 0o120000 {
                self.diff_attr(path)?.binary()
            } else {
                None
            };
            if forced.unwrap_or_else(|| is_binary(&l.data)) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Patch options for a pair whose driver sets hunk-header patterns or
    /// an algorithm; `None` to use the common ones.
    fn pair_options(&self, attr: &DiffAttr) -> Option<PatchOptions> {
        let d = attr.driver()?;
        let algorithm = d.algorithm.filter(|_| !self.cli_algorithm);
        if d.funcname.is_none() && algorithm.is_none() {
            return None;
        }
        let mut o = self.patch.clone();
        o.diff.funcname = d.funcname.clone();
        if let Some((a, minimal)) = algorithm {
            o.diff.algorithm = a;
            o.diff.minimal = minimal;
        }
        Some(o)
    }

    /// The external program for a pair (its driver's `command` first),
    /// when external programs are allowed.
    fn external_program(&self, attr: &DiffAttr) -> Option<external::Program> {
        let global = self.external.as_ref()?;
        if let Some(cmd) = attr.driver().and_then(|d| d.command.clone()) {
            let d = attr.driver().expect("driver");
            return Some(external::Program {
                cmd,
                trust_exit_code: d.trust_exit_code,
            });
        }
        global.clone()
    }

    /// Run `prog` for pair number `idx` of `total`; returns whether it
    /// counts as a change.
    #[allow(clippy::too_many_arguments)]
    fn run_external(
        &self,
        prog: &external::Program,
        p: &Pair,
        o: &Option<Loaded>,
        n: &Option<Loaded>,
        idx: usize,
        total: usize,
        quiet: bool,
    ) -> Result<bool, Die> {
        let sides = if p.unmerged {
            None
        } else {
            Some((
                self.external_side(&p.old, o, &p.old_path)?,
                self.external_side(&p.new, n, &p.path)?,
            ))
        };
        // Two different names without a rename (`--no-index`): the new
        // name follows, without an extended header.
        let renamed_names = (p.renamed.is_none() && !p.unmerged && p.old_path != p.path)
            .then(|| (&p.path[..], String::new()));
        let rename = p.renamed.map(|c| {
            let word = if c == b'C' { "copy" } else { "rename" };
            let q = |path: &[u8]| {
                let mut v = Vec::new();
                quote_path(&mut v, path, self.quote_high);
                String::from_utf8_lossy(&v).into_owned()
            };
            (
                &p.path[..],
                format!(
                    "similarity index {}%\n{word} from {}\n{word} to {}\n",
                    p.percent(),
                    q(&p.old_path),
                    q(&p.path)
                ),
            )
        });
        let rename = rename.or(renamed_names);
        let cwd = if self.convert {
            self.repo.and_then(|r| r.setup.work_tree.clone())
        } else {
            None
        };
        let call = external::Call {
            path: &p.old_path,
            sides,
            rename,
            counter: idx + 1,
            total,
        };
        let stdout = match (&self.output, quiet) {
            (Some(_), false) => self.output_handle(),
            _ => None,
        };
        external::run(prog, &call, cwd.as_deref(), quiet, stdout)
    }

    /// The `--output` file, created by the first call and appended to
    /// by later ones.
    fn output_handle(&self) -> Option<std::fs::File> {
        let path = self.output.as_ref()?;
        let f = if self.output_started.replace(true) {
            std::fs::OpenOptions::new().append(true).open(path)
        } else {
            std::fs::File::create(path)
        };
        f.ok()
    }

    /// Write `out` to the output (`--output` file or stdout).
    fn write_out(&self, out: &[u8]) {
        match self.output.is_some() {
            true => {
                if let Some(mut f) = self.output_handle() {
                    let _ = f.write_all(out);
                }
            }
            false => {
                let mut o = std::io::stdout().lock();
                let _ = o.write_all(out);
                let _ = o.flush();
            }
        }
    }

    /// A side as the external program sees it: the work tree file when
    /// it holds the contents, else the contents as they would be checked
    /// out.
    fn external_side<'x>(
        &self,
        s: &Option<Side>,
        l: &Option<Loaded>,
        path: &'x [u8],
    ) -> Result<Option<external::ExtSide<'x>>, Die> {
        let Some(s) = s.as_ref() else {
            return Ok(None);
        };
        let data = l.as_ref().map(|l| l.data.clone()).unwrap_or_default();
        let regular = is_file_mode(s.mode) && type_bits(s.mode) != GITLINK_MODE;
        // The file as the program (run at the top of the work tree) finds
        // it: the path before --relative cut it; `--no-index` files as
        // they were named.
        let mut file = match &self.relative {
            Some(prefix) if self.convert => {
                let mut full = prefix.clone();
                if !full.is_empty() && !full.ends_with(b"/") {
                    full.push(b'/');
                }
                full.extend_from_slice(path);
                full
            }
            _ => path.to_vec(),
        };
        let no_index_file = s.file.as_ref().filter(|_| !self.convert && regular);
        if let Some(f) = no_index_file {
            file = f.as_os_str().as_encoded_bytes().to_vec();
        }
        let mut in_worktree =
            (s.file.is_some() && self.convert && regular) || no_index_file.is_some();
        let repo = self.repo.filter(|_| self.convert && regular);
        let mut shown = data;
        if let (Some(repo), false) = (repo, in_worktree) {
            // git hands over the work tree file when it has the same
            // contents.
            if let Some(root) = &repo.setup.work_tree {
                let full = root.join(external::os(&file));
                let meta = std::fs::symlink_metadata(&full).ok();
                if meta.is_some_and(|m| m.is_file()) && type_bits(s.mode) != 0o120000 {
                    if let Ok(raw) = std::fs::read(&full) {
                        let conv = repo.converter()?;
                        let opts = rgit_convert::Options::inspect(conv.config());
                        let blob = conv.to_git(path, raw, opts)?;
                        if rgit_hash::hash_object(self.hash, "blob", &blob).ok() == Some(s.oid) {
                            in_worktree = true;
                        }
                    }
                }
            }
            if !in_worktree && type_bits(s.mode) != 0o120000 {
                shown = repo.converter()?.to_worktree(path, &s.oid, shown);
            }
        }
        Ok(Some(external::ExtSide {
            data: shown,
            oid: s.oid,
            mode: s.mode,
            in_worktree,
            path,
            file,
        }))
    }

    /// `--quiet` with external programs: only programs that tell about
    /// differences by their exit code are run.
    fn external_quiet(
        &self,
        items: &[(Pair, Option<Loaded>, Option<Loaded>)],
        changed: &mut [bool],
    ) -> Result<(), Die> {
        if self.external.is_none() {
            return Ok(());
        }
        for (idx, (p, o, n)) in items.iter().enumerate() {
            let attr = self.diff_attr(&p.old_path)?;
            if let Some(prog) = self.external_program(&attr) {
                changed[idx] = !prog.trust_exit_code
                    || self.run_external(&prog, p, o, n, idx, items.len(), true)?;
            }
        }
        Ok(())
    }
}

impl Output<'_> {
    /// Write the output to stdout, then the warnings, and compute the
    /// exit code.
    pub fn finish(&self, out: &[u8], changed: bool, opts: &DiffOpts) -> i32 {
        let code = match &self.output {
            Some(path) => {
                let written = if self.output_started.get() {
                    std::fs::OpenOptions::new()
                        .append(true)
                        .open(path)
                        .and_then(|mut f| f.write_all(out))
                } else {
                    std::fs::write(path, out)
                };
                if written.is_err() {
                    crate::messages::print_error(&msg::could_not_open_output(
                        &path.to_string_lossy(),
                    ));
                    return 128;
                }
                i32::from(opts.wants_exit_code() && changed)
            }
            None => finish(out, changed, opts),
        };
        self.warn();
        code | self.check_code()
    }

    /// 2 when `--check` found whitespace errors, else 0.
    pub fn check_code(&self) -> i32 {
        if self.ws_errors.get() { 2 } else { 0 }
    }

    /// Warn that the rename limit was exceeded.
    pub fn warn(&self) {
        if let Some((n, degraded)) = self.limit_needed.take() {
            let mut e = std::io::stderr().lock();
            let _ = e.write_all(if degraded {
                msg::RENAME_LIMIT_DEGRADED.as_bytes()
            } else {
                msg::RENAME_LIMIT_SKIPPED.as_bytes()
            });
            let _ = e.write_all(msg::rename_limit_hint(n).as_bytes());
        }
    }
}

/// Write the output to stdout and compute the exit code.
pub fn finish(out: &[u8], changed: bool, opts: &DiffOpts) -> i32 {
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(out);
    let _ = o.flush();
    i32::from(opts.wants_exit_code() && changed)
}
