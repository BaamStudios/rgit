// SPDX-License-Identifier: MIT
//! `git range-diff` (Documentation/git-range-diff.adoc): compare two
//! versions of a patch series. The comparison itself is in
//! [`crate::range_diff`]; this file reads the command line.
//!
//! The ranges can be given as `<range1> <range2>`, `<rev1>...<rev2>` or
//! `<base> <rev1> <rev2>`, followed by paths. Without `--` the form is
//! found as observed from the `git` binary: three revisions make the
//! base form, two ranges the two-range form; otherwise, when an
//! argument looks like a range, all arguments must be revisions and the
//! first one a symmetric range.

use std::io::Write;


use crate::diff::options::DiffOpts;
use crate::messages::range_diff as msg;
use crate::prelude::*;
use crate::range_diff::{self, Colors, Options, is_commit, is_range, is_revision_arg, is_symmetric};
use crate::repo::Mode;
use crate::repository::Repo;

/// `fatal: <message>` followed by the usage, exit 129.
fn fatal_usage(message: &str) -> Die {
    let mut err = std::io::stderr().lock();
    let _ = write!(err, "fatal: {message}\n\n{}", msg::usage());
    Die::Exit(rgit_options::EXIT_USAGE)
}

fn error_usage(message: &str) -> Die {
    let mut err = std::io::stderr().lock();
    let _ = write!(err, "error: {message}\n{}", msg::usage());
    Die::Exit(rgit_options::EXIT_USAGE)
}

/// A number with an optional `k`, `m` or `g` suffix.
fn magnitude(option: &str, value: &str) -> Result<i64, Die> {
    let (num, factor) = match value.as_bytes().last() {
        Some(b'k' | b'K') => (&value[..value.len() - 1], 1i64 << 10),
        Some(b'm' | b'M') => (&value[..value.len() - 1], 1 << 20),
        Some(b'g' | b'G') => (&value[..value.len() - 1], 1 << 30),
        _ => (value, 1),
    };
    num.parse::<i64>()
        .ok()
        .and_then(|n| n.checked_mul(factor))
        .ok_or_else(|| {
            eprintln!("error: {}", msg::expects_integer(option));
            Die::Exit(rgit_options::EXIT_USAGE)
        })
}

/// What the command line asked for.
struct Args {
    creation_factor: i64,
    dual_color: bool,
    left_only: bool,
    right_only: bool,
    log_args: Vec<String>,
    include_merges: bool,
    max_memory: u64,
    diff: DiffOpts,
    /// The arguments that are not options, before `--`.
    revs: Vec<String>,
    /// `--` and what follows it.
    dashdash: Option<Vec<String>>,
}

/// Options that take their value from the next argument.
const WITH_VALUE: &[&str] = &["creation-factor", "diff-merges", "max-memory"];

fn parse(argv: &[String]) -> Result<Args, Die> {
    let mut a = Args {
        creation_factor: range_diff::DEFAULT_CREATION_FACTOR,
        dual_color: true,
        left_only: false,
        right_only: false,
        log_args: Vec::new(),
        include_merges: false,
        max_memory: range_diff::DEFAULT_MAX_MEMORY,
        diff: DiffOpts::default(),
        revs: Vec::new(),
        dashdash: None,
    };
    let mut it = argv.iter();
    while let Some(arg) = it.next() {
        let s = arg.as_str();
        if s == "--" {
            let mut rest = vec![arg.clone()];
            rest.extend(it.by_ref().cloned());
            a.dashdash = Some(rest);
            break;
        }
        if !s.starts_with('-') || s == "-" {
            a.revs.push(arg.clone());
            continue;
        }
        if let Some(long) = s.strip_prefix("--") {
            let (name, value) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None if WITH_VALUE.contains(&long) => {
                    let Some(v) = it.next() else {
                        return Err(error_usage(&format!("option `{long}' requires a value")));
                    };
                    (long, Some(v.clone()))
                }
                None => (long, None),
            };
            let handled = match (name, value.as_deref()) {
                ("creation-factor", Some(v)) => {
                    a.creation_factor = magnitude(name, v)?;
                    true
                }
                ("no-creation-factor", None) => {
                    a.creation_factor = 0;
                    true
                }
                ("dual-color", None) => {
                    a.dual_color = true;
                    true
                }
                ("no-dual-color", None) => {
                    a.dual_color = false;
                    true
                }
                ("notes" | "no-notes", _) => {
                    a.log_args.push(arg.clone());
                    true
                }
                ("diff-merges", Some(v)) => {
                    a.log_args.push(format!("--diff-merges={v}"));
                    a.include_merges = true;
                    true
                }
                ("no-diff-merges" | "remerge-diff" | "no-remerge-diff", None) => {
                    a.log_args.push(arg.clone());
                    a.include_merges = true;
                    true
                }
                ("left-only", None) => {
                    a.left_only = true;
                    true
                }
                ("no-left-only", None) => {
                    a.left_only = false;
                    true
                }
                ("right-only", None) => {
                    a.right_only = true;
                    true
                }
                ("no-right-only", None) => {
                    a.right_only = false;
                    true
                }
                ("max-memory", Some(v)) => {
                    a.max_memory = magnitude(name, v)?.max(0) as u64;
                    true
                }
                ("no-max-memory", None) => {
                    a.max_memory = 0;
                    true
                }
                _ => false,
            };
            if handled {
                continue;
            }
        }
        if !a.diff.parse_arg(s)? {
            let text = match s.strip_prefix("--") {
                Some(long) => format!("unknown option `{long}'"),
                None => format!("unknown switch `{}'", &s[1..2]),
            };
            return Err(error_usage(&text));
        }
    }
    Ok(a)
}

