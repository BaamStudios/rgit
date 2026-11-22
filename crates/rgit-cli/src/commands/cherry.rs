// SPDX-License-Identifier: MIT
//! `git cherry` (Documentation/git-cherry.adoc): the commits of
//! `<limit>..<head>`, oldest first, marked `-` when `<head>..<upstream>`
//! has a commit with the same patch id and `+` otherwise. This is
//! `rev-list --cherry-mark --right-only --no-merges <upstream>...<head>`.

use std::io::Write;

use rgit_object::Object;

use crate::messages::cherry as msg;
use crate::prelude::*;
use crate::repository::Repo;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let verbose = Cell::new(0);
    // `None`: full names; `Some(None)`: the default length.
    let abbrev: Cell<Option<Option<usize>>> = Cell::new(None);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::callback(None, "abbrev", Some("n"), msg::HELP_ABBREV, |v, unset| {
                if unset {
                    abbrev.set(None);
                    return Ok(());
                }
                match v {
                    None => abbrev.set(Some(None)),
                    Some(v) => match v.parse::<i64>() {
                        Ok(n) => abbrev.set(Some(Some(usize::try_from(n).unwrap_or(0)))),
                        Err(_) => {
                            return Err(CallbackError::Error(
                                crate::messages::abbrev_not_numeric("abbrev"),
                            ));
                        }
                    },
                }
                Ok(())
            })
            .optional(None),
            Opt::verbose(&verbose, msg::HELP_VERBOSE),
        ],
    );
    let mut args = parser.parse(argv)?.args;
    let setup = crate::repo::setup(ctx, crate::repo::Mode::Required)?;
    let revs = crate::refs_support::Revs::new(ctx, &setup)?;
    let commit = |name: &str| -> Result<Option<rgit_hash::ObjectId>, Die> {
        revs.with_resolver(|r| {
            Ok(r.resolve(name)
                .ok()
                .and_then(|id| r.peel_to_commit(&id).ok()))
        })
    };
    // Without arguments (or with too many) the upstream of the current
    // branch is compared with HEAD.
    if args.is_empty() || args.len() > 3 {
        if commit("@{upstream}")?.is_none() {
            eprintln!("{}", msg::NO_UPSTREAM);
            return Err(parser.usage_error().into());
        }
        args = vec!["@{upstream}".to_owned()];
    }
    drop(parser);
    let resolve = |name: &str| -> Result<rgit_hash::ObjectId, Die> {
        commit(name)?.ok_or_else(|| Die::Fatal(msg::unknown_commit(name)))
    };
    let upstream_id = resolve(&args[0])?;
    let head_id = resolve(args.get(1).map_or("HEAD", String::as_str))?;
    let limit_id = args.get(2).map(|l| resolve(l)).transpose()?;
    let repo = Repo::from_setup(ctx, setup)?;
    let mut walk = vec![
        "--cherry-mark".to_owned(),
        "--right-only".to_owned(),
        "--no-merges".to_owned(),
        format!("{}...{}", upstream_id.to_hex(), head_id.to_hex()),
    ];
    if let Some(l) = limit_id {
        walk.push(format!("^{}", l.to_hex()));
    }
    let mut commits = crate::history::walk_marked(ctx, &walk)?;
    commits.reverse();
    let len = match abbrev.get() {
        None => None,
        Some(None) => Some(repo.default_abbrev()),
        Some(Some(n)) => Some(n.max(crate::repository::MIN_ABBREV)),
    };
    let mut out = Vec::new();
    for (id, same) in commits {
        out.extend_from_slice(if same { b"- " } else { b"+ " });
        let name = match len {
            Some(n) if n < id.kind().hex_len() => repo.abbrev(&id, n),
            _ => id.to_hex(),
        };
        out.extend_from_slice(name.as_bytes());
        if verbose.get() > 0
            && let Some(Object::Commit(c)) = repo.read_object(&id)
        {
            out.push(b' ');
            out.extend_from_slice(&subject(c.message()));
        }
        out.push(b'\n');
    }
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(&out);
    let _ = stdout.flush();
    Ok(0)
}

/// The first paragraph of a commit message joined into one line.
fn subject(message: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for line in message.split(|&b| b == b'\n') {
        let line = line.trim_ascii();
        if line.is_empty() {
            if out.is_empty() {
                continue;
            }
            break;
        }
        if !out.is_empty() {
            out.push(b' ');
        }
        out.extend_from_slice(line);
    }
    out
}
