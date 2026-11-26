// SPDX-License-Identifier: MIT
//! `git refs` (Documentation/git-refs.adoc): low-level access to the
//! references. `list` is `for-each-ref`, `optimize` is `pack-refs`;
//! `exists`, `create`, `update`, `delete` and `rename` work on single
//! references like `update-ref`; `verify` checks the reference store
//! ([`verify`]); `migrate` converts it to the other storage format
//! ([`migrate`]).

#[path = "refs/migrate.rs"]
mod migrate;
#[path = "refs/verify.rs"]
pub(crate) mod verify;

use rgit_hash::ObjectId;
use rgit_refs::name::{RefNameFlags, check_refname_format};
use rgit_refs::{RefStore, RefUpdate, Target, Transaction};

use crate::messages::refs as msg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, UpdateHooks};
use crate::repo::{self, Mode};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let args = &argv[1.min(argv.len())..];
    let sub = args.first().map(String::as_str);
    let mut top = Parser::new(msg::USAGE, vec![]);
    match sub {
        Some("list") => {
            crate::commands::for_each_ref::run_with_usage(ctx, args, msg::USAGE_LIST)
        }
        Some("optimize") => {
            crate::commands::pack_refs::run_with_usage(ctx, args, msg::USAGE_OPTIMIZE)
        }
        Some("exists") => exists(ctx, args),
        Some(op @ ("create" | "update" | "delete")) => write(ctx, op, args),
        Some("rename") => rename(ctx, args),
        Some("verify") => verify::run(ctx, args),
        Some("migrate") => migrate::run(ctx, args),
        Some(s) if !s.starts_with('-') => {
            Err(top.error_with_usage(&msg::unknown_subcommand(s)).into())
        }
        Some(s) => {
            // `-h` and options before the subcommand.
            top.parse(&["refs".to_owned(), s.to_owned()])?;
            Err(top.error_with_usage(msg::NEED_SUBCOMMAND).into())
        }
        None => Err(top.error_with_usage(msg::NEED_SUBCOMMAND).into()),
    }
}

/// `refs exists <ref>`: 0 if the reference exists, 2 if not.
fn exists(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mut parser = Parser::new(&[msg::USAGE_EXISTS], vec![]);
    let args = parser.parse(argv)?.args;
    if args.len() != 1 {
        return Err(Die::Fatal(msg::EXISTS_REQUIRES_REF.to_owned()));
    }
    let setup = repo::setup(ctx, Mode::Required)?;
    let config = refs_support::load_config(ctx, &setup)?;
    let store = refs_support::ref_store(&setup, &config);
    crate::commands::show_ref::exists_mode(&store, &args)
}

/// `refs create|update|delete`.
fn write(ctx: &mut Context, op: &str, argv: &[String]) -> CmdResult {
    let message: RefCell<Option<String>> = RefCell::new(None);
    let no_deref = Cell::new(false);
    let create_reflog = Cell::new(false);
    let mut opts = vec![
        Opt::string(None, "message", &message, "reason", msg::HELP_MESSAGE),
        Opt::callback(None, "no-deref", None, msg::HELP_NO_DEREF, |_, _| {
            no_deref.set(true);
            Ok(())
        })
        .noarg()
        .noneg(),
        Opt::callback(None, "deref", None, msg::HELP_DEREF, |_, _| {
            no_deref.set(false);
            Ok(())
        })
        .noarg()
        .noneg(),
    ];
    let usage = match op {
        "create" => msg::USAGE_CREATE,
        "update" => msg::USAGE_UPDATE,
        _ => msg::USAGE_DELETE,
    };
    if op != "delete" {
        opts.push(Opt::flag(
            None,
            "create-reflog",
            &create_reflog,
            msg::HELP_CREATE_REFLOG,
        ));
    }
    let mut parser = Parser::new(&[usage], opts);
    let args = parser.parse(argv)?.args;
    drop(parser);
    let (min, max, wrong) = match op {
        "create" => (2, 2, msg::CREATE_ARGS),
        "update" => (2, 3, msg::UPDATE_ARGS),
        _ => (1, 2, msg::DELETE_ARGS),
    };
    if args.len() < min || args.len() > max {
        eprintln!("usage: {wrong}");
        return Err(Die::Exit(rgit_options::EXIT_USAGE));
    }
    let message = message.borrow().clone();
    if message.as_deref() == Some("") {
        return Err(Die::Fatal(msg::EMPTY_MESSAGE.to_owned()));
    }
    let message = message.unwrap_or_default();

    let setup = repo::setup(ctx, Mode::Required)?;
    let config = refs_support::load_config(ctx, &setup)?;
    let store = refs_support::ref_store(&setup, &config);
    let objects = Objects::open(&setup);
    let revs = refs_support::Revs::new(ctx, &setup)?;
    let mut hooks = UpdateHooks::new(&setup, &config, &objects);
    let hash = setup.format.object_format;
    let name = args[0].as_str();
    let parse = |value: &str, what: fn(&str) -> String| -> Result<ObjectId, Die> {
        revs.resolve(value)
            .ok_or_else(|| Die::Fatal(what(value)))
    };
    let update = match op {
        "create" => {
            let new = parse(&args[1], msg::invalid_object_id)?;
            if new.is_null() {
                return Err(Die::Fatal(msg::CREATE_NULL.to_owned()));
            }
            RefUpdate::create(name, new).create_reflog(create_reflog.get())
        }
        "update" => {
            let new = parse(&args[1], msg::invalid_new_object_id)?;
            let old = args
                .get(2)
                .map(|o| parse(o, msg::invalid_old_object_id))
                .transpose()?;
            if new.is_null() {
                RefUpdate::delete(name, hash, old)
            } else {
                RefUpdate::update(name, new, old).create_reflog(create_reflog.get())
            }
        }
        _ => {
            let old = args
                .get(1)
                .map(|o| parse(o, msg::invalid_old_object_id))
                .transpose()?;
            if old.is_some_and(|o| o.is_null()) {
                return Err(Die::Fatal(msg::DELETE_NULL.to_owned()));
            }
            RefUpdate::delete(name, hash, old)
        }
    };
    let update = update.no_deref(no_deref.get()).message(message.as_str());
    let mut tx = Transaction::new(&store);
    let result = tx.push(update).and_then(|()| tx.commit(&mut hooks));
    match result {
        Ok(()) => Ok(0),
        Err(e) if op == "delete" => Err(Die::Error(e.to_string(), 1)),
        Err(e) => Err(Die::Error(
            crate::messages::update_ref::update_ref_failed(name, &e.to_string()),
            1,
        )),
    }
}

