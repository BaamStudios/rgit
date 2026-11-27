// SPDX-License-Identifier: MIT
//! `git commit-graph write` and `git commit-graph verify`
//! (Documentation/git-commit-graph.adoc). The file format work is in
//! rgit-commitgraph; this file parses the options, collects the starting
//! commits (refs, standard input, packs) and prints.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use rgit_commitgraph::verify::{self, Outcome};
use rgit_commitgraph::write::{self, Split, WriteError};
use rgit_hash::ObjectId;
use rgit_object::ObjectKind;

use crate::commit_graph as cg;
use crate::messages::commit_graph as msg;
use crate::prelude::*;
use crate::repo::{self, Mode};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let object_dir = RefCell::new(None);
    let mut parser = Parser::new(
        &[msg::USAGE_VERIFY, msg::USAGE_WRITE],
        vec![
            Opt::string(None, "object-dir", &object_dir, "dir", msg::H_OBJECT_DIR),
            Opt::subcommand("verify"),
            Opt::subcommand("write"),
        ],
    );
    let parsed = parser.parse(argv)?;
    let dir = object_dir.borrow().clone();
    match parsed.subcommand.as_deref() {
        Some("verify") => run_verify(ctx, &parsed.args, dir),
        _ => run_write(ctx, &parsed.args, dir),
    }
}

/// The object directory to work on: `--object-dir`, which must be the
/// repository's or one of its alternates, or the repository's own.
fn resolve_object_dir(local: &Path, given: Option<String>) -> Result<PathBuf, Die> {
    let Some(given) = given else {
        return Ok(local.to_path_buf());
    };
    let canon = |p: &Path| rgit_fs::path::canonicalize(p).ok();
    let want = canon(Path::new(&given));
    if want.is_some() {
        for d in cg::object_dirs(local) {
            if canon(&d) == want {
                return Ok(d);
            }
        }
    }
    Err(Die::Fatal(msg::no_object_dir(&given)))
}

fn progress_wanted(v: i32) -> bool {
    match v {
        -1 => rgit_progress::stderr_is_terminal(),
        v => v != 0,
    }
}

fn run_verify(ctx: &mut Context, argv: &[String], object_dir: Option<String>) -> CmdResult {
    let object_dir = RefCell::new(object_dir);
    let shallow = Cell::new(false);
    let progress = Cell::new(-1);
    let mut parser = Parser::new(
        &[msg::USAGE_VERIFY],
        vec![
            Opt::string(None, "object-dir", &object_dir, "dir", msg::H_OBJECT_DIR),
            Opt::flag(None, "shallow", &shallow, msg::H_SHALLOW),
            Opt::bool(None, "progress", &progress, msg::H_PROGRESS),
        ],
    );
    let args = parser.parse(argv)?.args;
    if !args.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let setup = repo::setup(ctx, Mode::Required)?;
    let config = repo::load_config(ctx, &setup)?;
    if !cg::enabled(&config) {
        return Ok(0);
    }
    let local = crate::revisions::objects_dir(&setup);
    let dir = resolve_object_dir(&local, object_dir.take())?;
    let odb = repo::odb_with_config(&setup, &config)?;
    let hash = setup.format.object_format;
    let objects = cg::DbObjects { odb: &odb };
    Ok(verify_graph(
        &objects,
        &dir,
        cg::load_options(&config, hash),
        shallow.get(),
        progress_wanted(progress.get()),
    ))
}

/// Run the verification and print its lines; the exit code (128 after a
/// fatal error).
pub fn verify_graph(
    objects: &dyn rgit_commitgraph::Objects,
    dir: &Path,
    opts: rgit_commitgraph::LoadOptions,
    shallow: bool,
    progress: bool,
) -> i32 {
    let dirs = cg::object_dirs(dir);
    let mut lines: Vec<String> = Vec::new();
    let mut err = std::io::stderr();
    let outcome = verify::verify(objects, dir, &dirs, opts, shallow, progress, &mut |l| {
        let _ = writeln!(err, "{l}");
        lines.push(l.to_string());
    });
    match outcome {
        Outcome::Exit(c) => c,
        Outcome::Fatal(_) => {
            // The last line is the fatal message; it was printed without
            // its prefix.
            128
        }
    }
}

