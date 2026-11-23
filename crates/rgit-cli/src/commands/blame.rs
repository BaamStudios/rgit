// SPDX-License-Identifier: MIT
//! `git blame` (Documentation/git-blame.adoc, blame-options.adoc,
//! config/blame.adoc): show which commit last changed each line of a file.
//! `git annotate` is the same command with the `-c` output format.
//!
//! The computation is in [`score`], `-L` parsing in [`range`], the output
//! formats in [`output`]. Without a revision the work tree version is
//! blamed: lines that differ from `HEAD` belong to a pseudo commit "Not
//! Committed Yet" with the null object name.

#[path = "blame/fuzzy.rs"]
mod fuzzy;
#[path = "blame/output.rs"]
mod output;
#[path = "blame/range.rs"]
mod range;
#[path = "blame/score.rs"]
mod score;

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::rc::Rc;

use rgit_date::{DateContext, DateMode, DateStyle};
use rgit_diff::{Algorithm, DiffOptions};
use rgit_hash::ObjectId;
use rgit_object::{Object, ObjectKind};

use crate::history::mailmap::Mailmap;
use crate::messages::blame as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;
use output::{Coloring, Format, Info, Printer};
use range::RangeError;
use score::{Blob, Entry, Scoreboard, Settings};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    main(ctx, argv, false)
}

/// The values of the command line options.
#[derive(Default)]
struct Cli {
    incremental: Cell<bool>,
    blank_boundary: Cell<bool>,
    root: Cell<i32>,
    show_stats: Cell<bool>,
    progress: Cell<i32>,
    score_debug: Cell<bool>,
    textconv: Cell<i32>,
    show_name: Cell<i32>,
    show_number: Cell<bool>,
    porcelain: Cell<bool>,
    line_porcelain: Cell<bool>,
    annotate: Cell<bool>,
    raw_time: Cell<bool>,
    long: Cell<bool>,
    no_author: Cell<bool>,
    show_email: Cell<i32>,
    ignore_ws: Cell<bool>,
    diff_algorithm: RefCell<Option<String>>,
    ignore_rev: RefCell<Vec<String>>,
    ignore_revs_file: RefCell<Vec<String>>,
    color_lines: Cell<bool>,
    color_by_age: Cell<bool>,
    revs_file: RefCell<Option<String>>,
    contents: RefCell<Option<String>>,
    copies: Cell<u8>,
    moves: Cell<bool>,
    /// The scores of `-C<num>` and `-M<num>` (0: default).
    copy_score: Cell<usize>,
    move_score: Cell<usize>,
    ranges: RefCell<Vec<String>>,
    /// `None`: default; `Some(0)`: full names.
    abbrev: Cell<Option<i64>>,
}

/// Revision options blame takes from the `rev-list` family.
#[derive(Default)]
struct RevOpts {
    reverse: bool,
    first_parent: bool,
    since: Option<i64>,
    date: Option<String>,
    minimal: bool,
    algorithm: Option<Algorithm>,
    indent_heuristic: Option<bool>,
    /// `--encoding=<encoding>` (`none`: as stored).
    encoding: Option<String>,
}

