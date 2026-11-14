// SPDX-License-Identifier: MIT
//! `git remote add`.

use rgit_refs::{RefUpdate, Transaction};

use super::common::{self, Repo};
use super::update;
use crate::messages::remote as msg;
use crate::prelude::*;
use crate::refs_support::{Objects, UpdateHooks};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mirror {
    None,
    Fetch,
    Push,
    Both,
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let fetch = Cell::new(false);
    let tags = Cell::new(-1);
    let track: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let master: RefCell<Option<String>> = RefCell::new(None);
    let mirror = Cell::new(Mirror::None);
    let mut parser = Parser::new(
        msg::USAGE_ADD,
        vec![
            Opt::flag('f', "fetch", &fetch, msg::HELP_FETCH),
            Opt::bool(None, "tags", &tags, msg::HELP_TAGS),
            Opt::string_list('t', "track", &track, "branch", msg::HELP_TRACK),
            Opt::string('m', "master", &master, "branch", msg::HELP_MASTER),
            Opt::callback(
                None,
                "mirror",
                Some(msg::ARGH_MIRROR),
                msg::HELP_MIRROR,
                |arg, unset| {
                    let m = match (unset, arg) {
                        (true, _) => Mirror::None,
                        (false, None) => {
                            eprintln!("warning: {}", msg::MIRROR_DEPRECATED);
                            Mirror::Both
                        }
                        (false, Some("fetch")) => Mirror::Fetch,
                        (false, Some("push")) => Mirror::Push,
                        (false, Some(other)) => {
                            return Err(CallbackError::Error(msg::unknown_mirror(other)));
                        }
                    };
                    mirror.set(m);
                    Ok(())
                },
            )
            .optional(None)
            .literal_argh(),
        ],
    );
    let args = parser.parse(argv)?.args;
    if args.len() != 2 {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let (name, url) = (args[0].as_str(), args[1].as_str());
    let mirror = mirror.get();
    let track = track.take();
    let master = master.take();
    if mirror != Mirror::None && master.is_some() {
        return Err(Die::Fatal(msg::MASTER_WITH_MIRROR.into()));
    }
    if mirror == Mirror::Push && !track.is_empty() {
        return Err(Die::Fatal(msg::TRACK_WITH_PUSH_MIRROR.into()));
    }

    let repo = Repo::open(ctx)?;
    let remotes = repo.remotes()?;
    if repo.exists(name) {
        if repo.legacy_file(name).is_some() && !repo.configured_in_repo(name) {
            // Reading the legacy definition warns about it.
            repo.remote(&remotes, name)?;
        }
        return Err(Die::Error(msg::already_exists(name), 3));
    }
    if !common::valid_name(name) {
        return Err(Die::Fatal(msg::invalid_name(name)));
    }
    for other in super::remote_names(&repo.config) {
        let other = other.to_string();
        if other == name {
            continue;
        }
        if name.strip_prefix(other.as_str()).is_some_and(|r| r.starts_with('/')) {
            return Err(Die::Fatal(msg::subset_of(name, &other)));
        }
        if other.strip_prefix(name).is_some_and(|r| r.starts_with('/')) {
            return Err(Die::Fatal(msg::superset_of(name, &other)));
        }
    }

    repo.set(&format!("remote.{name}.url"), url)?;
    if mirror != Mirror::Push {
        let key = format!("remote.{name}.fetch");
        let fetch_mirror = matches!(mirror, Mirror::Fetch | Mirror::Both);
        if track.is_empty() {
            let spec = if fetch_mirror {
                "+refs/*:refs/*".to_owned()
            } else {
                format!("+refs/heads/*:refs/remotes/{name}/*")
            };
            repo.add(&key, &spec)?;
        }
        for branch in &track {
            repo.add(&key, &tracking_refspec(name, branch, fetch_mirror))?;
        }
    }
    if matches!(mirror, Mirror::Push | Mirror::Both) {
        repo.set(&format!("remote.{name}.mirror"), "true")?;
    }
    match tags.get() {
        1 => repo.set(&format!("remote.{name}.tagOpt"), "--tags")?,
        0 => repo.set(&format!("remote.{name}.tagOpt"), "--no-tags")?,
        _ => {}
    }

    if fetch.get() {
        println!("{}", msg::updating(name));
        let code = update::fetch_one(ctx, &["fetch".to_owned(), name.to_owned()]);
        if code != 0 {
            return Err(Die::Error(msg::could_not_fetch_add(name), 1));
        }
    }

    if let Some(master) = master {
        // Read the repository again: the fetch may have changed refs.
        let repo = Repo::open(ctx)?;
        let head = format!("refs/remotes/{name}/HEAD");
        let target = format!("refs/remotes/{name}/{master}");
        let objects = Objects::open(&repo.setup);
        let mut hooks = UpdateHooks::new(&repo.setup, &repo.config, &objects);
        hooks.mark_symbolic(&head);
        let mut tx = Transaction::new(&repo.store);
        let r = tx
            .push(
                RefUpdate::symref_update(head.as_str(), target.as_str(), None)
                    .no_deref(true)
                    .skip_name_check(true)
                    .message("remote add"),
            )
            .and_then(|()| tx.commit(&mut hooks));
        if let Err(e) = r {
            return Err(Die::Error(e.to_string(), 1));
        }
    }
    Ok(0)
}

/// The refspec `-t <branch>` and `set-branches` add.
pub fn tracking_refspec(name: &str, branch: &str, mirror: bool) -> String {
    if mirror {
        format!("+refs/{branch}:refs/{branch}")
    } else {
        format!("+refs/heads/{branch}:refs/remotes/{name}/{branch}")
    }
}
