// SPDX-License-Identifier: MIT
//! `git pack-refs` (Documentation/git-pack-refs.adoc) for the files
//! backend: `--all`, `--[no-]prune`, `--include`, `--exclude` and `--auto`
//! (see [`needs_packing`]).

use rgit_object::ObjectKind;
use rgit_pathspec::wildmatch::{WildmatchFlags, wildmatch};

use crate::messages::pack_refs as msg;
use crate::prelude::*;
use crate::refs_support::{self, Objects};
use crate::repo::{self, Mode};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    run_with_usage(ctx, argv, msg::USAGE)
}

/// `pack-refs` with another usage text (`git refs optimize`).
pub fn run_with_usage(ctx: &mut Context, argv: &[String], usage: &[&str]) -> CmdResult {
    let all = Cell::new(false);
    let prune = Cell::new(true);
    let auto = Cell::new(false);
    let includes: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let excludes: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let mut parser = Parser::new(
        usage,
        vec![
            Opt::flag(None, "all", &all, msg::HELP_ALL),
            Opt::flag(None, "prune", &prune, msg::HELP_PRUNE),
            Opt::flag(None, "auto", &auto, msg::HELP_AUTO),
            Opt::string_list(None, "include", &includes, "pattern", msg::HELP_INCLUDE),
            Opt::string_list(None, "exclude", &excludes, "pattern", msg::HELP_EXCLUDE),
        ],
    );
    let args = parser.parse(argv)?.args;
    if !args.is_empty() {
        return Err(parser.usage_error().into());
    }
    let setup = repo::setup(ctx, Mode::Required)?;
    let config = refs_support::load_config(ctx, &setup)?;
    let store = refs_support::ref_store(&setup, &config);
    let objects = Objects::open(&setup);

    let includes = includes.borrow().clone();
    let excludes = excludes.borrow().clone();
    let all = all.get();
    let matches = |pats: &[String], name: &str| {
        pats.iter()
            .any(|p| wildmatch(p.as_bytes(), name.as_bytes(), WildmatchFlags::NONE))
    };
    let filter = |name: &str| -> bool {
        if matches(&excludes, name) {
            return false;
        }
        // `--include` replaces the default of packing the tags.
        all || matches(&includes, name) || (includes.is_empty() && name.starts_with("refs/tags/"))
    };
    // A tag whose target is not of the type it claims gets no peeled
    // value.
    let peel = |id: &rgit_hash::ObjectId| {
        if objects.kind(id) != Some(ObjectKind::Tag) {
            return None;
        }
        let mut cur = *id;
        for _ in 0..64 {
            let (kind, data) = objects.read(&cur)?;
            if kind != ObjectKind::Tag {
                return Some(cur);
            }
            let tag = rgit_object::Tag::parse(setup.format.object_format, &data).ok()?;
            let next = tag.object().ok()?;
            if objects.kind(&next)? != tag.target_kind().ok()? {
                return None;
            }
            cur = next;
        }
        None
    };
    if auto.get() && !needs_packing(&setup) {
        return Ok(0);
    }
    store
        .pack_refs(&filter, prune.get(), &peel)
        .map(|()| 0)
        .map_err(|e| Die::Fatal(e.to_string()))
}

/// `--auto` for the files backend: pack only when there are at least 16
/// loose references, and more the bigger `packed-refs` is (five more per
/// doubling of its size beyond 6400 bytes, as the reference git does).
pub(crate) fn needs_packing(setup: &rgit_repo::Setup) -> bool {
    fn count(dir: &std::path::Path) -> usize {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return 0;
        };
        entries
            .flatten()
            .map(|e| match e.file_type() {
                Ok(t) if t.is_dir() => count(&e.path()),
                Ok(_) => usize::from(!e.file_name().to_string_lossy().ends_with(".lock")),
                Err(_) => 0,
            })
            .sum()
    }
    let common = setup.absolute_common_dir();
    // Per-worktree references are never packed and do not count.
    let refs = common.join("refs");
    let loose: usize = std::fs::read_dir(&refs)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| !matches!(e.file_name().to_str(), Some("bisect" | "worktree" | "rewritten")))
                .map(|e| match e.file_type() {
                    Ok(t) if t.is_dir() => count(&e.path()),
                    Ok(_) => 1,
                    Err(_) => 0,
                })
                .sum()
        })
        .unwrap_or(0);
    let size = std::fs::metadata(common.join("packed-refs")).map_or(0, |m| m.len());
    let hundreds = size / 100;
    let limit = if hundreds == 0 {
        0
    } else {
        5 * u64::from(hundreds.ilog2())
    };
    loose as u64 >= limit.max(16)
}