pub(crate) fn main(ctx: &mut Context, argv: &[String], annotate: bool) -> CmdResult {
    let cli = Cli {
        root: Cell::new(-1),
        progress: Cell::new(-1),
        show_name: Cell::new(-1),
        show_email: Cell::new(-1),
        textconv: Cell::new(-1),
        ..Cli::default()
    };
    let mut parser = Parser::new(
        if annotate {
            msg::USAGE_ANNOTATE
        } else {
            msg::USAGE
        },
        vec![
            Opt::flag(None, "incremental", &cli.incremental, msg::HELP_INCREMENTAL),
            Opt::flag('b', None, &cli.blank_boundary, msg::HELP_BLANK_BOUNDARY),
            Opt::bool(None, "root", &cli.root, msg::HELP_ROOT),
            Opt::flag(None, "show-stats", &cli.show_stats, msg::HELP_SHOW_STATS),
            Opt::bool(None, "progress", &cli.progress, msg::HELP_PROGRESS),
            Opt::flag(None, "score-debug", &cli.score_debug, msg::HELP_SCORE_DEBUG),
            Opt::bool(None, "textconv", &cli.textconv, msg::HELP_TEXTCONV).hidden(),
            Opt::bool('f', "show-name", &cli.show_name, msg::HELP_SHOW_NAME),
            Opt::flag('n', "show-number", &cli.show_number, msg::HELP_SHOW_NUMBER),
            Opt::flag('p', "porcelain", &cli.porcelain, msg::HELP_PORCELAIN),
            Opt::flag(None, "line-porcelain", &cli.line_porcelain, msg::HELP_LINE_PORCELAIN),
            Opt::flag('c', None, &cli.annotate, msg::HELP_ANNOTATE),
            Opt::flag('t', None, &cli.raw_time, msg::HELP_RAW_TIME),
            Opt::flag('l', None, &cli.long, msg::HELP_LONG),
            Opt::flag('s', None, &cli.no_author, msg::HELP_NO_AUTHOR),
            Opt::bool('e', "show-email", &cli.show_email, msg::HELP_SHOW_EMAIL),
            Opt::flag('w', None, &cli.ignore_ws, msg::HELP_IGNORE_WS),
            Opt::string(
                None,
                "diff-algorithm",
                &cli.diff_algorithm,
                "algorithm",
                msg::HELP_DIFF_ALGORITHM,
            )
            .noneg(),
            Opt::string_list(None, "ignore-rev", &cli.ignore_rev, "rev", msg::HELP_IGNORE_REV),
            Opt::string_list(
                None,
                "ignore-revs-file",
                &cli.ignore_revs_file,
                "file",
                msg::HELP_IGNORE_REVS_FILE,
            ),
            Opt::flag(None, "color-lines", &cli.color_lines, msg::HELP_COLOR_LINES),
            Opt::flag(None, "color-by-age", &cli.color_by_age, msg::HELP_COLOR_BY_AGE),
            Opt::string('S', None, &cli.revs_file, "file", msg::HELP_REVS_FILE),
            Opt::string(None, "contents", &cli.contents, "file", msg::HELP_CONTENTS),
            Opt::callback('C', None, Some("score"), msg::HELP_COPIES, |v, _| {
                cli.copies.set(cli.copies.get().saturating_add(1));
                if let Some(v) = v {
                    cli.copy_score.set(v.parse().unwrap_or(0));
                }
                Ok(())
            })
            .optional(None),
            Opt::callback('M', None, Some("score"), msg::HELP_MOVES, |v, _| {
                cli.moves.set(true);
                if let Some(v) = v {
                    cli.move_score.set(v.parse().unwrap_or(0));
                }
                Ok(())
            })
            .optional(None),
            Opt::string_list('L', None, &cli.ranges, "range", msg::HELP_RANGE),
            Opt::callback(None, "abbrev", Some("n"), msg::HELP_ABBREV, |v, unset| {
                if unset {
                    cli.abbrev.set(Some(0));
                    return Ok(());
                }
                match v {
                    None => cli.abbrev.set(None),
                    Some(v) => match v.parse::<i64>() {
                        Ok(n) => cli.abbrev.set(Some(n)),
                        Err(_) => {
                            return Err(CallbackError::Error(
                                crate::messages::abbrev_not_numeric("abbrev"),
                            ));
                        }
                    },
                }
                Ok(())
            })
            .optional(None),
        ],
    )
    .flags(ParseFlags {
        keep_dashdash: true,
        keep_unknown_opt: true,
        ..ParseFlags::default()
    });
    let args = parser.parse(argv)?.args;

    // Revision options and the positional arguments.
    let date_ctx = DateContext::from_env();
    let mut rev = RevOpts::default();
    let mut positional: Vec<String> = Vec::new();
    let mut after_dashdash: Option<Vec<String>> = None;
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        if let Some(rest) = after_dashdash.as_mut() {
            rest.push(a);
            continue;
        }
        if a == "--" {
            after_dashdash = Some(Vec::new());
            continue;
        }
        if !a.starts_with('-') || a == "-" {
            positional.push(a);
            continue;
        }
        let value = |prefixes: &[&str]| prefixes.iter().find_map(|p| a.strip_prefix(p));
        if a == "--reverse" {
            rev.reverse = true;
        } else if a == "--first-parent" {
            rev.first_parent = true;
        } else if let Some(v) = value(&["--since=", "--after="]) {
            rev.since = Some(rgit_date::approxidate(v, &date_ctx) as i64);
        } else if let Some(v) = value(&["--max-age="]) {
            rev.since = v.parse().ok();
        } else if let Some(v) = value(&["--date="]) {
            rev.date = Some(v.to_owned());
        } else if a == "--date" {
            rev.date = it.next();
        } else if a == "--minimal" {
            rev.minimal = true;
        } else if a == "--patience" {
            rev.algorithm = Some(Algorithm::Patience);
        } else if a == "--histogram" {
            rev.algorithm = Some(Algorithm::Histogram);
        } else if a == "--indent-heuristic" {
            rev.indent_heuristic = Some(true);
        } else if a == "--no-indent-heuristic" {
            rev.indent_heuristic = Some(false);
        } else if let Some(v) = value(&["--encoding="]) {
            rev.encoding = Some(v.to_owned());
        } else if a == "--no-progress" {
        } else {
            return Err(parser.error_with_usage(&msg::unknown_option(&a)).into());
        }
    }

    if cli.progress.get() == 1
        && (cli.incremental.get() || cli.porcelain.get() || cli.line_porcelain.get())
    {
        return Err(Die::Fatal(msg::PROGRESS_WITH_PORCELAIN.into()));
    }

    let mut repo = Repo::open(ctx, Mode::Required)?;
    let config = repo.config.clone();
    let mailmap = {
        let setup = repo.setup.clone();
        let cell = RefCell::new(&mut repo);
        Mailmap::load_with(&setup, &config, &|spec| {
            let mut r = cell.borrow_mut();
            let id = r.resolve(spec)?;
            match r.read_object(&id) {
                Some(Object::Blob(b)) => Some(b.data),
                _ => None,
            }
        })
    };
    let cfg_bool = |key: &str| config.bool(key).ok().flatten();

    // Revisions and the path.
    let (revs, path_arg) = match after_dashdash {
        Some(mut paths) => {
            if paths.len() != 1 {
                return Err(parser.usage_error().into());
            }
            let mut revs = positional;
            let path = paths.pop().unwrap_or_default();
            revs.retain(|r| !r.is_empty());
            (revs, path)
        }
        None => match positional.len() {
            0 => return Err(parser.usage_error().into()),
            1 => (Vec::new(), positional.pop().unwrap_or_default()),
            2 if repo.resolve(&positional[1]).is_some() && !in_work_tree(&repo, &positional[1]) => {
                let rev = positional.pop().unwrap_or_default();
                (vec![rev], positional.pop().unwrap_or_default())
            }
            _ => {
                let path = positional.pop().unwrap_or_default();
                (positional, path)
            }
        },
    };
    drop(parser);

    let path = repo
        .prefix_path(path_arg.as_bytes())
        .unwrap_or_else(|()| path_arg.as_bytes().to_vec());
    let path_str = String::from_utf8_lossy(&path).into_owned();

    // Unsupported so far: say so instead of printing a wrong answer.
    if rev.reverse {
        return Err(Die::Fatal(msg::REVERSE_NOT_SUPPORTED.into()));
    }
    if cli.revs_file.borrow().is_some() {
        return Err(Die::Fatal(msg::REVS_FILE_NOT_SUPPORTED.into()));
    }

    // The diff.
    let mut diff = DiffOptions::default();
    let algo = cli.diff_algorithm.borrow().clone().or_else(|| {
        config
            .string("diff.algorithm")
            .ok()
            .flatten()
            .map(|v| v.to_string())
    });
    if let Some(name) = algo {
        let (a, minimal) = Algorithm::parse(&name)
            .ok_or_else(|| Die::Error(msg::INVALID_DIFF_ALGORITHM.into(), rgit_options::EXIT_USAGE))?;
        diff.algorithm = a;
        diff.minimal = minimal;
    }
    if let Some(a) = rev.algorithm {
        diff.algorithm = a;
    }
    diff.minimal = last_minimal(argv).unwrap_or(diff.minimal || rev.minimal);
    if let Some(h) = rev.indent_heuristic.or_else(|| cfg_bool("diff.indentheuristic")) {
        diff.indent_heuristic = h;
    }
    let settings = Settings {
        first_parent: rev.first_parent,
        show_root: match cli.root.get() {
            -1 => cfg_bool("blame.showroot").unwrap_or(false),
            v => v == 1,
        },
        since: rev.since,
        diff,
        ignore_whitespace: cli.ignore_ws.get(),
        moves: (cli.moves.get() || cli.copies.get() > 0).then_some(match cli.move_score.get() {
            0 => DEFAULT_MOVE_SCORE,
            n => n,
        }),
        copies: cli.copies.get().min(3),
        copy_score: match cli.copy_score.get() {
            0 => DEFAULT_COPY_SCORE,
            n => n,
        },
        ignore: ignored_revs(&mut repo, &config, &cli)?,
    };

    // Positive and negative revisions.
    let mut positive: Vec<(String, ObjectId)> = Vec::new();
    let mut negative: Vec<ObjectId> = Vec::new();
    for r in &revs {
        if let Some(base) = r.strip_suffix("^!") {
            let id = resolve_commit(&mut repo, base, r)?;
            negative.extend(commit_parents(&repo, &id));
            positive.push((base.to_owned(), id));
        } else if let Some(n) = r.strip_prefix('^') {
            negative.push(resolve_commit(&mut repo, n, r)?);
        } else if let Some((a, b)) = r.split_once("..").filter(|_| !r.contains("...")) {
            let a = if a.is_empty() { "HEAD" } else { a };
            let b = if b.is_empty() { "HEAD" } else { b };
            negative.push(resolve_commit(&mut repo, a, r)?);
            positive.push((b.to_owned(), resolve_commit(&mut repo, b, r)?));
        } else {
            positive.push((r.clone(), resolve_commit(&mut repo, r, r)?));
        }
    }
    if positive.len() > 1 {
        return Err(Die::Fatal(msg::more_than_one(&positive[0].0, &positive[1].0)));
    }

    let head = repo.resolve("HEAD").and_then(|h| peel_commit(&mut repo, h));
    let mut sb = Scoreboard::new(&repo, settings);
    // Every version of a file goes through the textconv filter of its
    // path's diff driver (not symbolic links: their target is shown as
    // it is).
    if cli.textconv.get() != 0 {
        let repo_ref = &repo;
        let commands: RefCell<HashMap<Vec<u8>, Option<String>>> = RefCell::new(HashMap::new());
        sb.textconv = Some(Box::new(move |p: &[u8], data: Vec<u8>| {
            let cmd = commands
                .borrow_mut()
                .entry(p.to_vec())
                .or_insert_with(|| textconv_command(repo_ref, p))
                .clone();
            match cmd {
                Some(cmd) => crate::diff::driver::run_textconv(&cmd, &data).unwrap_or(data),
                None => data,
            }
        }));
    }
    let mut fake: Option<(usize, Info)> = None;
    let contents = cli.contents.borrow().clone();
    let use_work_tree = positive.is_empty() && repo.setup.work_tree.is_some();
    let final_origin = if contents.is_some() || use_work_tree {
        let mut parents: Vec<ObjectId> = match positive.first() {
            Some((_, id)) => vec![*id],
            None => head.into_iter().collect(),
        };
        if contents.is_none() {
            parents.extend(merge_heads(&repo));
        }
        let (tracked, in_index) = index_paths(&repo, &path);
        if contents.is_none() && !in_index {
            let mut found = false;
            for p in &parents {
                let n = sb.commit(p).map_err(score_error)?;
                found |= sb.has_path(n, &path).map_err(score_error)?;
            }
            if !found {
                return Err(Die::Fatal(msg::no_such_path_in_head(&path_str)));
            }
        }
        let (data, source) = match &contents {
            Some(file) => (read_contents(ctx, file)?, file.clone()),
            None => (read_work_tree(&repo, &path, &path_str)?, path_str.clone()),
        };
        let blob = rgit_hash::hash_object(repo.hash, "blob", &data)
            .unwrap_or_else(|_| ObjectId::null(repo.hash));
        let mut flat = tracked;
        flat.insert(path.clone(), (0o100644, blob));
        let now = date_ctx.now;
        let n = sb.fake_commit(parents, now, flat);
        let (name, mail) = if contents.is_some() {
            (msg::EXTERNAL_FILE, msg::EXTERNAL_FILE_MAIL)
        } else {
            (msg::NOT_COMMITTED_YET, msg::NOT_COMMITTED_YET_MAIL)
        };
        let tz = format!("{:+05}", date_ctx.local_tz(now));
        fake = Some((
            n,
            Info::fake(name, mail, now, &tz, msg::summary(&path_str, &source)),
        ));
        let data = match &sb.textconv {
            Some(conv) => conv(&path, data),
            None => data,
        };
        sb.origin_with(n, &path, blob, Rc::new(Blob::new(data)))
    } else {
        let (name, id) = match positive.first() {
            Some(p) => p.clone(),
            None => {
                let id = head.ok_or_else(|| Die::Fatal(msg::bad_revision("HEAD")))?;
                ("HEAD".to_owned(), id)
            }
        };
        let n = sb.commit(&id).map_err(score_error)?;
        sb.final_origin(n, &path)
            .map_err(score_error)?
            .ok_or_else(|| Die::Fatal(msg::no_such_path_in(&path_str, &name)))?
    };
    for id in &negative {
        sb.exclude(id).map_err(score_error)?;
    }

    // The lines to blame.
    let final_blob = sb.content(final_origin).map_err(score_error)?;
    let lines = final_blob.lines();
    let mut ranges = Vec::new();
    let mut anchor = 0;
    // `-L :<funcname>` uses the function lines of the path's diff driver.
    let funcname = if cli.ranges.borrow().iter().any(|r| r.starts_with(':') || r.starts_with("^:")) {
        match repo.converter()?.attr(&path, b"diff") {
            rgit_attr::State::Value(v) => {
                crate::diff::driver::load(&repo.config, &String::from_utf8_lossy(&v))?.funcname
            }
            _ => None,
        }
    } else {
        None
    };
    for spec in cli.ranges.borrow().iter() {
        match range::parse(spec, &lines, anchor, &path_str, funcname.as_ref()) {
            Ok(r) => {
                anchor = r.1;
                ranges.push(r);
            }
            Err(RangeError::Fatal(m)) => return Err(Die::Fatal(m)),
            Err(RangeError::Usage) => {
                let usage = if annotate { msg::USAGE_ANNOTATE } else { msg::USAGE };
                let _ = writeln!(std::io::stderr(), "usage: {}", usage[0]);
                return Err(Die::Exit(rgit_options::EXIT_USAGE));
            }
        }
    }
    if ranges.is_empty() {
        ranges.push((0, lines.len()));
    }
    let entries: Vec<Entry> = range::normalize(ranges)
        .into_iter()
        .filter(|(a, b)| b > a)
        .map(|(a, b)| Entry {
            lno: a,
            num: b - a,
            suspect: final_origin,
            s_lno: a,
            ignored: false,
            unblamable: false,
        })
        .collect();
    drop(lines);
    let total: usize = entries.iter().map(|e| e.num).sum();
    sb.final_blob = Some(final_blob.clone());
    sb.add(final_origin, entries);
    sb.run().map_err(score_error)?;
    // Progress appears after GIT_PROGRESS_DELAY seconds (default 2); only
    // the final state is shown, and only when asked for explicitly.
    if cli.progress.get() == 1 && std::env::var("GIT_PROGRESS_DELAY").is_ok_and(|v| v == "0") {
        eprintln!("{}", msg::progress_done(total));
    }

    // Output.
    let done = std::mem::take(&mut sb.done);
    let mut batches: Vec<Vec<Entry>> = Vec::new();
    if cli.incremental.get() {
        let mut starts = std::mem::take(&mut sb.batches);
        starts.push(done.len());
        for w in starts.windows(2) {
            let mut batch = done[w[0]..w[1]].to_vec();
            batch.sort_by_key(|e| e.lno);
            batches.push(batch);
        }
    }
    let entries = output::coalesce(done);
    let mut infos: HashMap<usize, Info> = HashMap::new();
    if let Some((n, info)) = fake {
        infos.insert(n, info);
    }
    // Names and summaries in the log output encoding.
    let encoding = match &rev.encoding {
        Some(e) => e.as_bytes().to_vec(),
        None => crate::history::recode::output_encoding(&repo.config),
    };
    let convert = rev.encoding.as_deref() != Some("none");
    for e in &entries {
        let n = sb.origins[e.suspect].commit;
        if !infos.contains_key(&n)
            && let Some(c) = &sb.nodes[n].commit
        {
            let shown = convert
                .then(|| crate::history::recode::for_output(c, &encoding, repo.hash))
                .flatten();
            infos.insert(n, Info::of_commit(shown.as_ref().unwrap_or(c), &mailmap));
        }
    }

    let date = match rev
        .date
        .clone()
        .or_else(|| config.string("blame.date").ok().flatten().map(|v| v.to_string()))
    {
        Some(d) => rgit_date::parse_date_mode(&d, std::io::IsTerminal::is_terminal(&std::io::stdout()))
            .map_err(|e| Die::Fatal(e.to_string()))?,
        None => DateMode::new(DateStyle::Iso8601),
    };
    let hex_len = repo.hash.hex_len();
    let abbrev = if cli.long.get() {
        hex_len
    } else {
        match cli.abbrev.get() {
            None => repo.default_abbrev(),
            Some(0) => hex_len,
            // One more than the full length marks "longer than a name".
            Some(n) => usize::try_from(n).unwrap_or(0).clamp(4, hex_len + 1),
        }
    };
    let porcelain = cli.porcelain.get() || cli.line_porcelain.get();
    let coloring = coloring(&cli, &config, &date_ctx)?;
    let fmt = Format {
        coloring,
        porcelain,
        line_porcelain: cli.line_porcelain.get(),
        annotate: annotate || cli.annotate.get(),
        show_name: match cli.show_name.get() {
            -1 => None,
            v => Some(v == 1),
        },
        show_number: cli.show_number.get(),
        no_author: cli.no_author.get(),
        show_email: match cli.show_email.get() {
            -1 => cfg_bool("blame.showemail").unwrap_or(false),
            v => v == 1,
        },
        raw_time: cli.raw_time.get(),
        blank_boundary: cli.blank_boundary.get() || cfg_bool("blame.blankboundary").unwrap_or(false),
        mark_ignored: cfg_bool("blame.markignoredlines").unwrap_or(false),
        mark_unblamable: cfg_bool("blame.markunblamablelines").unwrap_or(false),
        abbrev,
        date,
        date_ctx,
    };
    crate::pager::setup(ctx, if annotate { "annotate" } else { "blame" }, true)?;
    let abbrev_fn = |id: &ObjectId, len: usize| repo.abbrev(id, len);
    let printer = Printer {
        sb: &sb,
        infos: &infos,
        fmt: &fmt,
        final_lines: &final_blob,
        final_path: &path,
        abbrev: &abbrev_fn,
    };
    let mut out = std::io::stdout().lock();
    let _ = if cli.incremental.get() {
        printer.incremental(&batches, &mut out)
    } else {
        printer.print(&entries, &mut out)
    };
    let _ = out.flush();
    Ok(0)
}