fn run_write(ctx: &mut Context, argv: &[String], object_dir: Option<String>) -> CmdResult {
    let object_dir = RefCell::new(object_dir);
    let reachable = Cell::new(false);
    let stdin_packs = Cell::new(false);
    let stdin_commits = Cell::new(false);
    let append = Cell::new(false);
    let changed_paths = Cell::new(-1);
    let split: RefCell<Option<Split>> = RefCell::new(None);
    let split_error: RefCell<Option<String>> = RefCell::new(None);
    let max_commits = Cell::new(0);
    let size_multiple = Cell::new(2);
    let expire_time = RefCell::new(None);
    let max_new_filters: Cell<Option<i64>> = Cell::new(None);
    let progress = Cell::new(-1);
    let mut parser = Parser::new(
        &[msg::USAGE_WRITE],
        vec![
            Opt::string(None, "object-dir", &object_dir, "dir", msg::H_OBJECT_DIR),
            Opt::flag(None, "reachable", &reachable, msg::H_REACHABLE),
            Opt::flag(None, "stdin-packs", &stdin_packs, msg::H_STDIN_PACKS),
            Opt::flag(None, "stdin-commits", &stdin_commits, msg::H_STDIN_COMMITS),
            Opt::flag(None, "append", &append, msg::H_APPEND),
            Opt::bool(None, "changed-paths", &changed_paths, msg::H_CHANGED_PATHS),
            Opt::callback(None, "split", Some("..."), msg::H_SPLIT, |v, _| {
                let s = match v {
                    None => Some(Split::Merge),
                    Some("no-merge") => Some(Split::NoMerge),
                    Some("replace") => Some(Split::Replace),
                    Some(other) => {
                        *split_error.borrow_mut() = Some(other.to_string());
                        Some(Split::Merge)
                    }
                };
                *split.borrow_mut() = s;
                Ok(())
            })
            .optional(None)
            .literal_argh()
            .noneg(),
            Opt::integer(None, "max-commits", &max_commits, msg::H_MAX_COMMITS),
            Opt::integer(None, "size-multiple", &size_multiple, msg::H_SIZE_MULTIPLE),
            Opt::string(
                None,
                "expire-time",
                &expire_time,
                "expiry-date",
                msg::H_EXPIRE_TIME,
            ),
            Opt::callback(
                None,
                "max-new-filters",
                Some("..."),
                msg::H_MAX_NEW_FILTERS,
                |v, unset| {
                    if unset {
                        max_new_filters.set(Some(-1));
                        return Ok(());
                    }
                    let v = v.unwrap_or("");
                    let n: i64 = v
                        .trim()
                        .parse()
                        .map_err(|_| CallbackError::Usage(msg::MAX_NEW_FILTERS_NUMBER.into()))?;
                    max_new_filters.set(Some(n));
                    Ok(())
                },
            )
            .literal_argh(),
            Opt::bool(None, "progress", &progress, msg::H_PROGRESS),
        ],
    );
    let args = parser.parse(argv)?.args;
    if !args.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    if let Some(bad) = split_error.take() {
        return Err(Die::Fatal(msg::unrecognized_split(&bad)));
    }
    let sources = [reachable.get(), stdin_packs.get(), stdin_commits.get()]
        .iter()
        .filter(|b| **b)
        .count();
    if sources > 1 {
        return Err(Die::Fatal(msg::AT_MOST_ONE_SOURCE.to_string()));
    }
    let setup = repo::setup(ctx, Mode::Required)?;
    let config = repo::load_config(ctx, &setup)?;
    if !cg::enabled(&config) {
        crate::argv::errln(&format!("warning: {}", msg::DISABLED));
        return Ok(0);
    }
    let hash = setup.format.object_format;
    let local = crate::revisions::objects_dir(&setup);
    let dir = resolve_object_dir(&local, object_dir.take())?;
    let show = progress_wanted(progress.get());

    let mut opts = cg::write_options(&config, hash, cg::file_mode(&config));
    let cpv = cg::changed_paths_version(&config);
    if !(-1..=2).contains(&cpv) {
        let _ = writeln!(
            std::io::stderr(),
            "warning: {}",
            msg::unsupported_changed_paths_version(i64::from(cpv))
        );
        return Ok(0);
    }
    opts.split = *split.borrow();
    opts.append = append.get();
    opts.changed_paths = match changed_paths.get() {
        -1 => None,
        v => Some(v != 0),
    };
    if let Some(n) = max_new_filters.get() {
        opts.max_new_filters = u32::try_from(n).ok();
    }
    opts.size_multiple = u32::try_from(size_multiple.get()).unwrap_or(2);
    opts.max_commits = u32::try_from(max_commits.get()).unwrap_or(0);
    if let Some(v) = expire_time.take() {
        let date_ctx = rgit_date::DateContext::from_env();
        let t = rgit_date::parse_expiry_date(&v, &date_ctx)
            .map_err(|_| Die::Fatal(msg::malformed_expiration(&v)))?;
        opts.expire_time = Some(i64::try_from(t).unwrap_or(i64::MAX));
    }
    opts.progress = show;

    // Grafts and shallow clones change the history: no graph then.
    let shallow = setup.common_dir.join("shallow");
    if !cg::history_unchanged(&setup.common_dir, &shallow) {
        return Ok(0);
    }
    let odb = repo::odb_with_config(&setup, &config)?;

    let start = if reachable.get() {
        let refs = crate::revisions::Refs::new(&setup, hash);
        let commits = cg::ref_commits(&odb, &refs);
        if show {
            let mut p = rgit_progress::Progress::start_delayed(msg::COLLECTING_REFS, 0, true);
            p.set(commits.len() as u64);
            p.stop();
        }
        commits
    } else if stdin_commits.get() {
        read_stdin_commits(&odb, show)?
    } else if stdin_packs.get() {
        let mut names = Vec::new();
        for line in std::io::stdin().lock().lines() {
            let line = line.map_err(|e| Die::Fatal(e.to_string()))?;
            names.push(line);
        }
        pack_commits(&odb, &dir, &names, show)?
    } else {
        let mut names: Vec<String> = std::fs::read_dir(dir.join("pack"))
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.ends_with(".idx"))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        pack_commits(&odb, &dir, &names, show)?
    };

    let existing = cg::load(&dir, cg::load_options_for_write(&config, hash));
    let objects = cg::DbObjects { odb: &odb };
    match write::write(&objects, &dir, existing.as_ref(), &start, &opts) {
        Ok(_) => Ok(0),
        Err(WriteError::UnparsableCommit(id)) => {
            let hex = id.to_hex();
            if !odb.contains(&id) {
                crate::argv::errln(&format!("error: {}", msg::could_not_read(&hex)));
            }
            Err(Die::Fatal(format!("unable to parse commit {hex}")))
        }
        Err(e) => Err(Die::Fatal(e.to_string())),
    }
}