/// The two ranges and the paths.
fn ranges(repo: &mut Repo, a: &Args) -> Result<(String, String, Vec<String>), Die> {
    let revs = &a.revs;
    let sym = |r: &str| -> (String, String) {
        let i = r.find("...").expect("symmetric");
        let (x, y) = (&r[..i], &r[i + 3..]);
        (format!("{y}..{x}"), format!("{x}..{y}"))
    };
    if let Some(paths) = &a.dashdash {
        match revs.len() {
            1 => {
                if !is_symmetric(&revs[0]) {
                    return Err(fatal_usage(&msg::not_a_symmetric_range(&revs[0])));
                }
                let (l, r) = sym(&revs[0]);
                return Ok((l, r, paths.clone()));
            }
            2 => {
                for r in revs {
                    if !is_range(r) {
                        return Err(fatal_usage(&msg::not_a_commit_range(r)));
                    }
                }
                for r in revs {
                    if !is_revision_arg(repo, r) {
                        return Err(Die::Fatal(msg::bad_revision(r)));
                    }
                }
                return Ok((revs[0].clone(), revs[1].clone(), paths.clone()));
            }
            3 => {
                for r in revs {
                    if !is_commit(repo, r) {
                        return Err(fatal_usage(&msg::not_a_revision(r)));
                    }
                }
                return Ok((
                    format!("{}..{}", revs[0], revs[1]),
                    format!("{}..{}", revs[0], revs[2]),
                    paths.clone(),
                ));
            }
            _ => return Err(fatal_usage(msg::NEED_TWO_RANGES)),
        }
    }
    if revs.len() >= 3 && revs[..3].iter().all(|r| !r.contains("..") && is_commit(repo, r)) {
        return Ok((
            format!("{}..{}", revs[0], revs[1]),
            format!("{}..{}", revs[0], revs[2]),
            revs[3..].to_vec(),
        ));
    }
    if revs.len() >= 2 && is_range(&revs[0]) && is_range(&revs[1]) {
        for r in &revs[..2] {
            if !is_revision_arg(repo, r) {
                return Err(Die::Fatal(msg::bad_revision(r)));
            }
        }
        return Ok((revs[0].clone(), revs[1].clone(), revs[2..].to_vec()));
    }
    if revs.len() == 1 && is_symmetric(&revs[0]) {
        let (l, r) = sym(&revs[0]);
        return Ok((l, r, Vec::new()));
    }
    if revs.iter().any(|r| r.contains("..")) {
        for r in revs {
            if !is_revision_arg(repo, r) {
                return Err(Die::Fatal(msg::bad_revision(r)));
            }
        }
        if is_symmetric(&revs[0]) {
            let (l, r) = sym(&revs[0]);
            return Ok((l, r, revs[1..].to_vec()));
        }
    }
    Err(fatal_usage(msg::NEED_TWO_RANGES))
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    if argv.len() == 2 && matches!(argv[1].as_str(), "-h" | "--help-all") {
        let _ = std::io::stdout().write_all(msg::usage().as_bytes());
        return Ok(0);
    }
    let a = parse(&argv[1..])?;
    let mut repo = Repo::open(ctx, Mode::Required)?;
    if a.left_only && a.right_only {
        return Err(Die::Error(
            msg::cannot_be_used_together("--left-only", "--right-only"),
            255,
        ));
    }
    let (range1, range2, paths) = ranges(&mut repo, &a)?;

    let formats = a.diff.formats(true)?;
    crate::pager::setup_for(ctx, &repo.config, "range-diff", true)?;
    let color_wanted = crate::color::want(&repo.config, Some("color.diff"), a.diff.color)?;
    let abbrev = match a.diff.abbrev {
        None | Some(None) => repo.default_abbrev(),
        Some(Some(n)) => n,
    };
    let mut opts = Options::new(abbrev);
    opts.creation_factor = a.creation_factor;
    opts.dual_color = a.dual_color;
    opts.left_only = a.left_only;
    opts.right_only = a.right_only;
    opts.log_args = a.log_args.clone();
    opts.include_merges = a.include_merges;
    opts.max_memory = a.max_memory;
    opts.patch = formats.patch;
    if formats.stat {
        opts.stat = Some(crate::diff::stat::StatLayout {
            width: a.diff.stat_width.unwrap_or(80),
            name_width: a.diff.stat_name_width,
            graph_width: a.diff.stat_graph_width,
            count: a.diff.stat_count,
        });
    }
    if let Some(n) = a.diff.context {
        opts.diff.context = n;
    }
    if let Some(n) = a.diff.interhunk {
        opts.diff.interhunk_context = n;
    }
    if let Some(h) = a.diff.indent_heuristic {
        opts.diff.indent_heuristic = h;
    }
    if let Some(alg) = a.diff.algorithm {
        opts.diff.algorithm = alg;
    }
    if let Some(m) = a.diff.minimal {
        opts.diff.minimal = m;
    }
    opts.diff.whitespace = a.diff.whitespace;
    opts.diff.ignore_blank_lines = a.diff.ignore_blank_lines;
    if let Some(f) = a.diff.function_context {
        opts.diff.function_context = f;
    }
    if color_wanted {
        opts.color = Some(Colors::from_config(&repo.config)?);
    }
    let out = range_diff::compare(ctx, &repo, &range1, &range2, &paths, &opts)?;
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(&out);
    let _ = stdout.flush();
    Ok(0)
}
