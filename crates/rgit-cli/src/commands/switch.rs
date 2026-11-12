// SPDX-License-Identifier: MIT
//! `git switch` (Documentation/git-switch.adoc), first slice of #42:
//! `-c <new-branch>` from HEAD, `<branch>` and `--detach <commit>` when the
//! target tree equals the HEAD tree, so index and work tree stay as they
//! are. Anything that would have to rewrite the work tree is refused until
//! unpack-trees (#107) lands.

use rgit_hash::ObjectId;
use rgit_object::ObjectKind;
use rgit_refs::name::{RefNameFlags, is_valid_refname};
use rgit_refs::{RefUpdate, Transaction};

use crate::messages::switch as msg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, Revs, UpdateHooks};
use crate::repo::{self, Mode};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let create = RefCell::new(None);
    let detach = Cell::new(false);
    let quiet = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::string('c', "create", &create, "branch", msg::HELP_CREATE),
            Opt::flag('d', "detach", &detach, msg::HELP_DETACH),
            Opt::flag('q', "quiet", &quiet, msg::HELP_QUIET),
        ],
    );
    let args = parser.parse(argv)?.args;
    let create = create.borrow().clone();
    if args.len() > 1 || (create.is_none() && args.is_empty()) {
        return Err(parser.usage_error().into());
    }

    let setup = repo::setup(ctx, Mode::Required)?;
    let config = refs_support::load_config(ctx, &setup)?;
    let store = refs_support::ref_store(&setup, &config);
    let objects = Objects::open(&setup);
    let revs = Revs::new(ctx, &setup)?;
    let mut hooks = UpdateHooks::new(&setup, &config, &objects);

    let head_text = std::fs::read_to_string(setup.git_dir.join("HEAD")).unwrap_or_default();
    let head_branch = head_text
        .trim()
        .strip_prefix("ref:")
        .map(|r| r.trim().to_owned());
    let head_oid = revs
        .resolve("HEAD")
        .ok_or_else(|| Die::Fatal(msg::UNBORN_UNSUPPORTED.to_owned()))?;
    let from = match &head_branch {
        Some(r) => r.strip_prefix("refs/heads/").unwrap_or(r).to_owned(),
        None => head_oid.to_hex(),
    };
    let say = |s: String| {
        if !quiet.get() {
            eprintln!("{s}");
        }
    };
    let commit = |tx_update: RefUpdate, hooks: &mut UpdateHooks<'_>| -> Result<(), Die> {
        let mut tx = Transaction::new(&store);
        tx.push(tx_update)
            .and_then(|()| tx.commit(hooks))
            .map_err(|e| Die::Fatal(e.to_string()))
    };

    if let Some(name) = create {
        let full = format!("refs/heads/{name}");
        if name == "HEAD"
            || name.starts_with('-')
            || !is_valid_refname(full.as_bytes(), RefNameFlags::STRICT)
        {
            return Err(Die::Fatal(msg::not_valid_branch_name(&name)));
        }
        if revs.resolve(&full).is_some() {
            return Err(Die::Fatal(msg::branch_exists(&name)));
        }
        let mut start_oid = head_oid;
        if let Some(start) = args.first() {
            start_oid = revs
                .resolve(start)
                .and_then(|id| objects.peel(&id))
                .filter(|id| objects.kind(id) == Some(ObjectKind::Commit))
                .ok_or_else(|| Die::Fatal(msg::invalid_ref(start)))?;
            if commit_tree(&objects, &start_oid) != commit_tree(&objects, &head_oid) {
                return Err(Die::Fatal(msg::TREE_CHANGE_UNSUPPORTED.to_owned()));
            }
        }
        let hash = setup.format.object_format;
        let reason = match args.first() {
            Some(s) => format!("branch: Created from {s}"),
            None => msg::BRANCH_CREATED_FROM_HEAD.to_owned(),
        };
        commit(
            RefUpdate::update(full.as_str(), start_oid, Some(ObjectId::null(hash)))
                .message(reason.as_str()),
            &mut hooks,
        )?;
        hooks.mark_symbolic("HEAD");
        let reason = msg::reflog_moving(&from, &name);
        commit(
            RefUpdate::symref_update("HEAD", full.as_str(), None)
                .no_deref(true)
                .message(reason.as_str()),
            &mut hooks,
        )?;
        say(msg::switched_new(&name));
        return Ok(0);
    }

    let target = &args[0];
    let branch_ref = format!("refs/heads/{target}");
    let branch_oid = if detach.get() {
        None
    } else {
        revs.resolve(&branch_ref)
    };
    let new_oid = match branch_oid {
        Some(id) => id,
        None => {
            if !detach.get() {
                return Err(Die::Fatal(msg::invalid_ref(target)));
            }
            revs.resolve(target)
                .and_then(|id| objects.peel(&id))
                .filter(|id| objects.kind(id) == Some(ObjectKind::Commit))
                .ok_or_else(|| Die::Fatal(msg::invalid_ref(target)))?
        }
    };
    if commit_tree(&objects, &new_oid) != commit_tree(&objects, &head_oid) {
        return Err(Die::Fatal(msg::TREE_CHANGE_UNSUPPORTED.to_owned()));
    }
    let reason = msg::reflog_moving(&from, target);
    if head_branch.is_none() && head_oid != new_oid {
        say(msg::previous_head(
            &objects.abbrev(&head_oid, 7),
            &subject(&objects, &head_oid),
        ));
    }
    if branch_oid.is_some() {
        hooks.mark_symbolic("HEAD");
        commit(
            RefUpdate::symref_update("HEAD", branch_ref.as_str(), None)
                .no_deref(true)
                .message(reason.as_str()),
            &mut hooks,
        )?;
        if head_branch.as_deref() == Some(branch_ref.as_str()) {
            say(msg::already_on(target));
        } else {
            say(msg::switched(target));
        }
    } else {
        commit(
            RefUpdate::update("HEAD", new_oid, None)
                .no_deref(true)
                .message(reason.as_str()),
            &mut hooks,
        )?;
        say(msg::head_now_at(
            &objects.abbrev(&new_oid, 7),
            &subject(&objects, &new_oid),
        ));
    }
    Ok(0)
}

/// The `tree` line of commit `id`.
fn commit_tree(objects: &Objects, id: &ObjectId) -> Option<Vec<u8>> {
    let (_, data) = objects.read(id)?;
    let line = data.split(|&b| b == b'\n').next()?;
    line.strip_prefix(b"tree ").map(<[u8]>::to_vec)
}

/// The subject of commit `id`: the first paragraph of its message, lines
/// joined with spaces.
fn subject(objects: &Objects, id: &ObjectId) -> String {
    let Some((_, data)) = objects.read(id) else {
        return String::new();
    };
    let text = String::from_utf8_lossy(&data);
    let body = text.split_once("\n\n").map(|(_, b)| b).unwrap_or("");
    body.lines()
        .skip_while(|l| l.trim().is_empty())
        .take_while(|l| !l.trim().is_empty())
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ")
}