/// `--stdin-commits`: full hex ids, one per line; tags are peeled, other
/// objects ignored.
fn read_stdin_commits(odb: &rgit_odb::ObjectDatabase, show: bool) -> Result<Vec<ObjectId>, Die> {
    let hash = odb.hash_kind();
    let mut out = Vec::new();
    let mut p = show.then(|| rgit_progress::Progress::start_delayed(msg::COLLECTING_INPUT, 0, true));
    let mut n = 0u64;
    for line in std::io::stdin().lock().lines() {
        let line = line.map_err(|e| Die::Fatal(e.to_string()))?;
        let text = line.trim_end_matches('\r');
        let id = match ObjectId::from_hex(hash, text.as_bytes()) {
            Ok(id) if text.len() == hash.hex_len() => id,
            _ => return Err(Die::Error(msg::non_hex(text), 1)),
        };
        if !odb.contains(&id) {
            return Err(Die::Error(msg::invalid_object(text), 1));
        }
        if let Some(c) = cg::peel_to_commit(odb, &id) {
            out.push(c);
        }
        n += 1;
        if let Some(p) = p.as_mut() {
            p.set(n);
        }
    }
    if let Some(p) = p {
        p.stop();
    }
    Ok(out)
}

/// The commits in the pack indexes `names` (file names in
/// `<object dir>/pack`).
fn pack_commits(
    odb: &rgit_odb::ObjectDatabase,
    dir: &Path,
    names: &[String],
    show: bool,
) -> Result<Vec<ObjectId>, Die> {
    let hash = odb.hash_kind();
    let pack_dir = dir.join("pack");
    let mut indexes = Vec::new();
    for name in names {
        let path = pack_dir.join(name);
        match rgit_pack::PackIndex::open(&path, hash) {
            Ok(i) => indexes.push(i),
            Err(_) => {
                return Err(Die::Error(
                    msg::error_adding_pack(&path.to_string_lossy()),
                    1,
                ));
            }
        }
    }
    let total: u64 = indexes.iter().map(|i| u64::from(i.len())).sum();
    let mut p = (show && !indexes.is_empty())
        .then(|| rgit_progress::Progress::start_delayed(&msg::finding_commits(indexes.len()), total, true));
    let mut out = Vec::new();
    let mut n = 0u64;
    for idx in &indexes {
        for id in idx.ids() {
            n += 1;
            if let Some(p) = p.as_mut() {
                p.set(n);
            }
            if let Ok(Some(h)) = odb.header(&id) {
                if h.kind == ObjectKind::Commit {
                    out.push(id);
                }
            }
        }
    }
    if let Some(p) = p {
        p.stop();
    }
    Ok(out)
}
