// SPDX-License-Identifier: MIT
//! `git remote show` and `git remote prune`.

use std::fmt::Write as _;

use bstr::ByteSlice;
use rgit_fetch::LocalRepo;
use rgit_hash::ObjectId;
use rgit_protocol::push::AdvertisedRef;
use rgit_protocol::v2::RemoteRef;
use rgit_refs::{RefStore, Target};
use rgit_remote::{Direction, Remote, RefspecSet};

use super::common::{self, Rebase, Repo, short_branch};
use super::head::head_candidates;
use crate::messages::remote as msg;
use crate::prelude::*;
use crate::push::matching::{self, Local};
use crate::refs_support::Revs;

pub fn show(ctx: &mut Context, argv: &[String], verbose: bool) -> CmdResult {
    let no_query = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE_SHOW,
        vec![Opt::flag('n', None, &no_query, msg::HELP_NO_QUERY)],
    );
    let args = parser.parse(argv)?.args;
    drop(parser);
    if args.is_empty() {
        return super::list(ctx, verbose);
    }
    let no_query = no_query.get();
    let repo = Repo::open(ctx)?;
    let remotes = repo.remotes()?;
    let local = LocalRepo::open(&repo.setup, repo.config.clone())
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let revs = Revs::new(ctx, &repo.setup)?;
    for name in &args {
        let remote = repo.remote(&remotes, name)?;
        let refs = if no_query {
            None
        } else {
            Some(repo.query(ctx, &remote)?)
        };
        let text = render(&repo, &local, &revs, &remote, refs.as_deref())?;
        print!("{text}");
    }
    Ok(0)
}

/// The text of `show` for one remote; `refs` is `None` with `-n`.
fn render(
    repo: &Repo,
    local: &LocalRepo,
    revs: &Revs,
    remote: &Remote,
    refs: Option<&[RemoteRef]>,
) -> Result<String, Die> {
    let name = remote.name.to_str_lossy().into_owned();
    let pull = pull_section(repo, &name);
    let mut out = String::new();
    let _ = writeln!(out, "{}", msg::show_header(&name));
    let fetch_url = remote
        .fetch_url()
        .map(|u| u.to_str_lossy().into_owned())
        .unwrap_or_else(|| msg::NO_URL.to_owned());
    let _ = writeln!(out, "{}", msg::fetch_url(&fetch_url));
    for url in &remote.push_url {
        let _ = writeln!(out, "{}", msg::push_url(&url.to_str_lossy()));
    }
    match refs {
        None => {
            let _ = writeln!(out, "{}", msg::HEAD_NOT_QUERIED);
        }
        Some(refs) => match head_candidates(refs).as_slice() {
            [] => {
                let _ = writeln!(out, "{}", msg::HEAD_UNKNOWN);
            }
            [one] => {
                let _ = writeln!(out, "{}", msg::head_branch(one));
            }
            many => {
                let _ = writeln!(out, "{}", msg::HEAD_AMBIGUOUS);
                for b in many {
                    let _ = writeln!(out, "    {b}");
                }
            }
        },
    }

    // Remote branches.
    let branches = remote_branches(local, remote, &name, refs);
    if !branches.is_empty() {
        let _ = writeln!(
            out,
            "{}",
            msg::remote_branches(branches.len(), refs.is_none())
        );
        let width = branches.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
        for (n, status) in &branches {
            match status {
                Some(s) => {
                    let _ = writeln!(out, "    {n:<width$} {s}");
                }
                None => {
                    let _ = writeln!(out, "    {n}");
                }
            }
        }
    }

    out.push_str(&pull);

    // Push.
    if remote.mirror {
        let _ = writeln!(out, "{}", msg::MIRRORED_BY_PUSH);
        return Ok(out);
    }
    let lines: Vec<PushLine> = match refs {
        None => push_specs(remote),
        Some(refs) => push_status(local, revs, remote, refs)?,
    };
    if !lines.is_empty() {
        let _ = writeln!(out, "{}", msg::push_refs(lines.len(), refs.is_none()));
        let ws = lines.iter().map(|l| l.src.len()).max().unwrap_or(0);
        let wd = lines.iter().map(|l| l.dst.len()).max().unwrap_or(0);
        for l in &lines {
            match &l.status {
                Some(s) => {
                    let _ = writeln!(
                        out,
                        "    {:<ws$} {} {:<wd$} ({s})",
                        l.src, l.verb, l.dst
                    );
                }
                None => {
                    let _ = writeln!(out, "    {:<ws$} {} {}", l.src, l.verb, l.dst);
                }
            }
        }
    }
    Ok(out)
}