/// `--color-lines`, `--color-by-age` or `blame.coloring`, with the colours
/// of `color.blame.repeatedLines` and `color.blame.highlightRecent`.
fn coloring(
    cli: &Cli,
    config: &rgit_config::ConfigSet,
    date_ctx: &DateContext,
) -> Result<Coloring, Die> {
    let mode = if cli.color_by_age.get() {
        "highlightrecent".to_owned()
    } else if cli.color_lines.get() {
        "repeatedlines".to_owned()
    } else {
        config
            .string("blame.coloring")
            .ok()
            .flatten()
            .map(|v| v.to_string().to_ascii_lowercase())
            .unwrap_or_default()
    };
    match mode.as_str() {
        "repeatedlines" => Ok(Coloring::Repeated(crate::color::slot(
            config,
            "color.blame.repeatedlines",
            REPEATED_COLOR,
        )?)),
        "highlightrecent" => {
            let spec = config
                .string("color.blame.highlightrecent")
                .ok()
                .flatten()
                .map_or_else(|| HIGHLIGHT_RECENT.join(","), |v| v.to_string());
            let mut parts = spec.split(',').map(str::trim);
            let color = |s: &str| {
                rgit_config::color::parse_color(s)
                    .ok_or_else(|| Die::Fatal(msg::invalid_color(s)))
            };
            let first = color(parts.next().unwrap_or_default())?;
            let mut steps = Vec::new();
            while let Some(date) = parts.next() {
                let time = rgit_date::approxidate(date, date_ctx) as i64;
                let c = color(parts.next().unwrap_or_default())?;
                steps.push((time, c));
            }
            Ok(Coloring::Age(first, steps))
        }
        _ => Ok(Coloring::None),
    }
}

