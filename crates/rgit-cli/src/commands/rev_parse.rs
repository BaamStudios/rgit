// SPDX-License-Identifier: MIT
//! `git rev-parse` (Documentation/git-rev-parse.adoc): so far only the
//! options that describe the repository location.

use std::io::Write;
use std::path::Path;

use rgit_repo::Setup;

use crate::prelude::*;
use crate::repo::{self, Mode};

const SUPPORTED: &[&str] = &[
    "--git-dir",
    "--absolute-git-dir",
    "--show-toplevel",
    "--show-prefix",
    "--show-cdup",
    "--is-bare-repository",
    "--is-inside-work-tree",
    "--is-inside-git-dir",
];

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let args = &argv[1.min(argv.len())..];
    if let Some(a) = args.iter().find(|a| !SUPPORTED.contains(&a.as_str())) {
        return Err(Die::Fatal(crate::messages::rev_parse_not_supported(a)));
    }
    let setup = repo::setup(ctx, Mode::RequiredQuiet)?;
    let mut out = std::io::stdout().lock();
    for a in args {
        let line = match a.as_str() {
            "--git-dir" => Some(git_dir(&setup)),
            "--absolute-git-dir" => Some(setup.absolute_git_dir().display().to_string()),
            "--show-toplevel" => match &setup.work_tree {
                Some(wt) => Some(wt.display().to_string()),
                None => {
                    drop(out);
                    return Err(repo::die(rgit_repo::Error::NoWorkTree));
                }
            },
            "--show-prefix" => Some(
                setup
                    .prefix
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            "--show-cdup" => show_cdup(&setup),
            "--is-bare-repository" => Some(setup.is_bare().to_string()),
            "--is-inside-work-tree" => Some(setup.is_inside_work_tree().to_string()),
            "--is-inside-git-dir" => Some(setup.is_inside_git_dir().to_string()),
            _ => None,
        };
        if let Some(line) = line {
            let _ = writeln!(out, "{line}");
        }
    }
    Ok(0)
}

/// `--git-dir`: as set up, but absolute when the command moved to the top
/// of the work tree.
fn git_dir(setup: &Setup) -> String {
    if setup.git_dir.is_absolute() || setup.prefix.is_none() {
        return setup.git_dir.display().to_string();
    }
    rgit_repo::path::normalize(&setup.cwd.join(&setup.git_dir))
        .display()
        .to_string()
}

/// `--show-cdup`: the way back up to the top of the work tree; the work
/// tree itself when the command started outside it; nothing without one.
fn show_cdup(setup: &Setup) -> Option<String> {
    let wt = setup.work_tree.as_deref()?;
    match &setup.prefix {
        Some(p) => {
            let depth = Path::new(p).components().count();
            Some("../".repeat(depth))
        }
        None if setup.original_cwd == wt => Some(String::new()),
        None => Some(wt.display().to_string()),
    }
}
