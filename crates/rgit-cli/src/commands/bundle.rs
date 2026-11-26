// SPDX-License-Identifier: MIT
//! `git bundle create | verify | list-heads | unbundle`
//! (Documentation/git-bundle.adoc, Documentation/gitformat-bundle.adoc).
//!
//! `create` walks `rev-list --boundary <git-rev-list-args>`: the boundary
//! commits become the prerequisites, the refs the positive arguments name
//! (see [`crate::history::bundle_walk`]) the references, and the objects
//! reachable from them but not from the prerequisites the (thin) pack.
//! Version 2 is written for SHA-1, version 3 (`@object-format`) for
//! SHA-256 or when asked for; `--filter` writes `@filter` and leaves the
//! filtered objects out.
//!
//! `verify`, `list-heads` and `unbundle` read the header with
//! `rgit_bundle`; `unbundle` stores the pack like a fetch does
//! (`rgit_fetch::bundle`).

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, IsTerminal, Read, Write};
use std::path::Path;

use rgit_bundle::filter::Filter;
use rgit_bundle::{FilterSpec, Header, HeaderError, Prerequisite, Reference};
use rgit_fetch::LocalRepo;
use rgit_fetch::bundle::{self as fb, OpenBundle, OpenError, Prerequisites};
use rgit_hash::{HashKind, ObjectId};
use rgit_object::{Commit, ObjectKind, Tag, TreeIter};
use rgit_odb::ObjectDatabase;
use rgit_pack::write::{self as pw, ListedObject, Tip};

use crate::messages::bundle as msg;
use crate::prelude::*;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mut parser = Parser::new(
        &[
            msg::USAGE_CREATE,
            msg::USAGE_VERIFY,
            msg::USAGE_LIST_HEADS,
            msg::USAGE_UNBUNDLE,
        ],
        vec![
            Opt::subcommand("create"),
            Opt::subcommand("verify"),
            Opt::subcommand("list-heads"),
            Opt::subcommand("unbundle"),
        ],
    );
    let parsed = parser.parse(argv)?;
    match parsed.subcommand.as_deref() {
        Some("create") => create(ctx, &parsed.args),
        Some("verify") => verify(ctx, &parsed.args),
        Some("list-heads") => list_heads(&parsed.args),
        _ => unbundle(ctx, &parsed.args),
    }
}

/// The name a bundle file is shown with (`-` is standard input).
fn shown_name(file: &str) -> &str {
    if file == "-" { msg::STDIN_NAME } else { file }
}

/// Open the bundle `file` (`-`: standard input); errors are printed and
/// give the exit code.
fn open(file: &str) -> Result<OpenBundle, Die> {
    let opened = if file == "-" {
        let stdin: Box<dyn BufRead + Send> = Box::new(BufReader::new(std::io::stdin()));
        fb::open_reader(stdin)
    } else {
        fb::open(Path::new(file))
    };
    opened.map_err(|e| match e {
        OpenError::CannotOpen(_) => Die::Error(msg::could_not_open(file), 1),
        OpenError::Header(HeaderError::NotABundle) => {
            Die::Error(rgit_bundle::messages::not_a_bundle(shown_name(file)), 1)
        }
        OpenError::Header(HeaderError::Error(m)) => Die::Error(m, 1),
        OpenError::Header(HeaderError::Fatal(m)) => Die::Fatal(m),
        OpenError::Header(HeaderError::Io(_)) => Die::Error(msg::could_not_open(file), 1),
    })
}

/// The refs of `header` matching `names` (all without names), one per
/// line: `<id> <name>`.
fn print_refs(header: &Header, names: &[String]) {
    let mut out = std::io::stdout().lock();
    for r in &header.references {
        if !names.is_empty() && !names.iter().any(|n| n.as_bytes() == r.name.as_slice()) {
            continue;
        }
        let _ = write!(out, "{} ", r.id.to_hex());
        let _ = out.write_all(&r.name);
        let _ = out.write_all(b"\n");
    }
}