/// The commits to ignore: the files of `blame.ignoreRevsFile`, then those
/// of `--ignore-revs-file` (an empty name forgets the files before), then
/// `--ignore-rev`.
fn ignored_revs(
    repo: &mut Repo,
    config: &rgit_config::ConfigSet,
    cli: &Cli,
) -> Result<std::collections::HashSet<ObjectId>, Die> {
    let mut out = std::collections::HashSet::new();
    let top = repo.setup.work_tree.clone();
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    let add = |files: &mut Vec<std::path::PathBuf>, name: &str, base: Option<&std::path::Path>| {
        if name.is_empty() {
            files.clear();
        } else {
            let path = std::path::Path::new(name);
            files.push(match base {
                Some(b) if path.is_relative() => b.join(path),
                _ => path.to_path_buf(),
            });
        }
    };
    for e in config.get_all("blame.ignorerevsfile") {
        let name = e.value.as_ref().map(|v| v.to_string()).unwrap_or_default();
        add(&mut files, &name, top.as_deref());
    }
    for name in cli.ignore_revs_file.borrow().iter() {
        add(&mut files, name, None);
    }
    for f in files {
        let data = std::fs::read(&f).map_err(|_| {
            Die::Fatal(msg::cannot_open_revs_file(&f.to_string_lossy()))
        })?;
        for line in data.split(|&b| b == b'\n') {
            let line = match line.iter().position(|&b| b == b'#') {
                Some(i) => &line[..i],
                None => line,
            };
            let line = line.trim_ascii();
            if line.is_empty() {
                continue;
            }
            let id = ObjectId::from_hex(repo.hash, line).map_err(|_| {
                Die::Fatal(msg::invalid_object_name(&String::from_utf8_lossy(line)))
            })?;
            out.insert(id);
        }
    }
    for rev in cli.ignore_rev.borrow().iter() {
        let id = repo
            .resolve(rev)
            .and_then(|id| peel_commit(repo, id))
            .ok_or_else(|| Die::Fatal(msg::cannot_find_ignored(rev)))?;
        out.insert(id);
    }
    Ok(out)
}

