// SPDX-License-Identifier: MIT

//! `submodule check-name`, `check-url`, `is-active <name>` and
//! `resolve-relative-url <up_path> <remoteurl> <url>` (t7450, t0060,
//! t7413), answered by the `rgit-submodule` crate.
//!
//! Observed behaviour of the reference binary: `check-name` and
//! `check-url` read lines from standard input and print those that pass
//! the check; `is-active` exits 0 for an active submodule named in the
//! work tree's `.gitmodules`, 1 otherwise; `resolve-relative-url` takes
//! `(null)` for a missing `up_path` and prints the result. Wrong argument
//! counts print the usage of the action and exit 129.

use std::io::{BufRead, Write};

use bstr::ByteSlice;
use rgit_submodule::{Gitmodules, Mode, Origin, check_name, check_url, is_active};

use crate::messages;
use crate::repo;

pub fn submodule(args: &[String]) -> i32 {
    let Some(action) = args.first() else {
        return usage(messages::SUBMODULE_USAGE);
    };
    let rest = &args[1..];
    match action.as_str() {
        "check-name" if rest.is_empty() => filter(check_name),
        "check-name" => usage(&messages::SUBMODULE_USAGE[0..1]),
        "check-url" if rest.is_empty() => filter(check_url),
        "check-url" => usage(&messages::SUBMODULE_USAGE[1..2]),
        "is-active" => match rest {
            [name] => is_active_cmd(name),
            _ => usage(&messages::SUBMODULE_USAGE[2..3]),
        },
        "resolve-relative-url" => match rest {
            [up, remote, url] => resolve(up, remote, url),
            _ => usage(&messages::SUBMODULE_USAGE[3..4]),
        },
        other => {
            eprintln!("{}", messages::unknown_subcommand(other));
            eprintln!();
            usage(messages::SUBMODULE_USAGE)
        }
    }
}

fn usage(lines: &[&str]) -> i32 {
    let mut err = std::io::stderr().lock();
    for (i, l) in lines.iter().enumerate() {
        let prefix = if i == 0 { "usage: " } else { "   or: " };
        let _ = writeln!(err, "{prefix}{l}");
    }
    let _ = writeln!(err);
    129
}

/// Print the standard input lines that pass `check`.
fn filter(check: fn(&[u8]) -> bool) -> i32 {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().split(b'\n') {
        let Ok(line) = line else { break };

        if check(&line) {
            let _ = out.write_all(&line);
            let _ = out.write_all(b"\n");
        }
    }
    0
}

fn resolve(up: &str, remote: &str, url: &str) -> i32 {
    let up = (up != "(null)").then_some(up.as_bytes());
    match rgit_submodule::resolve_relative_url(up, remote.as_bytes(), url.as_bytes()) {
        Ok(s) => {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(&s);
            let _ = out.write_all(b"\n");
            0
        }
        Err(e) => {
            eprintln!("fatal: {e}");
            128
        }
    }
}

fn is_active_cmd(name: &str) -> i32 {
    let Ok(r) = repo::discover(true) else {
        return 1;
    };
    let Ok(config) = crate::cmd::config::load(Some(&r), false) else {
        return 1;
    };
    let Some(top) = r.worktree.as_ref() else {
        return 1;
    };
    let file = top.join(rgit_submodule::GITMODULES);
    let Ok(data) = std::fs::read(&file) else {
        return 1;
    };
    let origin = Origin::File(file.to_string_lossy().into_owned());
    let Ok((gm, _)) = Gitmodules::parse(&data, &origin, Mode::Current) else {
        return 1;
    };
    let Some(path) = gm.by_name(name.as_bytes()).and_then(|s| s.path.clone()) else {
        return 1;
    };
    i32::from(!is_active(&config, name.as_bytes(), path.as_bytes()))
}
