// SPDX-License-Identifier: MIT
//! `git check-ref-format` (Documentation/git-check-ref-format.adoc).
//!
//! The rules and messages live in `rgit_refs`. `--branch @{-N}...` names
//! the N-th previous checkout (from the `HEAD` reflog) inside a
//! repository.

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

/// `@{-<n>}` at the start of `name` replaced by the n-th previously
/// checked out branch (or commit); `name` itself outside a repository or
/// when there are not that many checkouts.
fn expand_previous(ctx: &Context, name: &str) -> Result<String, Die> {
    let Some((n, tail)) = name
        .strip_prefix("@{-")
        .and_then(|r| r.split_once('}'))
        .and_then(|(n, t)| n.parse::<usize>().ok().filter(|&n| n > 0).map(|n| (n, t)))
    else {
        return Ok(name.to_owned());
    };
    let Some(setup) = crate::repo::setup_gently(ctx)? else {
        return Ok(name.to_owned());
    };
    let logs = rgit_refs::reflog::FilesReflog::new(
        setup.absolute_git_dir(),
        setup.absolute_common_dir(),
        setup.format.object_format,
    );
    let head = logs.read("HEAD").ok().flatten().unwrap_or_default();
    Ok(match rgit_refs::reflog::nth_prior_checkout(&head, n) {
        Some(b) => format!("{}{tail}", String::from_utf8_lossy(b)),
        None => name.to_owned(),
    })
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let args = &argv[1..];
    if args.len() == 1 && (args[0] == "-h" || args[0] == "--help-all") {
        return usage(true);
    }
    if args.first().map(String::as_str) == Some("--branch") {
        if args.len() != 2 {
            return usage(false);
        }
        let name = &expand_previous(ctx, &args[1])?;
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
