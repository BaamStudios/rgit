// SPDX-License-Identifier: MIT
//! `git fetch-pack` (Documentation/git-fetch-pack.adoc): fetch the named
//! remote refs (or `--all`) from a repository given as a URL or path and
//! print `<oid> <name>` for each of them. No refs are written.
//!
//! Behaviour observed from git:
//! - options come before the repository; `-h` alone prints the usage on
//!   standard output, any other unknown option prints it on standard
//!   error (exit 129);
//! - a ref is named by its full name (`HEAD`, `refs/heads/main`) or, when
//!   it matches no advertised name, by a full object id; names that match
//!   nothing give `error: no such remote ref <name>` and exit code 1 after
//!   the fetch of the others;
//! - the matched refs are printed in the order of the remote's ref list,
//!   object ids asked for directly after them;
//! - a pack that is kept (`-k`, or at least `fetch.unpackLimit` objects)
//!   keeps its `.keep` file and is reported as `keep\t<checksum>`
//!   (`pack\t<checksum>` when that pack was there already)
//!   (`--lock-pack`: `lock <path of the .keep file>`);
//! - `--diag-url` prints how the URL is split for a native transport.

use std::collections::HashSet;
use std::io::{BufRead, Write};

use bstr::ByteSlice;
use rgit_config::SystemEnv;
use rgit_fetch::{ConnectSettings, LocalRepo, PackOptions, Session, graph, shallow};
use rgit_hash::ObjectId;
use rgit_protocol::ProtocolVersion;
use rgit_transport::IpVersion;

use crate::messages::fetch_pack as msg;
use crate::prelude::*;
use crate::repo::{self, Mode};

#[derive(Default)]
struct Args {
    all: bool,
    stdin: bool,
    keep: u32,
    lock_pack: bool,
    include_tag: bool,
    no_progress: bool,
    verbose: bool,
    diag_url: bool,
    check_self_contained: bool,
    upload_pack: Option<String>,
    depth: Option<u32>,
    shallow_since: Option<String>,
    shallow_exclude: Vec<String>,
    deepen_relative: bool,
    refetch: bool,
    update_shallow: bool,
    filter: Option<String>,
    url: String,
    refs: Vec<String>,
}

fn usage(code: i32) -> Die {
    let text = format!("{}\n", msg::USAGE);
    Die::Output(rgit_options::Exit {
        code,
        stdout: if code == 0 { text.clone() } else { String::new() },
        stderr: if code == 0 { String::new() } else { text },
    })
}

/// `atoi`: the leading decimal digits, 0 without any.
fn leading_number(s: &str) -> i64 {
    let s = s.trim_start();
    let (neg, digits) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let n: i64 = digits
        .bytes()
        .take_while(u8::is_ascii_digit)
        .fold(0i64, |n, d| n.saturating_mul(10).saturating_add(i64::from(d - b'0')));
    if neg { -n } else { n }
}