fn open_repo(ctx: &Context, setup: &rgit_repo::Setup) -> Result<LocalRepo, Die> {
    let config = crate::repo::load_config(ctx, setup)?;
    LocalRepo::open(setup, config).map_err(|e| Die::Fatal(e.to_string()))
}

fn list_heads(argv: &[String]) -> CmdResult {
    let mut parser = Parser::new(&[msg::USAGE_LIST_HEADS], vec![]);
    let args = parser.parse(argv)?.args;
    let Some(file) = args.first() else {
        return Err(parser.usage_msg(msg::NEED_FILE).into());
    };
    let bundle = open(file)?;
    print_refs(&bundle.header, &args[1..]);
    Ok(0)
}

fn verify(ctx: &Context, argv: &[String]) -> CmdResult {
    let quiet = Cell::new(0);
    let mut parser = Parser::new(
        &[msg::USAGE_VERIFY],
        vec![Opt::bool('q', "quiet", &quiet, msg::H_VERIFY_QUIET)],
    );
    let args = parser.parse(argv)?.args;
    let Some(file) = args.first() else {
        return Err(parser.usage_msg(msg::NEED_FILE).into());
    };
    let Some(setup) = crate::repo::setup_gently(ctx)? else {
        return Err(Die::Error(msg::NEED_REPO_VERIFY.into(), 1));
    };
    let bundle = open(file)?;
    let repo = open_repo(ctx, &setup)?;
    let check = fb::check_prerequisites(&repo, &bundle.header);
    if let Prerequisites::Missing(_) = check {
        check.report();
        return Ok(1);
    }
    check.report();
    if check == Prerequisites::Ok {
        eprintln!("{}", msg::is_okay(shown_name(file)));
    }
    let h = &bundle.header;
    if quiet.get() == 0 {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{}", msg::contains_refs(h.references.len()));
        drop(out);
        print_refs(h, &[]);
        let mut out = std::io::stdout().lock();
        if h.prerequisites.is_empty() {
            let _ = writeln!(out, "{}", msg::COMPLETE_HISTORY);
        } else {
            let _ = writeln!(out, "{}", msg::requires_refs(h.prerequisites.len()));
            for p in &h.prerequisites {
                let _ = writeln!(out, "{} ", p.id.to_hex());
            }
        }
        let _ = writeln!(out, "{}", msg::uses_hash(h.hash.name()));
        if let Some(f) = &h.filter {
            let _ = writeln!(out, "{}", msg::uses_filter(f.as_str()));
        }
    }
    Ok(i32::from(check != Prerequisites::Ok))
}

fn unbundle(ctx: &Context, argv: &[String]) -> CmdResult {
    let progress = Cell::new(0);
    let mut parser = Parser::new(
        &[msg::USAGE_UNBUNDLE],
        vec![Opt::bool(None, "progress", &progress, msg::H_PROGRESS)],
    );
    let args = parser.parse(argv)?.args;
    let Some(file) = args.first() else {
        return Err(parser.usage_msg(msg::NEED_FILE).into());
    };
    let Some(setup) = crate::repo::setup_gently(ctx)? else {
        return Err(Die::Fatal(msg::NEED_REPO_UNBUNDLE.into()));
    };
    let mut bundle = open(file)?;
    let repo = open_repo(ctx, &setup)?;
    let check = fb::check_prerequisites(&repo, &bundle.header);
    if check != Prerequisites::Ok {
        check.report();
        return Ok(1);
    }
    let opts = fb::UnbundleOptions {
        progress: progress.get() > 0,
        title: Some(msg::UNBUNDLING.to_owned()),
        fsck: None,
    };
    match fb::unbundle(&repo, &mut bundle, &opts) {
        Ok(pack) => pack.release(),
        Err(e) => {
            eprintln!("fatal: {e}");
            eprintln!("error: {}", msg::INDEX_PACK_DIED);
            return Ok(1);
        }
    }
    print_refs(&bundle.header, &args[1..]);
    Ok(0)
}

