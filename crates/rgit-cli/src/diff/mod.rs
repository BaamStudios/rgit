// SPDX-License-Identifier: MIT
//! Shared machinery of `diff`, `diff-files`, `diff-index` and
//! `diff-tree`: option parsing ([`options`]), finding changed paths
//! ([`collect`]) and output ([`output`], with [`stat`]).
//! No rename detection, no combined diffs.

pub mod collect;
pub mod color;
pub mod options;
pub mod output;
mod stat;

use rgit_pathspec::{ParseOptions, Pathspec};

use crate::Die;
use crate::context::Context;
use crate::repository::{self, Repo};

/// Parse pathspecs relative to the current directory of the repository.
pub fn pathspec(ctx: &Context, repo: &Repo, args: &[String]) -> Result<Pathspec, Die> {
    let root = repo
        .setup
        .work_tree
        .as_ref()
        .map(|w| w.as_os_str().as_encoded_bytes().to_vec());
    let opts = ParseOptions {
        prefix: repo.prefix(),
        root,
        globals: repository::pathspec_globals(ctx),
        ..Default::default()
    };
    Pathspec::parse(args, &opts).map_err(|e| Die::Fatal(e.to_string()))
}

/// `-h` / `--help-all` as the only argument: the usage on stdout, exit 0
/// (works outside a repository).
pub fn help(argv: &[String], usage: &str) -> Option<i32> {
    if argv.len() == 2 && matches!(argv[1].as_str(), "-h" | "--help-all") {
        let mut o = std::io::stdout().lock();
        let _ = std::io::Write::write_all(&mut o, usage.as_bytes());
        return Some(0);
    }
    None
}

/// Split argv (without the command name) into diff options, command
/// options (handled by `own`, which returns whether it took the
/// argument) and the rest. Options end at `--` or, when
/// `stop_at_non_option`, at the first non-option. Returns the remaining
/// arguments and whether `--` was seen (the arguments after it are
/// returned after `dashdash_at`).
pub struct Args {
    pub opts: options::DiffOpts,
    pub args: Vec<String>,
    /// Index into `args` where the arguments after `--` start.
    pub dashdash_at: Option<usize>,
}

pub fn parse_args(
    argv: &[String],
    mut own: impl FnMut(&str) -> Result<bool, Die>,
    unknown: impl Fn(&str) -> Die,
) -> Result<Args, Die> {
    let mut opts = options::DiffOpts::default();
    let mut args = Vec::new();
    let mut dashdash_at = None;
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        if a == "--" {
            dashdash_at = Some(args.len());
            args.extend(it.by_ref().cloned());
            break;
        }
        if a == "--diff-algorithm" || a == "--anchored" {
            // The value may also be the next argument.
            let Some(v) = it.next() else {
                return Err(options::usage_exit(crate::messages::diff::requires_value(
                    &a[2..],
                )));
            };
            opts.parse_arg(&format!("{a}={v}"))?;
            continue;
        }
        if a.starts_with('-') && a != "-" {
            if own(a)? || opts.parse_arg(a)? {
                continue;
            }
            return Err(unknown(a));
        }
        args.push(a.clone());
    }
    Ok(Args {
        opts,
        args,
        dashdash_at,
    })
}

/// The merge bases of two commits (for `A...B`).
pub fn merge_bases(
    ctx: &Context,
    a: &rgit_hash::ObjectId,
    b: &rgit_hash::ObjectId,
) -> Result<Vec<rgit_hash::ObjectId>, Die> {
    use crate::revisions::{self, Config, IndexFile, Objects, Refs, WorkTree};
    use rgit_revwalk::revision::{self, Resolver};
    let setup = crate::repo::setup(ctx, crate::repo::Mode::RequiredQuiet)?;
    let kind = setup.format.object_format;
    let config = revisions::load_config(ctx, &setup)?;
    let rev_opts = revisions::options(ctx, &config, kind)?;
    let shallow = setup.common_dir.join("shallow");
    let objects = Objects::new(revisions::objects_dir(&setup), kind, &shallow);
    let refs = Refs::new(&setup, kind);
    let cfg = Config(&config);
    let index = IndexFile::new(&setup, kind);
    let work_tree = setup.work_tree.clone().map(WorkTree);
    let rev_repo = revision::Repo {
        objects: &objects,
        refs: &refs,
        config: &cfg,
        index: Some(&index),
        work_tree: work_tree
            .as_ref()
            .map(|w| w as &dyn revision::WorkTreeLookup),
        work_tree_info: revisions::work_tree_info(&setup),
    };
    let resolver = Resolver::new(rev_repo, rev_opts);
    resolver
        .merge_bases(a, b)
        .map_err(|_| Die::Fatal(format!("{}...{}", a.to_hex(), b.to_hex())))
}