fn parse(argv: &[String]) -> Result<Args, Die> {
    let mut a = Args::default();
    let mut i = 1;
    while i < argv.len() {
        let arg = argv[i].as_str();
        if !arg.starts_with('-') {
            break;
        }
        i += 1;
        if let Some(v) = arg
            .strip_prefix("--upload-pack=")
            .or_else(|| arg.strip_prefix("--exec="))
        {
            a.upload_pack = Some(v.to_string());
            continue;
        }
        if let Some(v) = arg.strip_prefix("--depth=") {
            let n = leading_number(v);
            a.depth = (n > 0).then(|| n.min(i64::from(i32::MAX)) as u32);
            continue;
        }
        if let Some(v) = arg.strip_prefix("--shallow-since=") {
            a.shallow_since = Some(v.to_string());
            continue;
        }
        if let Some(v) = arg.strip_prefix("--shallow-exclude=") {
            a.shallow_exclude.push(v.to_string());
            continue;
        }
        if let Some(v) = arg.strip_prefix("--filter=") {
            a.filter = Some(v.to_string());
            continue;
        }
        match arg {
            "--quiet" | "-q" => {}
            "--keep" | "-k" => a.keep += 1,
            "--lock-pack" => {
                a.lock_pack = true;
                a.keep += 1;
            }
            "--thin" | "--cloning" | "--no-dependents" | "--from-promisor" | "--reject-shallow" => {}
            "--include-tag" => a.include_tag = true,
            "--all" => a.all = true,
            "--stdin" => a.stdin = true,
            "--diag-url" => a.diag_url = true,
            "-v" => a.verbose = true,
            "--deepen-relative" => a.deepen_relative = true,
            "--no-progress" => a.no_progress = true,
            "--check-self-contained-and-connected" => a.check_self_contained = true,
            "--update-shallow" => a.update_shallow = true,
            "--refetch" => a.refetch = true,
            "--no-filter" => a.filter = None,
            _ => return Err(usage(129)),
        }
    }
    let Some(url) = argv.get(i) else {
        return Err(usage(129));
    };
    a.url = url.clone();
    a.refs = argv[i + 1..].to_vec();
    Ok(a)
}

/// `--diag-url`: the URL as a native transport splits it.
fn diag(url: &str) -> CmdResult {
    let c = rgit_remote::url::parse_connect_url(url.as_bytes())
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let mut out = std::io::stdout().lock();
    let mut s = format!("Diag: url={url}\nDiag: protocol={}\n", c.protocol.name());
    if c.protocol == rgit_remote::Protocol::Ssh {
        s += &format!("Diag: userandhost={}\n", c.host);
        match &c.port {
            Some(p) => s += &format!("Diag: port={p}\n"),
            None => s += "Diag: port=NONE\n",
        }
    } else {
        s += &format!("Diag: hostandport={}\n", c.host);
    }
    s += &format!("Diag: path={}\n", c.path);
    let _ = out.write_all(s.as_bytes());
    Ok(0)
}

