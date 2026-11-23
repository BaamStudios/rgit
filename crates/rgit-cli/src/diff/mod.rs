// SPDX-License-Identifier: MIT
//! Shared machinery of `diff`, `diff-files`, `diff-index` and
//! `diff-tree`: option parsing ([`options`]), finding changed paths
//! ([`collect`]) and output ([`output`], with [`stat`]).
//! Break, rename and copy detection, pickaxe and `--diff-filter` run in
//! [`output::Output::diffcore`] (`rgit_diff::diffcore`). No combined diffs.

mod check;
pub mod collect;
pub mod color;
pub mod combined;
mod dirstat;
pub mod driver;
pub mod external;
pub mod moved;
pub mod options;
mod order;
pub mod output;
pub mod remerge;
mod stat;
mod words;

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
        implicit_base: rgit_pathspec::ImplicitBase::Top,
        ..Default::default()
    };
    let ps = Pathspec::parse(args, &opts).map_err(|e| Die::Fatal(e.to_string()))?;
    // `<submodule>/` names the submodule itself (t4010): the trailing slash
    // of a pathspec naming a gitlink of the index is dropped.
    let slashed: Vec<&[u8]> = ps
        .items()
        .iter()
        .filter(|i| !i.has_wildcard() && i.pattern().len() > 1 && i.pattern().ends_with(b"/"))
        .map(|i| i.pattern())
        .collect();
    if slashed.is_empty() || repo.setup.work_tree.is_none() {
        return Ok(ps);
    }
    let Ok((index, _)) = repo.read_index() else {
        return Ok(ps);
    };
    let strip: Vec<&[u8]> = ps
        .items()
        .iter()
        .filter(|i| slashed.contains(&i.pattern()))
        .filter(|i| {
            let p = &i.pattern()[..i.pattern().len() - 1];
            index
                .entries()
                .iter()
                .any(|e| e.mode.is_gitlink() && e.path() == p)
        })
        .map(|i| i.original())
        .collect();
    if strip.is_empty() {
        return Ok(ps);
    }
    let args: Vec<String> = args
        .iter()
        .map(|a| match a.strip_suffix('/') {
            Some(s) if strip.contains(&a.as_bytes()) => s.to_owned(),
            _ => a.clone(),
        })
        .collect();
    Pathspec::parse(&args, &opts).map_err(|e| Die::Fatal(e.to_string()))
}

/// `-0`, `-1`/`--base`, `-2`/`--ours`, `-3`/`--theirs` of work tree
/// diffs: the stage unmerged paths are compared with.
pub fn stage_option(arg: &str) -> Option<usize> {
    Some(match arg {
        "-0" => 0,
        "-1" | "--base" => 1,
        "-2" | "--ours" => 2,
        "-3" | "--theirs" => 3,
        _ => return None,
    })
}

/// How a work tree diff shows unmerged paths: a stage option wins, then
/// `-c`/`--cc`; a patch is combined by default.
pub fn unmerged_as(
    stage: Option<usize>,
    combined: Option<bool>,
    patch: bool,
) -> combined::UnmergedAs {
    match (stage, combined) {
        (Some(n), _) => combined::UnmergedAs::Stage(n),
        (None, Some(_)) => combined::UnmergedAs::Combined,
        (None, None) if patch => combined::UnmergedAs::Combined,
        _ => combined::UnmergedAs::Default,
    }
}

/// `--max-depth` cannot be combined with wildcards in the pathspec.
pub fn check_max_depth(opts: &options::DiffOpts, ps: &Pathspec) -> Result<(), Die> {
    if opts.max_depth.is_some() && ps.items().iter().any(|i| i.has_wildcard()) {
        return Err(Die::Fatal(
            crate::messages::diff::MAX_DEPTH_WILDCARD.to_string(),
        ));
    }
    Ok(())
}

/// `attr:` pathspec magic in the diff family: the changes are collected
/// without a pathspec, and [`attr_retain`] keeps those the pathspec with
/// its attribute requirements selects.
pub struct AttrFilter {
    pathspec: Pathspec,
    tree: Option<Option<rgit_hash::ObjectId>>,
}

