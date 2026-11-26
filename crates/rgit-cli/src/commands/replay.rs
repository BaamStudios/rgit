// SPDX-License-Identifier: MIT
//! `git replay` (Documentation/git-replay.adoc, Documentation/config/
//! replay.adoc; experimental upstream): replay a range of commits onto a
//! new base (`--onto`), on top of a branch (`--advance`) or reverted on
//! top of a branch (`--revert`), without touching the index or a work
//! tree, and update the affected refs atomically or print the
//! `update-ref --stdin` commands (`--ref-action=print`).
//!
//! The commits are replayed oldest first (`--revert`: newest first), each
//! with a three-way merge of its tree against its original parent onto the
//! replayed parent. A commit whose changes are already present is dropped
//! (unless it was empty to begin with); a conflict ends the command with
//! exit status 1 and nothing updated.

use std::collections::{HashMap, HashSet};
use std::io::Write;

use rgit_hash::ObjectId;
use rgit_ident::ident::Role;
use rgit_object::{Commit, ObjectKind};
use rgit_refs::{RefUpdate, Transaction};

use crate::messages::replay as msg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, UpdateHooks};
use crate::repository::Repo;
use crate::sequencer::ident;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Onto,
    Advance,
    Revert,
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let contained = Cell::new(false);
    let onto: RefCell<Option<String>> = RefCell::new(None);
    let advance: RefCell<Option<String>> = RefCell::new(None);
    let revert: RefCell<Option<String>> = RefCell::new(None);
    let ref_name: RefCell<Option<String>> = RefCell::new(None);
    let ref_action: RefCell<Option<String>> = RefCell::new(None);
    let linearize = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag(None, "contained", &contained, msg::H_CONTAINED),
            Opt::string(None, "onto", &onto, "revision", msg::H_ONTO).noneg(),
            Opt::string(None, "advance", &advance, "branch", msg::H_ADVANCE).noneg(),
            Opt::string(None, "revert", &revert, "branch", msg::H_REVERT).noneg(),
            Opt::string(None, "ref", &ref_name, "branch", msg::H_REF).noneg(),
            Opt::string(None, "ref-action", &ref_action, "mode", msg::H_REF_ACTION).noneg(),
            Opt::flag(None, "linearize", &linearize, msg::H_LINEARIZE),
        ],
    )
    .flags(ParseFlags {
        keep_dashdash: true,
        keep_unknown_opt: true,
        ..Default::default()
    });
    let args = parser.parse(argv)?.args;
    let (onto, advance, revert) = (
        onto.take(),
        advance.take(),
        revert.take(),
    );
    let ref_name = ref_name.take();
    let given = [onto.is_some(), advance.is_some(), revert.is_some()];
    if given.iter().filter(|&&g| g).count() == 0 {
        eprintln!("error: {}", msg::NEED_MODE);
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let together = |a: &str, b: &str| Err(Die::Fatal(msg::together(a, b)));
    if onto.is_some() && advance.is_some() {
        return together("--onto", "--advance");
    }
    if onto.is_some() && revert.is_some() {
        return together("--onto", "--revert");
    }
    if advance.is_some() && revert.is_some() {
        return together("--advance", "--revert");
    }
    if contained.get() {
        if advance.is_some() {
            return together("--advance", "--contained");
        }
        if revert.is_some() {
            return together("--revert", "--contained");
        }
        if ref_name.is_some() {
            return together("--ref", "--contained");
        }
        if linearize.get() {
            return together("--linearize", "--contained");
        }
    }
    let mode = if onto.is_some() {
        Mode::Onto
    } else if advance.is_some() {
        Mode::Advance
    } else {
        Mode::Revert
    };

    let mut repo = Repo::open(ctx, crate::repo::Mode::Required)?;
    let print = match ref_action.take() {
        Some(v) => match v.as_str() {
            "print" => true,
            "update" => false,
            _ => return Err(Die::Fatal(msg::invalid_ref_action(&v))),
        },
        None => match repo
            .config
            .string("replay.refAction")
            .ok()
            .flatten()
            .map(|v| v.to_string())
            .as_deref()
        {
            Some("print") => true,
            Some("update") | None => false,
            Some(v) => return Err(Die::Fatal(msg::invalid_ref_action_config(v))),
        },
    };
    if let Some(r) = &ref_name
        && !(r.starts_with("refs/")
            && rgit_refs::name::is_valid_refname(
                r.as_bytes(),
                rgit_refs::name::RefNameFlags::STRICT,
            ))
    {
        return Err(Die::Error(msg::invalid_refname(r), 128));
    }

    // The base and the branch to advance.
    let (mut base, target_branch) = match mode {
        Mode::Onto => {
            let spec = onto.as_deref().unwrap_or_default();
            let id = repo
                .resolve(spec)
                .and_then(|id| repo.peel_to(id, ObjectKind::Commit))
                .ok_or_else(|| Die::Fatal(msg::bad_onto(spec)))?;
            (id, None)
        }
        Mode::Advance | Mode::Revert => {
            let name = if mode == Mode::Advance {
                advance.clone().unwrap_or_default()
            } else {
                revert.clone().unwrap_or_default()
            };
            let not_ref = || {
                Die::Fatal(if mode == Mode::Advance {
                    msg::ADVANCE_NOT_REF.into()
                } else {
                    msg::REVERT_NOT_REF.into()
                })
            };
            let full = branch_ref(&repo, &name).ok_or_else(not_ref)?;
            let id = repo
                .refs
                .resolve(&full)
                .and_then(|id| repo.peel_to(id, ObjectKind::Commit))
                .ok_or_else(not_ref)?;
            (id, Some((name, full)))
        }
    };

    // The revision arguments: positive tips and the walk.
    let dashdash = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let mut tips: Vec<(Option<String>, Option<ObjectId>)> = Vec::new();
    for a in &args[..dashdash] {
        if a.starts_with('-') {
            if a == "--branches" || a.starts_with("--branches=") || a == "--all" {
                for (name, id) in repo.refs.list("refs/heads/") {
                    tips.push((Some(name), Some(id)));
                }
            }
            continue;
        }
        // An argument that names no revision is ambiguous (there are no
        // paths here).
        let names: Vec<&str> = match a.split_once("...").or_else(|| a.split_once("..")) {
            Some((l, r)) => vec![l, r],
            None => vec![a.trim_start_matches('^').trim_end_matches("^!")],
        };
        for n in names {
            if !n.is_empty() && repo.resolve(n).is_none() {
                return Err(Die::Fatal(crate::messages::rev_list::ambiguous_argument(a)));
            }
        }
        if a.starts_with('^') {
            continue;
        }
        if let Some((l, r)) = a.split_once("...") {
            tips.push(tip(&mut repo, if l.is_empty() { "HEAD" } else { l }));
            tips.push(tip(&mut repo, if r.is_empty() { "HEAD" } else { r }));
        } else if let Some((_, r)) = a.split_once("..") {
            tips.push(tip(&mut repo, if r.is_empty() { "HEAD" } else { r }));
        } else if let Some(x) = a.strip_suffix("^!") {
            tips.push(tip(&mut repo, x));
        } else {
            tips.push(tip(&mut repo, a));
        }
    }
    match mode {
        Mode::Advance if tips.len() > 1 => {
            return Err(Die::Fatal(msg::ill_defined("--advance")));
        }
        Mode::Revert if tips.len() > 1 => {
            return Err(Die::Fatal(msg::ill_defined("--revert")));
        }
        Mode::Onto if ref_name.is_some() && tips.len() > 1 => {
            return Err(Die::Error(msg::REF_MULTIPLE.into(), 128));
        }
        _ => {}
    }
    if linearize.get() && tips.len() > 1 {
        return Err(Die::Error(msg::LINEARIZE_MULTIPLE.into(), 128));
    }
    let mut walk_args: Vec<String> = args[..dashdash].to_vec();
    walk_args.push("--reverse".into());
    walk_args.push("--topo-order".into());
    // The commits with their own parents (not the ones the walk rewrote).
    let mut walked: Vec<(ObjectId, Vec<ObjectId>)> = Vec::new();
    for id in crate::history::walk_commits(ctx, &walk_args)? {
        let parents = read_commit(&repo, &id)?
            .parents()
            .map_err(|_| bad_object(&id))?;
        walked.push((id, parents));
    }
    if walked.is_empty() {
        return Err(Die::Fatal(msg::NEED_COMMITS.into()));
    }
    if mode == Mode::Revert {
        walked.reverse();
    }
    if !linearize.get() && walked.iter().any(|(_, p)| p.len() > 1) {
        return Err(Die::Fatal(msg::MERGES_UNSUPPORTED.into()));
    }

    // Replay.
    let merge_opts = {
        let mut o = rgit_merge::Options::default();
        crate::commands::merge_tree::apply_rename_config(&repo.config, &mut o);
        o
    };
    let committer = ident(&repo, Role::Committer)?;
    let revert_author = if mode == Mode::Revert {
        Some(ident(&repo, Role::Author)?)
    } else {
        None
    };
    let hash = repo.hash;
    let empty_tree = hash.empty_tree();
    let mut mapped: HashMap<ObjectId, ObjectId> = HashMap::new();
    // Our side's renames carry over from one pick to the next.
    let mut renames = rgit_merge::RenameCache::default();
    let mut last = base;
    for (id, parents) in &walked {
        let orig = read_commit(&repo, id)?;
        let orig_tree = orig.tree().map_err(|_| bad_object(id))?;
        if parents.len() > 1 {
            // `--linearize`: the merge itself is dropped.
            mapped.insert(*id, last);
            continue;
        }
        let parent = parents.first().copied();
        let new_parent = match mode {
            Mode::Revert => last,
            _ if linearize.get() => last,
            _ => parent.and_then(|p| mapped.get(&p).copied()).unwrap_or(base),
        };
        let parent_tree = match parent {
            Some(p) => tree_of(&repo, &p)?,
            None => empty_tree,
        };
        let onto_tree = tree_of(&repo, &new_parent)?;
        let (merge_base, theirs) = if mode == Mode::Revert {
            (orig_tree, parent_tree)
        } else {
            (parent_tree, orig_tree)
        };
        let result =
            rgit_merge::merge_trees_cached(
                repo.odb(),
                &merge_base,
                &onto_tree,
                &theirs,
                &merge_opts,
                &mut renames,
            )
            .map_err(|e| {
                    for line in e.collect_errors().into_iter().flatten() {
                        eprintln!("error: {line}");
                    }
                    Die::Fatal(e.to_string())
                })?;
        if !result.clean {
            return Ok(1);
        }
        if result.tree == onto_tree && orig_tree != parent_tree {
            // Already present: dropped.
            mapped.insert(*id, new_parent);
            last = new_parent;
            continue;
        }
        let (author, message) = match &revert_author {
            Some(a) => (a.clone(), revert_message(orig.message(), id)),
            None => (
                orig.author()
                    .and_then(|a| a.to_owned())
                    .ok_or_else(|| bad_object(id))?,
                orig.message().to_vec(),
            ),
        };
        let encoding = orig.encoding().map(<[u8]>::to_vec);
        let commit = Commit::new(
            result.tree,
            &[new_parent],
            &author,
            &committer,
            encoding.as_deref(),
            Vec::new(),
            message,
        );
        let new = repo
            .odb()
            .write(ObjectKind::Commit, &commit.serialize())
            .map_err(|e| Die::Fatal(e.to_string()))?;
        mapped.insert(*id, new);
        last = new;
    }
    if mode != Mode::Onto {
        base = last;
    }

    // The refs to update, in the order their commits were replayed.
    let order: HashMap<ObjectId, usize> = walked
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (*id, i))
        .collect();
    let mut updates: Vec<(String, ObjectId, ObjectId)> = Vec::new();
    match (&target_branch, &ref_name) {
        (_, Some(r)) => {
            let old = repo.refs.resolve(r).unwrap_or(hash.null());
            let new = match (mode, tips.first().and_then(|t| t.1)) {
                (Mode::Onto, Some(t)) => mapped.get(&t).copied().unwrap_or(last),
                (Mode::Onto, None) => last,
                _ => base,
            };
            updates.push((r.clone(), new, old));
        }
        (Some((_, full)), None) => {
            let old = repo.refs.resolve(full).unwrap_or(hash.null());
            updates.push((full.clone(), base, old));
        }
        _ => {
            let mut refs: Vec<(usize, String, ObjectId)> = Vec::new();
            let mut seen: HashSet<String> = HashSet::new();
            let mut add = |name: String, id: ObjectId, refs: &mut Vec<(usize, String, ObjectId)>| {
                if let Some(&pos) = order.get(&id)
                    && seen.insert(name.clone())
                {
                    refs.push((pos, name, id));
                }
            };
            for (name, id) in tips.iter().cloned() {
                if let (Some(name), Some(id)) = (name, id) {
                    add(name, id, &mut refs);
                }
            }
            if contained.get() {
                for (name, id) in repo.refs.list("refs/heads/") {
                    add(name, id, &mut refs);
                }
            }
            refs.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
            for (_, name, old) in refs {
                let new = mapped.get(&old).copied().unwrap_or(old);
                updates.push((name, new, old));
            }
        }
    }

    if print {
        let mut out = Vec::new();
        for (name, new, old) in &updates {
            out.extend_from_slice(format!("update {name} {new} {old}\n").as_bytes());
        }
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(&out);
        let _ = stdout.flush();
        return Ok(0);
    }
    let reflog = match (&mode, &target_branch) {
        (Mode::Onto, _) => msg::reflog_onto(&base.to_string()),
        (Mode::Advance, Some((name, _))) => msg::reflog_branch("advance", name),
        (_, Some((name, _))) => msg::reflog_branch("revert", name),
        _ => String::new(),
    };
    let setup = &repo.setup;
    let config = refs_support::load_config(ctx, setup)?;
    let store = refs_support::ref_store(setup, &config);
    let objects = Objects::open(setup);
    let mut hooks = UpdateHooks::new(setup, &config, &objects);
    let mut tx = Transaction::new(&store);
    for (name, new, old) in updates {
        let update = if old.is_null() {
            RefUpdate::create(&name, new)
        } else {
            RefUpdate::update(&name, new, Some(old))
        };
        tx.push(update.message(reflog.clone()))
            .map_err(|e| Die::Fatal(e.to_string()))?;
    }
    tx.commit(&mut hooks)
        .map_err(|e| Die::Fatal(e.to_string()))?;
    Ok(0)
}

