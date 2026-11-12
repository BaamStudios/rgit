// SPDX-License-Identifier: MIT

//! `rgit-test-tool`: stand-ins for the subcommands of git's `test-tool` that
//! exercise git's library code (class C in rgit-conformance's
//! `TEST-TOOL.md`), answered by rgit's own crates instead.
//!
//! The conformance dispatcher (`test-tool/test-tool` in rgit-conformance)
//! execs `rgit-test-tool <subcommand> <args>...` for the subcommands listed in
//! [`TOOLS`]; everything else keeps going to the reference `test-tool`.
//!
//! Behaviour is defined only by observation of the reference `test-tool`
//! (its `-h` usage text and its output for the calls made by the upstream
//! test scripts); see `CLEANROOM.md`. The tests in `tests/` replay those calls
//! against both binaries.

mod cmd;
mod messages;
mod repo;

use std::io::{self, Write};
use std::process::exit;

/// A subcommand: name and entry point. The entry point gets the arguments
/// after the subcommand name and returns the exit code.
type Tool = (&'static str, fn(&[String]) -> i32);

/// Implemented subcommands, in the order the usage lists them.
const TOOLS: &[Tool] = &[
    ("config", cmd::config::config),
    ("date", cmd::date::date),
    ("parse-options", cmd::parse_options::parse_options),
    (
        "parse-options-flags",
        cmd::parse_options::parse_options_flags,
    ),
    ("parse-subcommand", cmd::parse_options::parse_subcommand),
    ("ref-store", cmd::ref_store::ref_store),
    ("sha1", cmd::sha::sha1),
    ("sha1-is-sha1dc", cmd::sha::sha1_is_sha1dc),
    ("sha256", cmd::sha::sha256),
    ("wildmatch", cmd::wildmatch::wildmatch),
];

fn usage_and_exit(unknown: Option<&str>) -> ! {
    let mut err = io::stderr().lock();
    if let Some(name) = unknown {
        let _ = writeln!(err, "{}", messages::no_such_tool(name));
    }
    let _ = writeln!(err, "{}", messages::USAGE);
    for (name, _) in TOOLS {
        let _ = writeln!(err, "  {name}");
    }
    exit(128);
}

fn main() {
    let mut args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    // Leading `-C <dir>`, as the reference binary accepts it.
    while let Some(first) = args.first() {
        let dir = if first == "-C" {
            if args.len() < 2 {
                usage_and_exit(None);
            }
            args.remove(0);
            args.remove(0)
        } else if let Some(d) = first.strip_prefix("-C") {
            let d = d.to_owned();
            args.remove(0);
            d
        } else {
            break;
        };
        if let Err(e) = std::env::set_current_dir(&dir) {
            eprintln!("{}", messages::cannot_chdir(&dir, &e));
            exit(128);
        }
    }

    let Some(name) = args.first() else {
        usage_and_exit(None);
    };
    let Some((_, run)) = TOOLS.iter().find(|(n, _)| n == name) else {
        usage_and_exit(Some(name));
    };
    let code = run(&args[1..]);
    let _ = io::stdout().flush();
    exit(code);
}