/// The capabilities `-v` reports for a v0/v1 server, in git's order.
const REPORTED_CAPS: &[&[&str]] = &[
    &["shallow"],
    &["multi_ack_detailed", "multi_ack"],
    &["side-band-64k", "side-band"],
    &["allow-tip-sha1-in-want"],
    &["allow-reachable-sha1-in-want"],
    &["thin-pack"],
    &["no-progress"],
    &["include-tag"],
    &["ofs-delta"],
    &["filter"],
    &["deepen-since"],
    &["deepen-not"],
    &["deepen-relative"],
];

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    if argv.len() == 2 && (argv[1] == "-h" || argv[1] == "--help-all") {
        return Err(usage(0));
    }
    let args = parse(argv)?;
    let setup = repo::setup(ctx, Mode::Required)?;
    if args.diag_url {
        return diag(&args.url);
    }
    let config = repo::config(ctx, &setup);
    let repo = LocalRepo::open(&setup, config).map_err(|e| Die::Fatal(e.to_string()))?;
    let fatal = |e: rgit_fetch::Error| Die::Fatal(e.to_string());

    // The refs asked for, duplicates dropped.
    let mut sought: Vec<String> = Vec::new();
    let mut add = |name: &str| {
        if !name.is_empty() && !sought.iter().any(|s| s == name) {
            sought.push(name.to_string());
        }
    };
    for r in &args.refs {
        add(r);
    }
    if args.stdin {
        let stdin = std::io::stdin();
        for line in stdin.lock().split(b'\n') {
            let line = line.map_err(|e| Die::Fatal(e.to_string()))?;
            add(line.trim().to_str_lossy().as_ref());
        }
    }

    let deepen_since = args.shallow_since.as_deref().map(|t| {
        let dctx = rgit_date::DateContext::from_env();
        rgit_date::approxidate(t, &dctx) as i64
    });
    let env = SystemEnv::default();
    let connect = ConnectSettings {
        env: &env,
        config: &repo.config,
        base_env: ctx.child_env(),
        upload_pack: args.upload_pack.clone(),
        ip: IpVersion::Any,
        verbose: false,
    };
    let negotiation = rgit_fetch::Negotiation::from_config(&repo.config).map_err(fatal)?;
    let mut session = Session::connect(&args.url, &connect, "fetch-pack").map_err(fatal)?;
    let refs = session.ls_refs(&[], &[]).map_err(fatal)?;
    let mut err = std::io::stderr().lock();
    if args.verbose && session.version() != ProtocolVersion::V2 {
        let caps = session.capabilities();
        if let Some(agent) = caps.agent() {
            let _ = writeln!(err, "{}", msg::server_version(agent));
        }
        for group in REPORTED_CAPS {
            if let Some(cap) = group.iter().find(|c| caps.contains(c)) {
                let _ = writeln!(err, "{}", msg::server_supports(cap));
            }
        }
    }

    // Matched refs in the remote's order, then object ids asked for.
    let mut matched: Vec<(String, ObjectId)> = Vec::new();
    let mut found: HashSet<&str> = HashSet::new();
    for r in &refs {
        let Some(oid) = r.oid else { continue };
        let name = r.name.to_str_lossy();
        let named = sought.iter().find(|s| s.as_bytes() == r.name.as_slice());
        if let Some(s) = named {
            found.insert(s.as_str());
        }
        if args.all || named.is_some() {
            matched.push((name.into_owned(), oid));
        }
    }
    let mut errors: Vec<String> = Vec::new();
    let v0 = session.version() != ProtocolVersion::V2;
    let caps = session.capabilities();
    let any_allowed = caps.contains("allow-tip-sha1-in-want")
        || caps.contains("allow-reachable-sha1-in-want")
        || caps.contains("allow-any-sha1-in-want");
    for s in &sought {
        if found.contains(s.as_str()) {
            continue;
        }
        let id = (s.len() == repo.hash.hex_len())
            .then(|| ObjectId::from_hex(repo.hash, s.as_bytes()).ok())
            .flatten();
        match id {
            Some(id) => {
                let advertised = refs.iter().any(|r| r.oid == Some(id));
                if v0 && !any_allowed && !advertised {
                    errors.push(rgit_fetch::messages::unadvertised_object(s));
                } else {
                    matched.push((s.clone(), id));
                }
            }
            None => errors.push(msg::no_such_remote_ref(s)),
        }
    }

    let deepens = args.depth.is_some() || deepen_since.is_some() || !args.shallow_exclude.is_empty();
    // Like git, an object counts as present only when a local ref reaches
    // it; others are asked for again.
    let local: Vec<ObjectId> = repo.list_refs("").into_iter().map(|(_, id)| id).collect();
    let complete = |oid: &ObjectId| {
        repo.has_object(oid)
            && (local.contains(oid)
                || local
                    .iter()
                    .any(|tip| graph::is_ancestor(&repo.odb, oid, tip)))
    };
    if args.verbose {
        let mut marked: Vec<ObjectId> = Vec::new();
        for oid in refs.iter().filter_map(|r| r.oid) {
            if !marked.contains(&oid)
                && repo.kind(&oid) == Some(rgit_object::ObjectKind::Commit)
                && complete(&oid)
            {
                marked.push(oid);
                let _ = writeln!(err, "{}", msg::marking_complete(&oid.to_hex()));
            }
        }
    }
    let mut wants: Vec<ObjectId> = Vec::new();
    for (name, oid) in &matched {
        if deepens || args.refetch || !complete(oid) {
            if args.verbose {
                let _ = writeln!(err, "{}", msg::want(&oid.to_hex(), name));
            }
            if !wants.contains(oid) {
                wants.push(*oid);
            }
        } else if args.verbose {
            let _ = writeln!(err, "{}", msg::already_have(&oid.to_hex(), name));
        }
    }
    if args.verbose && v0 && !wants.is_empty() {
        let _ = writeln!(err, "done");
    }
    drop(err);

    // A filtered pack leaves objects out on purpose.
    let filtered = args.filter.is_some() && session.supports_filter();
    if args.filter.is_some() && !filtered {
        eprintln!("warning: {}", rgit_protocol::messages::FILTER_NOT_RECOGNIZED);
    }
    let pack_opts = PackOptions {
        no_progress: args.no_progress,
        include_tag: args.include_tag,
        unpack_limit: (args.keep == 0).then(|| rgit_fetch::unpack_limit(&repo.config)),
        deepen: args.depth,
        deepen_relative: args.deepen_relative,
        deepen_since,
        deepen_not: args.shallow_exclude.clone(),
        negotiation,
        fsck: rgit_fetch::FsckCheck::from_config(
            &repo.config,
            repo.hash,
            rgit_fetch::messages::INDEX_PACK_FAILED,
        ),
        filter: args.filter.clone(),
        ..PackOptions::default()
    };
    // Advertised objects count as common only when a local ref reaches
    // them (an object merely present may lack its history).
    let advertised: Vec<ObjectId> = refs
        .iter()
        .filter_map(|r| r.oid)
        .filter(|id| complete(id))
        .collect();
    let haves = (!args.refetch).then_some(advertised.as_slice());
    let pack = if matched.is_empty() {
        None
    } else {
        session
            .fetch_pack(&repo, &wants, haves, &pack_opts)
            .map_err(fatal)?
    };
    let shallow_info = session.take_shallow_info();
    session.close().map_err(fatal)?;

    let current = repo.shallow();
    let mut new_shallow = None;
    if let Some(info) = &shallow_info {
        if deepens || args.update_shallow {
            new_shallow = Some(shallow::apply(&current, info));
        }
    }
    let boundary: HashSet<ObjectId> = match &new_shallow {
        Some(set) => set.iter().copied().collect(),
        None => current.iter().copied().collect(),
    };
    let received: HashSet<ObjectId> = pack.as_ref().map(|p| p.objects.clone()).unwrap_or_default();
    let connected = !filtered;
    if !filtered {
        let mut tips: Vec<ObjectId> = matched.iter().map(|(_, id)| *id).collect();
        if let (Some(_), Some(info)) = (&new_shallow, &shallow_info) {
            for id in &info.unshallow {
                tips.extend(
                    graph::commit_info(&repo.odb, id)
                        .map(|c| c.parents)
                        .unwrap_or_default(),
                );
            }
        }
        if graph::check_connected(&repo.odb, &tips, &received, &boundary).is_err() {
            if let Some(p) = &pack {
                p.release();
            }
            return Err(Die::Fatal(rgit_fetch::messages::did_not_send_all(&args.url)));
        }
    }
    if let Some(set) = &new_shallow {
        shallow::write(&repo.shallow_file(), set).map_err(fatal)?;
    }

    let mut out = std::io::stdout().lock();
    let mut text = String::new();
    if let Some(p) = &pack {
        if let Some(path) = &p.pack {
            if args.lock_pack {
                if let Some(keep) = &p.keep {
                    let cwd = std::env::current_dir().unwrap_or_default();
                    let shown = keep.strip_prefix(&cwd).unwrap_or(keep);
                    text += &format!("lock {}\n", shown.display());
                }
            } else {
                let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned());
                let checksum = stem.as_deref().and_then(|s| s.strip_prefix("pack-")).unwrap_or("");
                // `pack` when the pack (and its `.keep`) existed already.
                let word = if p.keep.is_some() { "keep" } else { "pack" };
                text += &format!("{word}\t{checksum}\n");
            }
            if args.check_self_contained && connected {
                text += "connectivity-ok\n";
            }
        }
    }
    for e in &errors {
        eprintln!("error: {e}");
    }
    for (name, oid) in &matched {
        text += &format!("{oid} {name}\n");
    }
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
    Ok(if errors.is_empty() && !matched.is_empty() { 0 } else { 1 })
}
