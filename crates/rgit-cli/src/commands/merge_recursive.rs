// SPDX-License-Identifier: MIT
//! `git merge-recursive` (Documentation/git-merge.adoc "MERGE
//! STRATEGIES", the strategy program behind `merge -s recursive`):
//! `merge-recursive [--<option>...] <base>... -- <head> <remote>` merges
//! `<remote>` into the index and work tree at `<head>` with the merge
//! engine ([`crate::sequencer::merge_with`]); exit 0 when clean, 1 with
//! conflicts. Custom strategy programs call it (t3436). The usage and
//! the messages were taken from the behaviour of the `git` binary.

use rgit_hash::ObjectId;

use crate::messages::merge as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;
use crate::sequencer::{self, Fail};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    run_with(ctx, argv, None, msg::MERGE_RECURSIVE_USAGE)
}

/// `merge-recursive` with the engine's `strategy` (`subtree` for
/// `merge-subtree`) and that command's `usage`.
pub(crate) fn run_with(ctx: &mut Context, argv: &[String], strategy: Option<&str>, usage: &str) -> CmdResult {
    let args = &argv[1..];
    if matches!(args.first().map(String::as_str), Some("-h" | "--help-all")) {
        println!("{usage}");
        return Ok(0);
    }
    if args.len() < 3 {
        eprintln!("{usage}");
        return Ok(129);
    }
    let mut xopts = Vec::new();
    let mut bases = Vec::new();
    let mut rest = args.iter();
    for a in rest.by_ref() {
        if a == "--" {
            break;
        }
        match a.strip_prefix("--") {
            Some(x) => xopts.push(x.to_owned()),
            None => bases.push(a.clone()),
        }
    }
    let heads: Vec<&String> = rest.collect();
    for x in &xopts {
        use crate::commands::merge_tree::{StrategyOptionError, apply_strategy_option};
        let mut probe = rgit_merge::Options::default();
        if let Err(StrategyOptionError::Unknown) = apply_strategy_option(&mut probe, x) {
            return Err(Die::Fatal(msg::merge_recursive_unknown_option(x)));
        }
    }
    if heads.len() != 2 {
        return Err(Die::Fatal(msg::MERGE_RECURSIVE_TWO_HEADS.to_owned()));
    }
    let mut repo = Repo::open(ctx, Mode::Required)?;
    repo.work_tree()?;
    let mut commit = |name: &str| -> Result<ObjectId, Die> {
        repo.resolve(name)
            .and_then(|id| repo.peel_to(id, rgit_object::ObjectKind::Commit))
            .ok_or_else(|| Die::Fatal(msg::merge_recursive_bad_object(name)))
    };
    let head = commit(heads[0])?;
    let remote = commit(heads[1])?;
    let base = match bases.first() {
        Some(b) => Some(commit(b)?),
        None => None,
    };
    // The conflict markers name the heads as given (`GITHEAD_<id>` is
    // not used here, observed).
    let their_label = heads[1].clone();
    let fail = |f: Fail| match f {
        Fail::Die(d) => d,
        Fail::Error(m) => Die::Error(m, 2),
        Fail::Printed => Die::Exit(2),
    };
    let head_tree = sequencer::tree_of(&repo, &head).map_err(fail)?;
    let remote_tree = sequencer::tree_of(&repo, &remote).map_err(fail)?;
    let base_tree = match base {
        Some(b) => sequencer::tree_of(&repo, &b).map_err(fail)?,
        None => ObjectId::empty_tree(repo.hash),
    };
    let base_label = bases
        .first()
        .cloned()
        .unwrap_or_else(|| "empty tree".to_owned());
    let result = sequencer::merge_with(
        &repo,
        strategy,
        &xopts,
        &base_tree,
        &head_tree,
        &remote_tree,
        (&base_label, &their_label),
    )
    .map_err(fail)?;
    sequencer::two_way(&repo, head_tree, result.tree, Some(&result), false).map_err(fail)?;
    let mut out = Vec::new();
    for m in &result.messages {
        out.extend_from_slice(&m.text);
        out.push(b'\n');
    }
    {
        use std::io::Write as _;
        let mut o = std::io::stdout().lock();
        let _ = o.write_all(&out);
        let _ = o.flush();
    }
    Ok(if result.clean { 0 } else { 1 })
}
