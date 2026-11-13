// SPDX-License-Identifier: MIT
//! `git symbolic-ref` (Documentation/git-symbolic-ref.adoc). Creating and
//! deleting go through `rgit_refs` transactions; reflog entries (`-m`) are
//! written once the reflog exists (#30).

use std::path::{Path, PathBuf};

use rgit_refs::files::FilesStore;
use rgit_refs::messages as refs_msg;
use rgit_refs::name::{RefNameFlags, is_valid_refname};
use rgit_refs::{NoHooks, RefUpdate, Transaction};

use crate::messages as msg;
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
        msg::SYMBOLIC_REF_USAGE,
        vec![
            Opt::flag('q', "quiet", &quiet, msg::SYMBOLIC_REF_HELP_QUIET),
            Opt::flag('d', "delete", &delete, msg::SYMBOLIC_REF_HELP_DELETE),
            Opt::flag(None, "short", &short, msg::SYMBOLIC_REF_HELP_SHORT),
            Opt::flag(None, "recurse", &recurse, msg::SYMBOLIC_REF_HELP_RECURSE),
            Opt::string('m', None, &reason, "reason", msg::SYMBOLIC_REF_HELP_REASON),
        ],
    );
    let args = parser.parse(argv)?.args;
    if delete.get() {
        if args.len() != 1 {
            return Err(parser.usage_error().into());
        }
        let setup = repo::setup(ctx, Mode::Required)?;
        return delete_symref(&setup, &args[0]);
    }
    match args.len() {
        1 => {}
        2 => {
            let setup = repo::setup(ctx, Mode::Required)?;
            let reason = reason.borrow().clone().unwrap_or_default();
            return create_symref(&setup, &args[0], &args[1], &reason);
        }
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
            let t = if short.get() {
                shorten(ctx, &setup, &t)?
            } else {
                t
            };
            println!("{t}");
            Ok(0)
        }
        None if quiet.get() => Ok(1),
        None => Err(Die::Fatal(msg::not_a_symbolic_ref(name))),
    }
}

/// The reference store holding `name` (per-worktree refs, #104, are only
/// told apart by directory).
fn store_for(setup: &rgit_repo::Setup, name: &str) -> FilesStore {
    let dir = if per_worktree(name) {
        setup.absolute_git_dir()
    } else {
        setup.cwd.join(&setup.common_dir)
    };
    FilesStore::new(dir, setup.format.object_format)
}

/// Runs one update; errors are printed as `error: ...` with exit code 1.
fn apply(store: &FilesStore, update: RefUpdate) -> CmdResult {
    let mut tx = Transaction::new(store);
    tx.push(update)
        .and_then(|()| tx.commit(&mut NoHooks))
        .map(|()| 0)
        .map_err(|e| Die::Error(e.to_string(), 1))
}

/// `symbolic-ref <name> <target>`.
fn create_symref(setup: &rgit_repo::Setup, name: &str, target: &str, reason: &str) -> CmdResult {
    if name == "HEAD" && !target.starts_with("refs/") {
        return Err(Die::Fatal(refs_msg::HEAD_OUTSIDE_REFS.to_owned()));
    }
    if !is_valid_refname(target.as_bytes(), RefNameFlags::ONELEVEL) {
        return Err(Die::Fatal(refs_msg::invalid_symref_target(name, target)));
    }
    let store = store_for(setup, name);
    let update = RefUpdate::symref_update(name, target, None)
        .no_deref(true)
        .skip_name_check(true)
        .message(reason);
    apply(&store, update)
}

/// `symbolic-ref --delete <name>`.
fn delete_symref(setup: &rgit_repo::Setup, name: &str) -> CmdResult {
    if read_symref(setup, name).is_none() {
        return Err(Die::Fatal(refs_msg::cannot_delete_not_symref(name)));
    }
    if name == "HEAD" {
        return Err(Die::Fatal(refs_msg::DELETING_HEAD.to_owned()));
    }
    let store = store_for(setup, name);
    let update = RefUpdate::delete(name, setup.format.object_format, None)
        .no_deref(true)
        .skip_name_check(true);
    apply(&store, update)
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

/// `--short`: the shortest name that still resolves to `name` without
/// being taken by a higher-priority rule (`refs/remotes/origin/HEAD` ->
/// `origin`), like `rev-parse --abbrev-ref` in loose mode.
fn shorten(ctx: &Context, setup: &rgit_repo::Setup, name: &str) -> Result<String, Die> {
    use crate::revisions::{self, Config, Objects, Refs};
    use rgit_revwalk::revision::{Repo, Resolver};

    let kind = setup.format.object_format;
    let config = revisions::load_config(ctx, setup)?;
    let opts = revisions::options(ctx, &config, kind)?;
    let shallow = setup.common_dir.join("shallow");
    let objects = Objects::new(revisions::objects_dir(setup), kind, &shallow);
    let refs = Refs::new(setup, kind);
    let cfg = Config(&config);
    let resolver = Resolver::new(
        Repo {
            objects: &objects,
            refs: &refs,
            config: &cfg,
            index: None,
            work_tree: None,
            work_tree_info: None,
        },
        opts,
    );
    resolver
        .shorten_ref(name, false)
        .map_err(|e| Die::Fatal(e.to_string()))
}