/// Splits `pathspec` into the one to collect changes with and, when it
/// has `attr:` magic, the filter to apply afterwards.
pub fn attr_split(
    ctx: &Context,
    repo: &mut Repo,
    pathspec: Pathspec,
) -> Result<(Pathspec, Option<AttrFilter>), Die> {
    match crate::attributes::PathspecAttrs::prepare(ctx, repo, &pathspec)? {
        None => Ok((pathspec, None)),
        Some(tree) => {
            let all = Pathspec::parse(Vec::<String>::new(), &ParseOptions::default())
                .map_err(|e| Die::Fatal(e.to_string()))?;
            Ok((all, Some(AttrFilter { pathspec, tree })))
        }
    }
}

/// Keeps the pairs that `filter` (from [`attr_split`]) selects.
pub fn attr_retain(
    repo: &Repo,
    pairs: &mut Vec<collect::Pair>,
    filter: Option<AttrFilter>,
) -> Result<(), Die> {
    let Some(filter) = filter else {
        return Ok(());
    };
    let (index, _) = repo.read_index()?;
    let attrs = std::cell::RefCell::new(crate::attributes::PathspecAttrs::new(
        repo,
        &index,
        filter.tree,
    ));
    pairs.retain(|p| {
        filter
            .pathspec
            .match_path_with(&p.path, None, &mut |q, s| attrs.borrow_mut().check(q, s))
            .is_some()
    });
    Ok(())
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
        if matches!(a.as_str(), "-S" | "-G" | "-l" | "-I" | "-O") {
            // The value may also be the next argument.
            let Some(v) = it.next() else {
                return Err(options::usage_exit(crate::messages::diff::requires_value(
                    &a[1..],
                )));
            };
            opts.parse_arg(&format!("{a}{v}"))?;
            continue;
        }
        if matches!(
            a.as_str(),
            "--find-object"
                | "--diff-filter"
                | "--ignore-matching-lines"
                | "--output"
                | "--rotate-to"
                | "--skip-to"
        ) {
            let Some(v) = it.next() else {
                return Err(options::usage_exit(crate::messages::diff::requires_value(
                    &a[2..],
                )));
            };
            opts.parse_arg(&format!("{a}={v}"))?;
            continue;
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
/// Whether an argument that is not a file may still be meant as a
/// pathspec without `--`: it has pathspec magic or wildcards.
pub fn looks_like_pathspec(arg: &str) -> bool {
    arg.starts_with(':') || arg.contains(['*', '?', '[', '\\'])
}

/// Paths after the revisions without `--` must exist (or look like
/// pathspecs): the first one is an "ambiguous argument", later ones
/// "no such path".
pub fn verify_paths(repo: &Repo, args: &[String]) -> Result<(), Die> {
    for (i, arg) in args.iter().enumerate() {
        let exists = repo.setup.work_tree.is_some()
            && repo.setup.original_cwd.join(arg).symlink_metadata().is_ok();
        if !exists && !looks_like_pathspec(arg) {
            return Err(Die::Fatal(if i == 0 {
                crate::messages::ambiguous_argument(arg)
            } else {
                crate::messages::no_such_path(arg)
            }));
        }
    }
    Ok(())
}

/// `--merge-base`: the single merge base of `a` and `b`, which must be
/// commits (or tags of commits).
pub fn single_merge_base(
    ctx: &Context,
    repo: &mut Repo,
    a: rgit_hash::ObjectId,
    b: rgit_hash::ObjectId,
) -> Result<rgit_hash::ObjectId, Die> {
    use crate::messages::diff as msg;
    let mut commits = Vec::new();
    for id in [a, b] {
        match repo.peel_to(id, rgit_object::ObjectKind::Commit) {
            Some(c) => commits.push(c),
            None => {
                let kind = repo.object_kind(&id).map_or("object", |k| k.as_str());
                eprintln!("{}", msg::not_a_commit(&id.to_hex(), kind));
                return Err(Die::Fatal(msg::NO_MERGE_BASE_FOUND.to_owned()));
            }
        }
    }
    let bases = merge_bases(ctx, &commits[0], &commits[1])?;
    match bases.len() {
        0 => Err(Die::Fatal(msg::NO_MERGE_BASE_FOUND.to_owned())),
        1 => Ok(bases[0]),
        _ => Err(Die::Fatal(msg::MULTIPLE_MERGE_BASES_FOUND.to_owned())),
    }
}

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
