// SPDX-License-Identifier: MIT
//! `git remote set-head`.

use bstr::ByteSlice;
use rgit_protocol::v2::RemoteRef;
use rgit_refs::{RefStore, RefUpdate, Target, Transaction};

use super::common::Repo;
use crate::messages::remote as msg;
use crate::prelude::*;
use crate::refs_support::{Objects, UpdateHooks};

/// The branches the remote's `HEAD` may be: the target of a symbolic
/// `HEAD`, else every branch pointing at the same commit as a detached
/// one. Empty when the remote has no (born) `HEAD`.
pub fn head_candidates(refs: &[RemoteRef]) -> Vec<String> {
    let Some(head) = refs.iter().find(|r| r.name == b"HEAD") else {
        return Vec::new();
    };
    let Some(oid) = head.oid else {
        return Vec::new();
    };
    if let Some(target) = &head.symref_target {
        if let Some(b) = target.strip_prefix(b"refs/heads/") {
            return vec![b.to_str_lossy().into_owned()];
        }
    }
    refs.iter()
        .filter(|r| r.oid == Some(oid))
        .filter_map(|r| r.name.strip_prefix(b"refs/heads/"))
        .map(|b| b.to_str_lossy().into_owned())
        .collect()
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let auto = Cell::new(false);
    let delete = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE_SET_HEAD,
        vec![
            Opt::flag('a', "auto", &auto, msg::HELP_AUTO),
            Opt::flag('d', "delete", &delete, msg::HELP_DELETE_HEAD),
        ],
    );
    let args = parser.parse(argv)?.args;
    let (auto, delete) = (auto.get(), delete.get());
    let explicit = match (args.len(), auto, delete) {
        (2, false, false) => Some(args[1].clone()),
        (1, true, false) | (1, false, true) => None,
        _ => return Err(parser.usage_error().into()),
    };
    drop(parser);
    let name = args[0].as_str();
    let repo = Repo::open(ctx)?;
    let head = format!("refs/remotes/{name}/HEAD");

    if delete {
        let exists = matches!(repo.store.read_raw(&head), Ok(Some(_)));
        if !exists {
            return Ok(0);
        }
        let objects = Objects::open(&repo.setup);
        let mut hooks = UpdateHooks::new(&repo.setup, &repo.config, &objects);
        let mut tx = Transaction::new(&repo.store);
        let r = tx
            .push(RefUpdate::symref_delete(
                head.as_str(),
                repo.setup.format.object_format,
                None,
            ))
            .and_then(|()| tx.commit(&mut hooks));
        if let Err(e) = r {
            eprintln!("error: {e}");
            return Err(Die::Error(msg::could_not_delete(&head), 1));
        }
        return Ok(0);
    }

    let (branch, report) = match explicit {
        Some(b) => (b, false),
        None => {
            let remotes = repo.remotes()?;
            let remote = repo.remote(&remotes, name)?;
            let refs = repo.query(ctx, &remote)?;
            let candidates = head_candidates(&refs);
            match candidates.as_slice() {
                [] => return Err(Die::Error(msg::CANNOT_DETERMINE_HEAD.into(), 1)),
                [one] => (one.clone(), true),
                many => {
                    let mut text = msg::MULTIPLE_HEADS.to_owned();
                    for b in many {
                        text.push('\n');
                        text.push_str(&msg::set_head_choice(name, b));
                    }
                    return Err(Die::Error(text, 1));
                }
            }
        }
    };
    let target = format!("refs/remotes/{name}/{branch}");
    if !matches!(repo.store.resolve_oid(&target), Ok(Some(_))) {
        return Err(Die::Error(msg::not_a_valid_ref(&target), 1));
    }
    let previous = repo.store.read_raw(&head).ok().flatten();
    let objects = Objects::open(&repo.setup);
    let mut hooks = UpdateHooks::new(&repo.setup, &repo.config, &objects);
    hooks.mark_symbolic(&head);
    let mut tx = Transaction::new(&repo.store);
    let r = tx
        .push(
            RefUpdate::symref_update(head.as_str(), target.as_str(), None)
                .no_deref(true)
                .message(msg::SET_HEAD_REFLOG),
        )
        .and_then(|()| tx.commit(&mut hooks));
    if let Err(e) = r {
        eprintln!("error: {e}");
        return Err(Die::Error(msg::could_not_set_up(&head), 1));
    }
    if !report {
        return Ok(0);
    }
    let remote_prefix = format!("refs/remotes/{name}/");
    let line = match previous {
        None => msg::head_created(name, &branch),
        Some(Target::Symbolic(t)) if t == target => msg::head_unchanged(name, &branch),
        Some(Target::Symbolic(t)) => match t.strip_prefix(&remote_prefix) {
            Some(prev) => msg::head_changed(name, prev, &branch),
            None => msg::head_was_strange(name, &t, &branch),
        },
        Some(Target::Object(oid)) => msg::head_was_detached(name, &oid.to_hex(), &branch),
    };
    println!("{line}");
    let key = format!("remote.{name}.followremotehead");
    if let Ok(Some(v)) = repo.config.string(key.as_bytes()) {
        if v.eq_ignore_ascii_case(b"always") {
            repo.set(&key, "warn")?;
        }
    }
    Ok(0)
}
