// SPDX-License-Identifier: MIT
//! `git check-ref-format` (Documentation/git-check-ref-format.adoc).
//!
//! The rules and messages live in `rgit_refs`. `--branch @{-N}` (the
//! previous checkout, read from the reflog) is not supported yet (#30):
//! such names are rejected as outside a repository.

use std::io::Write;

use rgit_refs::messages::{CHECK_REF_FORMAT_USAGE, invalid_branch_name};
use rgit_refs::name::{RefNameFlags, check_branch_name, check_refname_format, normalize_refname};

use crate::prelude::*;

fn usage(to_stdout: bool) -> CmdResult {
    if to_stdout {
        print!("{CHECK_REF_FORMAT_USAGE}");
        Ok(0)
    } else {
        eprint!("{CHECK_REF_FORMAT_USAGE}");
        Ok(rgit_options::EXIT_USAGE)
    }
}

pub fn run(_ctx: &mut Context, argv: &[String]) -> CmdResult {
    let args = &argv[1..];
    if args.len() == 1 && args[0] == "-h" {
        return usage(true);
    }
    if args.first().map(String::as_str) == Some("--branch") {
        if args.len() != 2 {
            return usage(false);
        }
        let name = &args[1];
        return match check_branch_name(name.as_bytes()) {
            Ok(()) => {
                println!("{name}");
                Ok(0)
            }
            Err(_) => Err(Die::Fatal(invalid_branch_name(name))),
        };
    }
    let mut normalize = false;
    let mut flags = RefNameFlags::STRICT;
    let mut rest = args;
    while let Some(arg) = rest.first().filter(|a| a.starts_with("--")) {
        match arg.as_str() {
            "--normalize" | "--print" => normalize = true,
            "--allow-onelevel" => flags.allow_onelevel = true,
            "--no-allow-onelevel" => flags.allow_onelevel = false,
            "--refspec-pattern" => flags.refspec_pattern = true,
            _ => return usage(false),
        }
        rest = &rest[1..];
    }
    let [name] = rest else {
        return usage(false);
    };
    if normalize {
        match normalize_refname(name.as_bytes(), flags) {
            Ok(n) => {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(&n);
                let _ = out.write_all(b"\n");
                Ok(0)
            }
            Err(_) => Ok(1),
        }
    } else {
        Ok(i32::from(check_refname_format(name.as_bytes(), flags).is_err()))
    }
}
