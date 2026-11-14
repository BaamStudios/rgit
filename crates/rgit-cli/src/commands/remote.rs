// SPDX-License-Identifier: MIT
//! `git remote` (Documentation/git-remote.adoc,
//! Documentation/config/remote.adoc, Documentation/config/remotes.adoc).
//!
//! All subcommands: listing (`-v`), `add` (`-f`, `-t`, `-m`,
//! `--[no-]tags`, `--mirror[=fetch|push]`), `rename` (refs, reflogs,
//! fetch refspecs, `branch.*`, `remote.pushDefault`, legacy `remotes/` and
//! `branches/` files), `remove`/`rm`, `set-head` (`-a`, `-d`, explicit),
//! `set-branches`, `get-url`, `set-url`, `show` (`-n`), `prune` and
//! `update` (groups, `-p`). Fetches run the built-in `fetch` in-process.
//! Remotes are queried with protocol v2 `ls-refs`, like `fetch` does.

#[path = "remote/add.rs"]
mod add;
#[path = "remote/common.rs"]
mod common;
#[path = "remote/head.rs"]
mod head;
#[path = "remote/rename.rs"]
mod rename;
#[path = "remote/show.rs"]
mod show;
#[path = "remote/update.rs"]
mod update;
#[path = "remote/urls.rs"]
mod urls;

use bstr::BString;

use crate::messages::remote as msg;
use crate::prelude::*;

const SUBCOMMANDS: &[&str] = &[
    "add",
    "rename",
    "remove",
    "rm",
    "set-head",
    "show",
    "prune",
    "update",
    "set-branches",
    "get-url",
    "set-url",
];

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let verbose = Cell::new(0);
    let mut opts = vec![Opt::verbose(&verbose, msg::HELP_VERBOSE)];
    opts.extend(SUBCOMMANDS.iter().map(|s| Opt::subcommand(s)));
    let mut parser = Parser::new(msg::USAGE, opts).flags(ParseFlags {
        subcommand_optional: true,
        ..Default::default()
    });
    let parsed = parser.parse(argv)?;
    let verbose = verbose.get() > 0;
    let Some(sub) = parsed.subcommand else {
        if let Some(arg) = parsed.args.first() {
            return Err(parser.error_with_usage(&msg::unknown_subcommand(arg)).into());
        }
        drop(parser);
        return list(ctx, verbose);
    };
    drop(parser);
    let args = &parsed.args;
    match sub.as_str() {
        "add" => add::run(ctx, args),
        "rename" => rename::rename(ctx, args),
        "remove" | "rm" => rename::remove(ctx, args),
        "set-head" => head::run(ctx, args),
        "show" => show::show(ctx, args, verbose),
        "prune" => show::prune(ctx, args),
        "update" => update::run(ctx, args, verbose),
        "set-branches" => urls::set_branches(ctx, args),
        "get-url" => urls::get_url(ctx, args),
        _ => urls::set_url(ctx, args),
    }
}

/// The names of all configured remotes, sorted.
fn remote_names(config: &rgit_config::ConfigSet) -> Vec<BString> {
    let mut names = config.subsections("remote");
    names.sort();
    names
}

/// `git remote [-v]`.
fn list(ctx: &Context, verbose: bool) -> CmdResult {
    let repo = common::Repo::open(ctx)?;
    let remotes = repo.remotes()?;
    let mut out = String::new();
    for name in remote_names(&repo.config) {
        if !verbose {
            out.push_str(&format!("{name}\n"));
            continue;
        }
        let remote = remotes
            .get(&name, None)
            .map_err(|e| Die::Fatal(e.to_string()))?;
        // Without any URL configured, nothing is shown (the remote's name
        // is not used as its URL here).
        let has_url = has_configured_url(&repo.config, &name, "url");
        match remote.url.first().filter(|_| has_url) {
            Some(url) => {
                out.push_str(&format!("{name}\t{url} (fetch)"));
                let filter = format!("remote.{name}.partialclonefilter");
                if let Ok(Some(f)) = repo.config.string(filter.as_bytes()) {
                    out.push_str(&format!(" [{f}]"));
                }
                out.push('\n');
            }
            None => out.push_str(&format!("{name}\t\n")),
        }
        if remote.has_pushurl || has_url {
            for url in &remote.push_url {
                out.push_str(&format!("{name}\t{url} (push)\n"));
            }
        }
    }
    print!("{out}");
    Ok(0)
}

/// Whether `remote.<name>.<key>` holds at least one URL (an empty value
/// clears the list).
fn has_configured_url(config: &rgit_config::ConfigSet, name: &[u8], key: &str) -> bool {
    let mut full = b"remote.".to_vec();
    full.extend_from_slice(name);
    full.push(b'.');
    full.extend_from_slice(key.as_bytes());
    // The list is non-empty exactly when its last value is.
    config
        .get_all(&full)
        .last()
        .is_some_and(|e| e.value.as_ref().is_some_and(|v| !v.is_empty()))
}
