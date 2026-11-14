// SPDX-License-Identifier: MIT
//! `stash export` and `stash import`: the stash list as a chain of
//! commits that fetch and push can transfer.
//!
//! The chain (shape taken from the `git` binary): a root commit with the
//! empty tree, no message and a fixed identity and date (so every export
//! shares it), then one commit per entry, oldest first, each with the
//! empty tree, the previous chain commit as first parent, the stash
//! commit as second parent and the message `git stash: <stash subject>`.

use rgit_hash::ObjectId;
use rgit_object::{ObjectKind, Signature, SignatureRef};
use rgit_refs::{RefUpdate, Transaction};

use super::{
    STASH_REF, commit_parts, do_store, open, parse, stash_entries, stash_exists, write_commit,
};
use crate::messages::stash as msg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, UpdateHooks};
use crate::repository::Repo;

/// Date of the root commit of every export.
const BASE_TIME: &str = "1000684800 +0000";
/// Subject prefix of the chain commits.
const PREFIX: &[u8] = b"git stash: ";

fn base_signature() -> Signature {
    let raw = format!(
        "{} <{}> {BASE_TIME}",
        msg::FALLBACK_NAME,
        msg::FALLBACK_EMAIL
    );
    SignatureRef::parse(raw.as_bytes())
        .and_then(|s| s.to_owned())
        .expect("valid signature")
}

/// The root commit every export starts from.
fn base_commit(repo: &Repo) -> Result<ObjectId, Die> {
    // The empty tree is known without being stored.
    let empty = ObjectId::empty_tree(repo.hash);
    let sig = base_signature();
    let new = rgit_commit::NewCommit {
        tree: empty,
        parents: &[],
        author: &sig,
        committer: &sig,
        encoding: None,
        message: b"",
    };
    rgit_commit::create_commit(repo.odb(), &new).map_err(|e| Die::Fatal(e.to_string()))
}

fn commit_message(repo: &Repo, id: &ObjectId) -> Vec<u8> {
    match repo.read_object(id) {
        Some(rgit_object::Object::Commit(c)) => c.message().to_vec(),
        _ => Vec::new(),
    }
}

fn is_stash_like(repo: &Repo, id: &ObjectId) -> bool {
    commit_parts(repo, id).is_some_and(|(_, p)| p.len() >= 2)
}

/// A chain commit; author and committer (with their dates) are those of
/// the stash commit `w`, so an export of the same entries is the same
/// chain.
fn chain_commit(
    repo: &Repo,
    tree: ObjectId,
    parents: &[ObjectId],
    message: &[u8],
    w: &ObjectId,
) -> Result<ObjectId, Die> {
    let sigs = match repo.read_object(w) {
        Some(rgit_object::Object::Commit(c)) => c
            .author()
            .and_then(|a| a.to_owned())
            .zip(c.committer().and_then(|c| c.to_owned())),
        _ => None,
    };
    let Some((author, committer)) = sigs else {
        return write_commit(repo, tree, parents, message);
    };
    let new = rgit_commit::NewCommit {
        tree,
        parents,
        author: &author,
        committer: &committer,
        encoding: None,
        message,
    };
    rgit_commit::create_commit(repo.odb(), &new).map_err(|e| Die::Fatal(e.to_string()))
}