/// The default minimum scores of `-M` and `-C` (blame-options.adoc).
const DEFAULT_MOVE_SCORE: usize = 20;
const DEFAULT_COPY_SCORE: usize = 40;

/// `color.blame.repeatedLines` default: cyan.
const REPEATED_COLOR: &str = "\x1b[36m";
/// `color.blame.highlightRecent` default (Documentation/config/color.adoc).
const HIGHLIGHT_RECENT: [&str; 5] = ["blue", "12 month ago", "white", "1 month ago", "red"];

/// Whether the diff is minimal according to the last of `--minimal` and
/// `--diff-algorithm` on the command line (they are parsed apart, so their
/// order is not known otherwise); `None` if neither is given.
fn last_minimal(argv: &[String]) -> Option<bool> {
    let mut out = None;
    let mut it = argv.iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--" => break,
            "--minimal" => out = Some(true),
            "--diff-algorithm" => out = it.next().map(|v| v == "minimal"),
            _ => {
                if let Some(v) = a.strip_prefix("--diff-algorithm=") {
                    out = Some(v == "minimal");
                }
            }
        }
    }
    out
}

fn score_error(e: score::Error) -> Die {
    match e {
        score::Error::Missing(id) => Die::Fatal(msg::unable_to_read(&id.to_hex())),
    }
}

