// SPDX-License-Identifier: MIT
//! `git pack-refs` (Documentation/git-pack-refs.adoc) for the files
//! backend: `--all`, `--[no-]prune`, `--include`, `--exclude`; `--auto`
//! packs like a plain run.

use rgit_object::ObjectKind;
use rgit_pathspec::wildmatch::{WildmatchFlags, wildmatch};

use crate::messages::pack_refs as msg;
use crate::prelude::*;
use crate::refs_support::{self, Objects};
use crate::repo::{self, Mode};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let all = Cell::new(false);
    let prune = Cell::new(true);
    let auto = Cell::new(false);
    let includes: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let excludes: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let mut parser = Parser::new(
        msg::USAGE,
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
    let config = refs_support::load_config(ctx, &setup);
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
        all || name.starts_with("refs/tags/") || matches(&includes, name)
    };
    let peel = |id: &rgit_hash::ObjectId| {
        if objects.kind(id) == Some(ObjectKind::Tag) {
            objects.peel(id)
        } else {
            None
        }
    };
    store
        .pack_refs(&filter, prune.get(), &peel)
        .map(|()| 0)
        .map_err(|e| Die::Fatal(e.to_string()))
}