pub(crate) fn export(ctx: &mut Context, args: &[String]) -> CmdResult {
    let print = Cell::new(false);
    let to_ref = RefCell::new(None);
    let mut parser = Parser::new(
        &[msg::USAGE_EXPORT],
        vec![
            Opt::flag(None, "print", &print, msg::HELP_EXPORT_PRINT).noneg(),
            Opt::string(None, "to-ref", &to_ref, "ref", msg::HELP_EXPORT_TO_REF),
        ],
    );
    let rest = match parse(&mut parser, args) {
        Ok(r) => r,
        Err(Die::Exit(0)) => return Ok(0),
        Err(e) => return Err(e),
    };
    drop(parser);
    let to_ref = to_ref.into_inner();
    if print.get() == to_ref.is_some() {
        return Err(Die::Error(msg::EXPORT_PRINT_OR_REF.to_owned(), 128));
    }
    let mut repo = open(ctx)?;

    // The entries, newest first.
    let mut stashes: Vec<ObjectId> = Vec::new();
    if rest.is_empty() {
        if stash_exists(&mut repo) {
            stashes = stash_entries(&repo).iter().map(|e| e.new).collect();
        }
    } else {
        for a in &rest {
            let name = if !a.is_empty() && a.bytes().all(|b| b.is_ascii_digit()) {
                format!("{STASH_REF}@{{{a}}}")
            } else {
                a.clone()
            };
            let Some(id) = repo.resolve(&name) else {
                return Err(Die::Error(msg::no_stash_entry(a), 128));
            };
            if !is_stash_like(&repo, &id) {
                return Err(Die::Error(msg::not_a_stash_commit(a), 128));
            }
            stashes.push(id);
        }
    }

    let empty = ObjectId::empty_tree(repo.hash);
    let mut tip = base_commit(&repo)?;
    for w in stashes.iter().rev() {
        let mut message = PREFIX.to_vec();
        message.extend_from_slice(&commit_message(&repo, w));
        if message.last() != Some(&b'\n') {
            message.push(b'\n');
        }
        tip = chain_commit(&repo, empty, &[tip, *w], &message, w)?;
    }

    match to_ref {
        Some(name) => {
            if name.starts_with("refs/") {
                let objects = Objects::open(&repo.setup);
                let store = refs_support::ref_store(&repo.setup, &repo.config);
                let mut hooks = UpdateHooks::new(&repo.setup, &repo.config, &objects);
                let mut tx = Transaction::new(&store);
                tx.push(RefUpdate::update(name, tip, None))
                    .and_then(|()| tx.commit(&mut hooks))
                    .map_err(|e| Die::Error(e.to_string(), 128))?;
            }
        }
        None => println!("{tip}"),
    }
    Ok(0)
}

pub(crate) fn import(ctx: &mut Context, args: &[String]) -> CmdResult {
    let mut parser = Parser::new(&[msg::USAGE_IMPORT], vec![]);
    let rest = match parse(&mut parser, args) {
        Ok(r) => r,
        Err(Die::Exit(0)) => return Ok(0),
        Err(e) => return Err(e),
    };
    if rest.len() != 1 {
        return Err(Die::Output(parser.usage_msg(msg::REVISION_REQUIRED)));
    }
    drop(parser);
    let mut repo = open(ctx)?;
    let Some(start) = repo.resolve(&rest[0]) else {
        return Err(Die::Error(msg::not_a_valid_revision(&rest[0]), 128));
    };
    let Some(start) = repo.peel_to(start, ObjectKind::Commit) else {
        return Err(Die::Error(msg::not_valid_export(&start.to_hex()), 128));
    };
    let empty = ObjectId::empty_tree(repo.hash);
    let base = base_commit(&repo)?;
    if commit_parts(&repo, &start).is_none_or(|(t, _)| t != empty) {
        return Err(Die::Error(msg::not_valid_export(&start.to_hex()), 128));
    }

    // Walk to the root, newest first, checking every link.
    let mut items: Vec<(ObjectId, Vec<u8>)> = Vec::new();
    let mut cur = start;
    loop {
        let Some((tree, parents)) = commit_parts(&repo, &cur) else {
            return Err(Die::Error(msg::not_valid_export(&cur.to_hex()), 128));
        };
        if parents.is_empty() {
            if cur != base {
                return Err(Die::Error(msg::invalid_root(&cur.to_hex()), 128));
            }
            break;
        }
        let message = commit_message(&repo, &cur);
        if parents.len() != 2 || tree != empty || !message.starts_with(PREFIX) {
            return Err(Die::Error(msg::without_prefix(&cur.to_hex()), 128));
        }
        let w = parents[1];
        if !is_stash_like(&repo, &w) {
            return Err(Die::Error(msg::not_a_stash_commit(&w.to_hex()), 128));
        }
        // The reflog message is the stash commit's own message.
        let mut subject = commit_message(&repo, &w);
        while subject.last() == Some(&b'\n') {
            subject.pop();
        }
        items.push((w, subject));
        cur = parents[0];
    }
    for (w, subject) in items.iter().rev() {
        do_store(&repo, *w, &String::from_utf8_lossy(subject), false)?;
    }
    Ok(0)
}