/// `arg` names an existing file below the current directory.
fn in_work_tree(repo: &Repo, arg: &str) -> bool {
    repo.setup.work_tree.is_some() && std::path::Path::new(arg).symlink_metadata().is_ok()
}

fn peel_commit(repo: &mut Repo, id: ObjectId) -> Option<ObjectId> {
    repo.peel_to(id, ObjectKind::Commit)
}

fn resolve_commit(repo: &mut Repo, name: &str, arg: &str) -> Result<ObjectId, Die> {
    repo.resolve(name)
        .and_then(|id| peel_commit(repo, id))
        .ok_or_else(|| Die::Fatal(msg::bad_revision(arg)))
}

fn commit_parents(repo: &Repo, id: &ObjectId) -> Vec<ObjectId> {
    match repo.read_object(id) {
        Some(Object::Commit(c)) => c.parents().unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// The commits in `MERGE_HEAD`.
fn merge_heads(repo: &Repo) -> Vec<ObjectId> {
    std::fs::read_to_string(repo.setup.git_dir.join("MERGE_HEAD"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| ObjectId::from_hex(repo.hash, l.trim().as_bytes()).ok())
        .collect()
}

/// The stage 0 entries of the index, and whether `path` is in it at any
/// stage.
fn index_paths(repo: &Repo, path: &[u8]) -> (BTreeMap<Vec<u8>, (u32, ObjectId)>, bool) {
    let mut out = BTreeMap::new();
    let mut found = false;
    if let Ok((index, _)) = repo.read_index() {
        for e in index.entries() {
            found |= e.path() == path;
            if e.stage() == rgit_index::Stage::MERGED {
                out.insert(e.path().to_vec(), (e.mode.0, e.id));
            }
        }
    }
    (out, found)
}

/// The work tree version of `path` in blob form.
fn read_work_tree(repo: &Repo, path: &[u8], shown: &str) -> Result<Vec<u8>, Die> {
    let top = repo.work_tree()?;
    let full = top.join(std::path::Path::new(&*String::from_utf8_lossy(path)));
    let meta = full.symlink_metadata().map_err(|e| {
        Die::Fatal(msg::cannot_lstat(shown, &rgit_messages::strerror(&e)))
    })?;
    let data = if meta.file_type().is_symlink() {
        std::fs::read_link(&full)
            .map(|t| t.into_os_string().into_encoded_bytes())
            .map_err(|_| Die::Fatal(msg::cannot_read(shown)))?
    } else {
        std::fs::read(&full).map_err(|_| Die::Fatal(msg::cannot_read(shown)))?
    };
    if meta.file_type().is_symlink() {
        return Ok(data);
    }
    repo.converter()?
        .to_git(path, data, rgit_convert::Options::QUIET)
}

/// The textconv program of `path`'s diff driver; none for a symbolic
/// link in the work tree.
fn textconv_command(repo: &Repo, path: &[u8]) -> Option<String> {
    let link = repo.setup.work_tree.as_ref().is_some_and(|w| {
        std::fs::symlink_metadata(w.join(String::from_utf8_lossy(path).as_ref()))
            .is_ok_and(|m| m.file_type().is_symlink())
    });
    if link {
        return None;
    }
    let rgit_attr::State::Value(v) = repo.converter().ok()?.attr(path, b"diff") else {
        return None;
    };
    crate::diff::driver::load(&repo.config, &String::from_utf8_lossy(&v))
        .ok()?
        .textconv
}

/// `--contents <file>` (`-` for standard input).
fn read_contents(ctx: &Context, file: &str) -> Result<Vec<u8>, Die> {
    if file == "-" {
        let mut data = Vec::new();
        let _ = std::io::Read::read_to_end(&mut std::io::stdin().lock(), &mut data);
        return Ok(data);
    }
    let _ = ctx;
    std::fs::metadata(file)
        .map_err(|e| Die::Fatal(msg::cannot_stat(file, &rgit_messages::strerror(&e))))?;
    std::fs::read(file).map_err(|_| Die::Fatal(msg::cannot_read(file)))
}