/// `(name, status)` of the remote branches; without a query, the local
/// remote-tracking branches and no status.
fn remote_branches(
    local: &LocalRepo,
    remote: &Remote,
    name: &str,
    refs: Option<&[RemoteRef]>,
) -> Vec<(String, Option<String>)> {
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    match refs {
        None => {
            for (r, _) in local.list_refs("") {
                if matches!(local.refs.read_raw(&r), Ok(Some(Target::Symbolic(_)))) {
                    continue;
                }
                for spec in remote.fetch.positive() {
                    let Some(src) = spec.map_dst(r.as_bytes()) else {
                        continue;
                    };
                    let src = src.to_str_lossy();
                    if let Some(b) = src.strip_prefix("refs/heads/") {
                        if !out.iter().any(|(n, _)| n == b) {
                            out.push((b.to_owned(), None));
                        }
                    }
                }
            }
        }
        Some(refs) => {
            for r in refs {
                let Some(short) = r.name.strip_prefix(b"refs/heads/") else {
                    continue;
                };
                if r.oid.is_none() {
                    continue;
                }
                let short = short.to_str_lossy().into_owned();
                if remote.fetch.is_excluded(&r.name) {
                    out.push((short, Some(msg::STATUS_SKIPPED.to_owned())));
                    continue;
                }
                let Some(dst) = remote.fetch.positive().find_map(|s| s.map_src(&r.name)) else {
                    continue;
                };
                let status = if local.resolve(&dst.to_str_lossy()).is_some() {
                    msg::STATUS_TRACKED.to_owned()
                } else {
                    msg::status_new(name)
                };
                out.push((short, Some(status)));
            }
            for (stale, _) in rgit_fetch::update::stale_refs(local, &remote.fetch, refs) {
                out.push((
                    stale.to_str_lossy().into_owned(),
                    Some(msg::STATUS_STALE.to_owned()),
                ));
            }
        }
    }
    out.sort();
    out
}

/// The "Local branches configured for 'git pull'" section.
fn pull_section(repo: &Repo, name: &str) -> String {
    let mut branches: Vec<common::Branch> = common::read_branches(&repo.config)
        .into_iter()
        .filter(|b| b.remote.as_deref() == Some(name) && !b.merge.is_empty())
        .collect();
    branches.sort_by(|a, b| a.name.cmp(&b.name));
    let width = branches.iter().map(|b| b.name.len()).max().unwrap_or(0);
    let any_rebase = branches.iter().any(|b| b.rebase != Rebase::No);
    let pad = if any_rebase { " " } else { "" };
    let mut shown = Vec::new();
    for b in &branches {
        if b.rebase != Rebase::No && b.merge.len() > 1 {
            eprintln!("error: {}", msg::invalid_rebase_merge(&b.name));
            continue;
        }
        shown.push(b);
    }
    if shown.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let _ = writeln!(out, "{}", msg::pull_branches(shown.len()));
    for b in shown {
        let first = short_branch(&b.merge[0]);
        let verb = match b.rebase {
            Rebase::No => {
                let _ = writeln!(out, "    {:<width$} {pad}{} {first}", b.name, msg::MERGES);
                for m in &b.merge[1..] {
                    let _ = writeln!(
                        out,
                        "    {:<width$} {pad}{} {}",
                        "",
                        msg::AND_WITH,
                        short_branch(m)
                    );
                }
                continue;
            }
            Rebase::Yes => msg::REBASES,
            Rebase::Interactive => msg::REBASES_INTERACTIVELY,
            Rebase::Merges => msg::REBASES_MERGES,
        };
        let _ = writeln!(out, "    {:<width$} {verb} {first}", b.name);
    }
    out
}

struct PushLine {
    src: String,
    verb: &'static str,
    dst: String,
    status: Option<&'static str>,
}

/// The configured push refspecs, as `show -n` lists them.
fn push_specs(remote: &Remote) -> Vec<PushLine> {
    if remote.push.items.is_empty() {
        return vec![PushLine {
            src: msg::MATCHING.into(),
            verb: msg::PUSHES_TO,
            dst: msg::MATCHING.into(),
            status: None,
        }];
    }
    let mut out = Vec::new();
    for spec in &remote.push.items {
        if spec.negative {
            continue;
        }
        let verb = if spec.force {
            msg::FORCES_TO
        } else {
            msg::PUSHES_TO
        };
        let (src, dst) = if spec.matching {
            (msg::MATCHING.to_owned(), msg::MATCHING.to_owned())
        } else {
            let src = spec.src.to_str_lossy().into_owned();
            let dst = spec
                .dst
                .as_ref()
                .map(|d| d.to_str_lossy().into_owned())
                .unwrap_or_else(|| src.clone());
            (src, dst)
        };
        out.push(PushLine {
            src,
            verb,
            dst,
            status: None,
        });
    }
    out
}