/// The full name of the branch `name` (`refs/heads/<name>` or a full
/// `refs/...` name that exists).
fn branch_ref(repo: &Repo, name: &str) -> Option<String> {
    if name.starts_with("refs/") && repo.refs.resolve(name).is_some() {
        return Some(name.to_owned());
    }
    let full = format!("refs/heads/{name}");
    repo.refs.resolve(&full).map(|_| full)
}

/// A positive revision: the ref to update (a branch or `HEAD`) and its
/// commit.
fn tip(repo: &mut Repo, spec: &str) -> (Option<String>, Option<ObjectId>) {
    let id = repo
        .resolve(spec)
        .and_then(|id| repo.peel_to(id, ObjectKind::Commit));
    let name = if spec == "HEAD" {
        Some("HEAD".to_owned())
    } else {
        branch_ref(repo, spec).filter(|n| n.starts_with("refs/heads/"))
    };
    (name, id)
}

fn bad_object(id: &ObjectId) -> Die {
    Die::Fatal(crate::messages::rev_list::bad_object(&id.to_hex()))
}

fn read_commit(repo: &Repo, id: &ObjectId) -> Result<Commit, Die> {
    let raw = repo
        .odb()
        .read(id)
        .ok()
        .flatten()
        .ok_or_else(|| bad_object(id))?;
    Commit::parse(repo.hash, &raw.data).map_err(|_| bad_object(id))
}