/// Pull `--filter` (and `--no-filter`) out of the rev-list arguments, and
/// drop `--objects` (the bundle has the objects anyway).
fn take_filters(args: &[String]) -> (Vec<String>, Vec<String>) {
    let mut rest = Vec::new();
    let mut filters = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            rest.extend(args[i..].iter().cloned());
            break;
        }
        if let Some(v) = a.strip_prefix("--filter=") {
            filters.push(v.to_owned());
        } else if a == "--filter" && i + 1 < args.len() {
            filters.push(args[i + 1].clone());
            i += 1;
        } else if a == "--no-filter" {
            filters.clear();
        } else if a != "--objects" {
            rest.push(a.clone());
        }
        i += 1;
    }
    (rest, filters)
}

/// The subject of a commit as `%s` shows it: the first paragraph, lines
/// joined with spaces.
fn subject(message: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(message);
    let mut lines = text.lines().skip_while(|l| l.trim().is_empty());
    let mut out = String::new();
    for l in lines.by_ref() {
        if l.trim().is_empty() {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(l.trim_end());
    }
    out.into_bytes()
}

/// Whether the tagger date of the tag `data` is within `--since` /
/// `--until` (a tag without date is).
fn tag_in_range(hash: HashKind, data: &[u8], since: Option<i64>, until: Option<i64>) -> bool {
    let Ok(tag) = Tag::parse(hash, data) else {
        return true;
    };
    let Some(time) = tag.tagger().and_then(|t| t.time()) else {
        return true;
    };
    if since.is_some_and(|s| time.seconds < s) {
        return false;
    }
    if until.is_some_and(|u| time.seconds > u) {
        return false;
    }
    true
}

fn create(ctx: &Context, argv: &[String]) -> CmdResult {
    let quiet = Cell::new(i32::from(!std::io::stderr().is_terminal()));
    let progress = Cell::new(0);
    let version = Cell::new(-1);
    let mut parser = Parser::new(
        &[msg::USAGE_CREATE],
        vec![
            Opt::quiet(&quiet, msg::H_CREATE_QUIET),
            Opt::set_int(None, "progress", &progress, msg::H_PROGRESS, 1),
            Opt::integer(None, "version", &version, msg::H_VERSION),
        ],
    )
    .flags(ParseFlags {
        stop_at_non_option: true,
        keep_dashdash: true,
        ..Default::default()
    });
    let args = parser.parse(argv)?.args;
    let Some(file) = args.first().cloned() else {
        return Err(parser.usage_msg(msg::NEED_FILE).into());
    };
    let Some(setup) = crate::repo::setup_gently(ctx)? else {
        return Err(Die::Fatal(msg::NEED_REPO_CREATE.into()));
    };
    let show_progress = progress.get() > 0 || quiet.get() == 0;
    let hash = setup.format.object_format;
    let (rev_args, filters) = take_filters(&args[1..]);
    let filter = FilterSpec::parse_all(&filters).map_err(Die::Fatal)?;
    let version = match version.get() {
        -1 if filter.is_some() || hash != HashKind::Sha1 => 3,
        -1 => 2,
        2 if filter.is_some() || hash != HashKind::Sha1 => {
            return Err(Die::Fatal(msg::cannot_write_v2(hash.name())));
        }
        v @ (2 | 3) => v as u32,
        v => return Err(Die::Fatal(msg::unsupported_version(v))),
    };
    if Path::new(&file).is_dir() {
        return Err(Die::Fatal(msg::cannot_create_dir(&file)));
    }
    let stdin = if rev_args
        .iter()
        .take_while(|a| *a != "--")
        .any(|a| a == "--stdin")
    {
        let mut input = Vec::new();
        let _ = std::io::stdin().lock().read_to_end(&mut input);
        Some(input)
    } else {
        None
    };
    let walked = crate::history::bundle_walk(ctx, &rev_args, stdin.as_deref())?;

    let config = crate::repo::load_config(ctx, &setup)?;
    let odb = crate::repo::odb_with_config(&setup, &config)?;
    let shown: HashSet<ObjectId> = walked.shown.iter().copied().collect();
    let boundary: HashSet<ObjectId> = walked.boundary.iter().copied().collect();
    let mut header = Header::new(version, hash);
    header.filter = filter;
    for r in &walked.refs {
        let Ok(Some(raw)) = odb.read(&r.id) else {
            continue;
        };
        match raw.kind {
            ObjectKind::Tag => {
                if !tag_in_range(hash, &raw.data, walked.max_age, walked.min_age) {
                    continue;
                }
            }
            ObjectKind::Commit => {
                if boundary.contains(&r.id)
                    || walked
                        .negative
                        .iter()
                        .any(|n| rgit_fetch::graph::is_ancestor(&odb, &r.id, n))
                {
                    continue;
                }
                if !shown.contains(&r.id) {
                    eprintln!("warning: {}", msg::ref_excluded(&r.given));
                    continue;
                }
            }
            _ => {}
        }
        header.references.push(Reference {
            id: r.id,
            name: r.name.clone().into_bytes(),
        });
    }
    if header.references.is_empty() {
        return Err(Die::Fatal(msg::EMPTY_BUNDLE.into()));
    }
    for id in &walked.boundary {
        let comment = odb
            .read(id)
            .ok()
            .flatten()
            .and_then(|raw| Commit::parse(hash, &raw.data).ok())
            .map(|c| subject(c.message()))
            .unwrap_or_default();
        header.prerequisites.push(Prerequisite { id: *id, comment });
    }

    // The objects: reachable from the refs, not from the prerequisites.
    let mut tips: Vec<Tip> = header
        .references
        .iter()
        .map(|r| Tip {
            id: r.id,
            exclude: false,
        })
        .collect();
    // Like git, everything the negative arguments reach is left out too
    // (not only what the prerequisites reach).
    tips.extend(
        walked
            .boundary
            .iter()
            .chain(walked.negative.iter())
            .map(|id| Tip {
                id: *id,
                exclude: true,
            }),
    );
    let sparse = config.bool("pack.usesparse").ok().flatten().unwrap_or(true);
    let (mut listed, bases) = pw::enumerate_with_edges(&odb, hash, &tips, sparse)
        .map_err(|e| Die::Fatal(e.to_string()))?;
    if let Some(f) = &header.filter {
        let named: HashSet<ObjectId> = header.references.iter().map(|r| r.id).collect();
        apply_filter(&odb, hash, &f.filter, &named, &mut listed)?;
    }
    let mut opts = super::pack_objects::config_options(&config)?;
    opts.ofs_delta = true;
    opts.progress = pw::WriteProgress {
        compress: show_progress,
        write: show_progress,
        throughput: show_progress,
    };
    if show_progress {
        super::pack_objects::show_enumerated(&listed, &bases);
    }
    let ordered = pw::pack_order(&listed);
    let packs = odb.packs().map(|p| p.set().packs()).unwrap_or_default();

    let write = |out: &mut dyn Write| -> Result<pw::PackSummary, Die> {
        header
            .write(out)
            .map_err(|e| Die::Fatal(e.to_string()))?;
        let (summary, _) =
            pw::write_thin_pack(&mut *out, hash, &ordered, &bases, &odb, &packs, &opts)
                .map_err(|e| Die::Fatal(e.to_string()))?;
        Ok(summary)
    };
    let summary = if file == "-" {
        let mut out = std::io::BufWriter::with_capacity(1 << 16, std::io::stdout().lock());
        let s = write(&mut out)?;
        out.flush().map_err(|e| Die::Fatal(e.to_string()))?;
        s
    } else {
        let target = std::path::absolute(&file).unwrap_or_else(|_| file.clone().into());
        let mut lock = rgit_fs::LockFile::acquire(&target, &rgit_fs::LockOptions::default())
            .map_err(|e| Die::Fatal(e.to_string()))?;
        let s = {
            let mut out = std::io::BufWriter::with_capacity(1 << 16, &mut lock);
            let s = write(&mut out)?;
            out.flush().map_err(|e| Die::Fatal(e.to_string()))?;
            s
        };
        lock.commit().map_err(|e| Die::Fatal(e.to_string()))?;
        s
    };
    if show_progress {
        eprintln!(
            "{}",
            crate::messages::pack_objects::total(
                summary.entries.len(),
                summary.deltas,
                summary.reused,
                summary.reused_deltas
            )
        );
    }
    Ok(0)
}

/// Leave out the objects `filter` excludes (objects named as refs stay).
fn apply_filter(
    odb: &ObjectDatabase,
    hash: HashKind,
    filter: &Filter,
    named: &HashSet<ObjectId>,
    listed: &mut Vec<ListedObject>,
) -> Result<(), Die> {
    let depths = if needs_depth(filter) {
        Some(tree_depths(odb, hash, listed))
    } else {
        None
    };
    let size = |id: &ObjectId| {
        odb.header(id)
            .ok()
            .flatten()
            .map_or(0, |h| h.size)
    };
    let keep = |o: &ListedObject| -> Result<bool, Die> {
        if named.contains(&o.id) {
            return Ok(true);
        }
        keeps(filter, o, &size, depths.as_ref())
    };
    let mut out = Vec::with_capacity(listed.len());
    for o in listed.drain(..) {
        if keep(&o)? {
            out.push(o);
        }
    }
    *listed = out;
    Ok(())
}

fn needs_depth(filter: &Filter) -> bool {
    match filter {
        Filter::Tree(_) => true,
        Filter::Combine(parts) => parts.iter().any(needs_depth),
        _ => false,
    }
}

fn keeps(
    filter: &Filter,
    o: &ListedObject,
    size: &dyn Fn(&ObjectId) -> u64,
    depths: Option<&HashMap<ObjectId, u64>>,
) -> Result<bool, Die> {
    Ok(match filter {
        Filter::BlobNone => o.kind != ObjectKind::Blob,
        Filter::BlobLimit(n) => o.kind != ObjectKind::Blob || size(&o.id) < *n,
        Filter::Tree(d) => match o.kind {
            ObjectKind::Tree | ObjectKind::Blob => depths
                .and_then(|m| m.get(&o.id))
                .is_some_and(|depth| depth < d),
            _ => true,
        },
        Filter::ObjectType(t) => o.kind.name() == t.as_str(),
        Filter::Combine(parts) => {
            for p in parts {
                if !keeps(p, o, size, depths)? {
                    return Ok(false);
                }
            }
            true
        }
        Filter::SparseOid(_) => {
            return Err(Die::Fatal(msg::filter_not_supported("sparse:oid")));
        }
    })
}

/// The smallest depth at which each listed tree and blob is found below
/// the root trees of the listed commits (root trees have depth 0).
fn tree_depths(odb: &ObjectDatabase, hash: HashKind, listed: &[ListedObject]) -> HashMap<ObjectId, u64> {
    let mut depth: HashMap<ObjectId, u64> = HashMap::new();
    let mut queue: VecDeque<(ObjectId, u64)> = VecDeque::new();
    for o in listed.iter().filter(|o| o.kind == ObjectKind::Commit) {
        if let Some(tree) = odb
            .read(&o.id)
            .ok()
            .flatten()
            .and_then(|raw| Commit::parse(hash, &raw.data).ok())
            .and_then(|c| c.tree().ok())
        {
            queue.push_back((tree, 0));
        }
    }
    while let Some((id, d)) = queue.pop_front() {
        match depth.get(&id) {
            Some(&old) if old <= d => continue,
            _ => {}
        }
        depth.insert(id, d);
        let Ok(Some(raw)) = odb.read(&id) else {
            continue;
        };
        if raw.kind != ObjectKind::Tree {
            continue;
        }
        for entry in TreeIter::new(hash, &raw.data).flatten() {
            queue.push_back((entry.oid, d + 1));
        }
    }
    depth
}