/// What a push would do with every matched ref, compared with the
/// remote's state.
fn push_status(
    local: &LocalRepo,
    revs: &Revs,
    remote: &Remote,
    refs: &[RemoteRef],
) -> Result<Vec<PushLine>, Die> {
    let advertised: Vec<AdvertisedRef> = refs
        .iter()
        .filter(|r| r.name.starts_with(b"refs/"))
        .filter_map(|r| {
            r.oid.map(|oid| AdvertisedRef {
                name: r.name.to_str_lossy().into_owned(),
                oid,
            })
        })
        .collect();
    let mut local_refs: Vec<(String, ObjectId)> = Vec::new();
    if let Some(h) = local.resolve("HEAD") {
        local_refs.push(("HEAD".to_owned(), h));
    }
    local_refs.extend(local.list_refs(""));
    let head = local.current_branch();
    let resolve = |s: &str| revs.resolve(s);
    let kind = |id: &ObjectId| local.kind(id);
    let lctx = Local {
        refs: &local_refs,
        head: head.as_deref(),
        resolve: &resolve,
        kind: &kind,
    };
    let specs = if remote.push.items.is_empty() {
        RefspecSet::parse([":"], Direction::Push).map_err(|e| Die::Fatal(e.to_string()))?
    } else {
        remote.push.clone()
    };
    let (list, errors) = matching::match_refs(&lctx, &advertised, &specs, None, false);
    for e in errors {
        eprintln!("error: {}", e.error);
    }
    let mut out = Vec::new();
    for r in list {
        let Some(src) = &r.src else { continue };
        let status = match (r.old, r.new) {
            (_, None) => msg::PUSH_DELETE,
            (None, Some(_)) => msg::PUSH_CREATE,
            (Some(old), Some(new)) if old == new => msg::PUSH_UP_TO_DATE,
            (Some(old), Some(new)) => {
                if local.has_object(&old) && revs.is_ancestor(&old, &new) {
                    msg::PUSH_FAST_FORWARD
                } else {
                    msg::PUSH_OUT_OF_DATE
                }
            }
        };
        out.push(PushLine {
            src: short_branch(src).to_owned(),
            verb: if r.force {
                msg::FORCES_TO
            } else {
                msg::PUSHES_TO
            },
            dst: short_branch(&r.dst).to_owned(),
            status: Some(status),
        });
    }
    out.sort_by(|a, b| a.src.cmp(&b.src));
    Ok(out)
}

pub fn prune(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let dry_run = Cell::new(0);
    let mut parser = Parser::new(
        msg::USAGE_PRUNE,
        vec![Opt::dry_run(&dry_run, msg::HELP_DRY_RUN)],
    );
    let args = parser.parse(argv)?.args;
    if args.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let dry_run = dry_run.get() > 0;
    let repo = Repo::open(ctx)?;
    let remotes = repo.remotes()?;
    let local = LocalRepo::open(&repo.setup, repo.config.clone())
        .map_err(|e| Die::Fatal(e.to_string()))?;
    for name in &args {
        let remote = repo.remote(&remotes, name)?;
        let refs = repo.query(ctx, &remote)?;
        let mut stale = rgit_fetch::update::stale_refs(&local, &remote.fetch, &refs);
        if stale.is_empty() {
            continue;
        }
        stale.sort();
        let mut out = String::new();
        let _ = writeln!(out, "{}", msg::pruning(name));
        let url = remote
            .fetch_url()
            .map(|u| u.to_str_lossy().into_owned())
            .unwrap_or_default();
        let _ = writeln!(out, "{}", msg::prune_url(&url));
        let mut deleted = Vec::new();
        for (r, oid) in &stale {
            let r = r.to_str_lossy().into_owned();
            let shown = r.strip_prefix("refs/remotes/").unwrap_or(&r).to_owned();
            if dry_run {
                let _ = writeln!(out, "{}", msg::would_prune(&shown));
                continue;
            }
            if let Err(e) = rgit_fetch::update::delete(&local, r.as_str().into(), *oid) {
                eprintln!("error: {e}");
                continue;
            }
            let _ = writeln!(out, "{}", msg::pruned(&shown));
            deleted.push(r);
        }
        // Symbolic refs left pointing at a deleted ref.
        for (sym, target) in symrefs(&repo) {
            if deleted.contains(&target) {
                let _ = writeln!(out, "{}", msg::dangling_after_prune(&sym, &target));
            }
        }
        print!("{out}");
    }
    Ok(0)
}

/// Every symbolic reference below `refs/` with its target.
fn symrefs(repo: &Repo) -> Vec<(String, String)> {
    repo.store
        .iter(&rgit_refs::IterOptions::prefix(""))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| match i {
            rgit_refs::IterItem::Ref(r) => match r.target {
                Target::Symbolic(t) => Some((r.name, t)),
                Target::Object(_) => None,
            },
            _ => None,
        })
        .collect()
}