fn tree_of(repo: &Repo, id: &ObjectId) -> Result<ObjectId, Die> {
    read_commit(repo, id)?.tree().map_err(|_| bad_object(id))
}

/// `Revert "<subject>"` (or `Reapply` for a revert) and the reverted id.
fn revert_message(message: &[u8], id: &ObjectId) -> Vec<u8> {
    let subject = crate::sequencer::subject_line(message);
    let mut out = Vec::new();
    match subject
        .strip_prefix(b"Revert \"")
        .and_then(|s| s.strip_suffix(b"\""))
    {
        Some(inner) => {
            out.extend_from_slice(b"Reapply \"");
            out.extend_from_slice(inner);
        }
        None => {
            out.extend_from_slice(b"Revert \"");
            out.extend_from_slice(subject);
        }
    }
    out.extend_from_slice(b"\"\n\nThis reverts commit ");
    out.extend_from_slice(id.to_hex().as_bytes());
    out.extend_from_slice(b".\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revert_messages() {
        let id = rgit_hash::HashKind::Sha1.empty_tree();
        let m = revert_message(b"Fix it\n\nbody\n", &id);
        assert_eq!(
            m,
            format!("Revert \"Fix it\"\n\nThis reverts commit {id}.\n").into_bytes()
        );
        let m = revert_message(b"Revert \"Fix it\"\n", &id);
        assert!(m.starts_with(b"Reapply \"Fix it\"\n\n"));
    }
}
