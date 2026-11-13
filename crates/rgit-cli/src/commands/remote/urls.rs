// SPDX-License-Identifier: MIT
//! `git remote set-branches`, `get-url` and `set-url`.

use rgit_config::ValuePattern;

use super::add::tracking_refspec;
use super::common::{self, Repo};
use crate::messages::remote as msg;
use crate::prelude::*;

pub fn set_branches(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let add = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE_SET_BRANCHES,
        vec![Opt::flag(None, "add", &add, msg::HELP_ADD_BRANCH)],
    );
    let args = parser.parse(argv)?.args;
    let Some((name, branches)) = args.split_first() else {
        return Err(parser.error_with_usage(msg::NO_REMOTE_SPECIFIED).into());
    };
    drop(parser);
    let repo = Repo::open(ctx)?;
    if !repo.exists(name) {
        return Err(Die::Error(msg::no_such_remote(name), 2));
    }
    let key = format!("remote.{name}.fetch");
    if !add.get() && !repo.unset_all(&key)? {
        // Like git: nothing to replace is a (silent) failure.
        return Ok(1);
    }
    let mirror = repo
        .config
        .bool(format!("remote.{name}.mirror").as_bytes())
        .ok()
        .flatten()
        .unwrap_or(false);
    for b in branches {
        repo.add(&key, &tracking_refspec(name, b, mirror))?;
    }
    Ok(0)
}

pub fn get_url(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let push = Cell::new(false);
    let all = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE_GET_URL,
        vec![
            Opt::flag(None, "push", &push, msg::HELP_GET_PUSH),
            Opt::flag(None, "all", &all, msg::HELP_GET_ALL),
        ],
    );
    let args = parser.parse(argv)?.args;
    if args.len() != 1 {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let name = args[0].as_str();
    let repo = Repo::open(ctx)?;
    if !repo.exists(name) {
        return Err(Die::Error(msg::no_such_remote(name), 2));
    }
    let remotes = repo.remotes()?;
    let remote = repo.remote(&remotes, name)?;
    let urls = if push.get() {
        &remote.push_url
    } else {
        &remote.url
    };
    let mut out = String::new();
    for url in urls.iter().take(if all.get() { usize::MAX } else { 1 }) {
        out.push_str(&format!("{url}\n"));
    }
    print!("{out}");
    Ok(0)
}

pub fn set_url(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let push = Cell::new(false);
    let add = Cell::new(false);
    let delete = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE_SET_URL,
        vec![
            Opt::flag(None, "push", &push, msg::HELP_SET_PUSH),
            Opt::flag(None, "add", &add, msg::HELP_SET_ADD),
            Opt::flag(None, "delete", &delete, msg::HELP_SET_DELETE),
        ],
    );
    let args = parser.parse(argv)?.args;
    let (add, delete, push) = (add.get(), delete.get(), push.get());
    if add && delete {
        return Err(Die::Fatal(msg::ADD_DELETE.into()));
    }
    let ok = if add || delete {
        args.len() == 2
    } else {
        args.len() == 2 || args.len() == 3
    };
    if !ok {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let name = args[0].as_str();
    let new = args[1].as_str();
    let old = args.get(2).map(String::as_str);
    let repo = Repo::open(ctx)?;
    if !repo.exists(name) {
        return Err(Die::Error(msg::no_such_remote(name), 2));
    }
    let key = format!("remote.{name}.{}", if push { "pushurl" } else { "url" });
    if add {
        repo.add(&key, new)?;
        return Ok(0);
    }
    let values: Vec<String> = repo
        .config
        .get_all(key.as_bytes())
        .filter_map(|e| e.value.as_ref().map(|v| v.to_string()))
        .collect();
    if delete {
        let pattern = ValuePattern::regex(new.as_bytes())
            .map_err(|_| Die::Fatal(msg::invalid_url_pattern(new)))?;
        let matches = values
            .iter()
            .filter(|v| pattern.matches(Some(v.as_bytes())))
            .count();
        if !push && matches == values.len() {
            return Err(Die::Fatal(msg::DELETE_ALL_URLS.into()));
        }
        return match repo.set_request(&key, None, &pattern, true) {
            Ok(()) => Ok(0),
            Err(e) if common::is_nothing_to_unset(&e) => {
                Err(Die::Fatal(msg::could_not_unset(&key)))
            }
            Err(e) => Err(common::write_die(e)),
        };
    }
    let pattern = match old {
        Some(old) => {
            let p = ValuePattern::regex(old.as_bytes())
                .map_err(|_| Die::Fatal(msg::invalid_url_pattern(old)))?;
            if !values.iter().any(|v| p.matches(Some(v.as_bytes()))) {
                return Err(Die::Fatal(msg::no_such_url(old)));
            }
            p
        }
        None => ValuePattern::Any,
    };
    match repo.set_request(&key, Some(new), &pattern, false) {
        Ok(()) => Ok(0),
        Err(e) if matches!(e.edit_error(), Some(rgit_config::EditError::MultipleValues { .. })) => {
            eprintln!("warning: {}", msg::has_multiple_values(&key));
            Err(Die::Fatal(msg::could_not_set(&key, new)))
        }
        Err(e) => Err(common::write_die(e)),
    }
}
