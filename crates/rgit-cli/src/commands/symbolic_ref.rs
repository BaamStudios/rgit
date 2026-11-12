// SPDX-License-Identifier: MIT
//! `git symbolic-ref` (Documentation/git-symbolic-ref.adoc), reading only:
//! creating and deleting symbolic refs needs the refs backend (#29).

use std::path::{Path, PathBuf};

use crate::prelude::*;
use crate::repo::{self, Mode};

/// Symbolic refs followed before giving up (git's limit for symref chains).
const MAX_DEPTH: usize = 5;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let quiet = Cell::new(false);
    let delete = Cell::new(false);
    let short = Cell::new(false);
    let recurse = Cell::new(true);
    let reason = RefCell::new(None);
    let mut parser = Parser::new(
        &[
            "git symbolic-ref [-m <reason>] <name> <ref>",
            "git symbolic-ref [-q] [--short] [--no-recurse] <name>",
            "git symbolic-ref --delete [-q] <name>",
        ],
        vec![
            Opt::flag(
                'q',
                "quiet",
                &quiet,
                "suppress error message for non-symbolic (detached) refs",
            ),
            Opt::flag('d', "delete", &delete, "delete symbolic ref"),
            Opt::flag(None, "short", &short, "shorten ref output"),
            Opt::flag(None, "recurse", &recurse, "recursively dereference (default)"),
            Opt::string('m', None, &reason, "reason", "reason of the update"),
        ],
    );
    let args = parser.parse(argv)?.args;
    if delete.get() {
        if args.len() != 1 {
            return Err(parser.usage_error().into());
        }
        return Err(Die::Fatal(not_supported("deleting symbolic refs")));
    }
    match args.len() {
        1 => {}
        2 => return Err(Die::Fatal(not_supported("creating symbolic refs"))),
        _ => return Err(parser.usage_error().into()),
    }
    let setup = repo::setup(ctx, Mode::Required)?;
    let name = &args[0];
    let mut target = None;
    let mut current = name.clone();
    for _ in 0..MAX_DEPTH {
        match read_symref(&setup, &current) {
            Some(t) => {
                target = Some(t.clone());
                current = t;
                if !recurse.get() {
                    break;
                }
            }
            None => break,
        }
    }
    match target {
        Some(t) => {
            let t = if short.get() { shorten(&t) } else { &t };
            println!("{t}");
            Ok(0)
        }
        None if quiet.get() => Ok(1),
        None => Err(Die::Fatal(format!("ref {name} is not a symbolic ref"))),
    }
}

fn not_supported(what: &str) -> String {
    format!("rgit symbolic-ref: {what} is not supported yet")
}

/// Refs that live in the work tree's own git directory rather than the
/// common directory (`gitrepository-layout(5)`).
fn per_worktree(name: &str) -> bool {
    !name.starts_with("refs/")
        || name.starts_with("refs/bisect/")
        || name.starts_with("refs/worktree/")
        || name.starts_with("refs/rewritten/")
}

/// The target of the loose symbolic ref `name`, if it is one.
fn read_symref(setup: &rgit_repo::Setup, name: &str) -> Option<String> {
    if name.split('/').any(|c| c.is_empty() || c == "." || c == "..") {
        return None;
    }
    let base: &Path = if per_worktree(name) {
        &setup.git_dir
    } else {
        &setup.common_dir
    };
    let file: PathBuf = base.join(name);
    let meta = std::fs::symlink_metadata(&file).ok()?;
    if meta.file_type().is_symlink() {
        let t = std::fs::read_link(&file).ok()?;
        let t = t.to_str()?;
        return t.starts_with("refs/").then(|| t.to_string());
    }
    let data = std::fs::read(&file).ok()?;
    let rest = data.strip_prefix(b"ref:")?;
    let text = std::str::from_utf8(rest).ok()?.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// `--short`: drop the namespace prefix.
fn shorten(name: &str) -> &str {
    for p in ["refs/heads/", "refs/tags/", "refs/remotes/", "refs/"] {
        if let Some(s) = name.strip_prefix(p) {
            if !s.is_empty() {
                return s;
            }
        }
    }
    name
}