/// `refs rename <old-ref> <new-ref>`: the value and the reflog move to the
/// new name, with one entry noting the rename.
fn rename(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let message: RefCell<Option<String>> = RefCell::new(None);
    let mut parser = Parser::new(
        &[msg::USAGE_RENAME],
        vec![Opt::string(None, "message", &message, "reason", msg::HELP_MESSAGE)],
    );
    let args = parser.parse(argv)?.args;
    drop(parser);
    let [old, new] = args.as_slice() else {
        eprintln!("usage: {}", msg::RENAME_ARGS);
        return Err(Die::Exit(rgit_options::EXIT_USAGE));
    };
    let message = message.borrow().clone();
    if message.as_deref() == Some("") {
        return Err(Die::Fatal(msg::EMPTY_MESSAGE.to_owned()));
    }
    let message = message.unwrap_or_default();
    let setup = repo::setup(ctx, Mode::Required)?;
    for n in [old, new] {
        if check_refname_format(n.as_bytes(), RefNameFlags::STRICT).is_err()
            || !n.starts_with("refs/")
        {
            return Err(Die::Fatal(msg::invalid_ref_format(n)));
        }
    }
    let config = refs_support::load_config(ctx, &setup)?;
    let store = refs_support::ref_store(&setup, &config);
    let objects = Objects::open(&setup);
    let hash = setup.format.object_format;
    let oid = match store.read_raw(old) {
        Ok(Some(Target::Object(id))) => id,
        Ok(Some(Target::Symbolic(_))) if store.resolve_oid(old).ok().flatten().is_some() => {
            return Err(Die::Error(msg::rename_symref(old), 1));
        }
        _ => return Err(Die::Fatal(msg::does_not_exist(old))),
    };
    if !matches!(store.read_raw(new), Ok(None)) {
        return Err(Die::Fatal(msg::already_exists(new)));
    }
    let hooks = UpdateHooks::new(&setup, &config, &objects);
    if store.is_reftable() {
        // One table with the deletion, the creation and the moved log.
        let Some(committer) = hooks.committer().cloned() else {
            return Err(Die::Fatal(msg::does_not_exist(old)));
        };
        return store
            .rename_reftable(old, new, &committer, &message, false)
            .map(|()| 0)
            .map_err(|e| Die::Fatal(e.to_string()));
    }
    let logs = hooks.reflogs().cloned();
    let had_log = logs.as_ref().is_some_and(|l| l.exists(old));
    if let Some(logs) = &logs {
        if had_log {
            logs.rename(old, new).map_err(|e| Die::Fatal(e.to_string()))?;
        }
    }
    let run = |update: RefUpdate, hooks: &mut UpdateHooks<'_>| -> Result<(), Die> {
        let mut tx = Transaction::new(&store);
        tx.push(update)
            .and_then(|()| tx.commit(hooks))
            .map_err(|e| Die::Fatal(e.to_string()))
    };
    // Deleting the old name notes it in the log of a symbolic reference
    // (`HEAD`) that points to it.
    let mut h = UpdateHooks::new(&setup, &config, &objects);
    run(
        RefUpdate::delete(old.as_str(), hash, Some(oid))
            .no_deref(true)
            .message(message.as_str()),
        &mut h,
    )?;
    let mut silent = UpdateHooks::new(&setup, &config, &objects).without_reflogs();
    run(RefUpdate::create(new.as_str(), oid).no_deref(true), &mut silent)?;
    let create = had_log
        || rgit_refs::LogAllRefUpdates::parse(
            &config
                .get("core.logallrefupdates")
                .and_then(|e| e.value.as_ref().map(|v| v.to_string()))
                .unwrap_or_default(),
        )
        .unwrap_or_else(|| rgit_refs::LogAllRefUpdates::default_for(setup.is_bare()))
        .should_autocreate(new);
    hooks.log(new, oid, oid, &message, create);
    Ok(0)
}
