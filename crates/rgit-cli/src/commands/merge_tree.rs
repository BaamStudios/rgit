// SPDX-License-Identifier: MIT
//! `git merge-tree --write-tree` (Documentation/git-merge-tree.adoc): merge
//! two commits (or, with `--merge-base`, two trees) in memory with
//! [`rgit_merge`], write the result tree and print it with the conflicted
//! files and the merge messages.
//!
//! Not supported yet: rename detection (#151; every merge runs as with
//! `-X no-renames`), `--stdin`, the deprecated `--trivial-merge` mode, and
//! the `-X` options other than `ours`, `theirs`, the rename options and the
//! diff algorithm.

use std::io::Write;

use rgit_hash::ObjectId;
use rgit_merge::{Algorithm, Favor, MergeResult, MessageKind, ObjectCommits, Options};
use rgit_object::ObjectKind;

use crate::messages::merge_tree as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;
use crate::revision::Resolver;

const MODE_WRITE_TREE: i32 = 1;
const MODE_TRIVIAL: i32 = 2;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mode = Cell::new(0);
    let messages = Cell::new(-1);
    let quiet = Cell::new(false);
    let nul = Cell::new(false);
    let name_only = Cell::new(false);
    let allow_unrelated = Cell::new(false);
    let stdin = Cell::new(false);
    let merge_base = RefCell::new(None::<String>);
    let xopts = RefCell::new(Vec::<String>::new());
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::cmdmode(
                None,
                "write-tree",
                &mode,
                msg::HELP_WRITE_TREE,
                MODE_WRITE_TREE,
            ),
            Opt::cmdmode(
                None,
                "trivial-merge",
                &mode,
                msg::HELP_TRIVIAL_MERGE,
                MODE_TRIVIAL,
            ),
            Opt::bool(None, "messages", &messages, msg::HELP_MESSAGES),
            Opt::flag(None, "quiet", &quiet, msg::HELP_QUIET).noneg(),
            Opt::flag('z', None, &nul, msg::HELP_Z),
            Opt::flag(None, "name-only", &name_only, msg::HELP_NAME_ONLY).noneg(),
            Opt::flag(
                None,
                "allow-unrelated-histories",
                &allow_unrelated,
                msg::HELP_ALLOW_UNRELATED,
            )
            .noneg(),
            Opt::flag(None, "stdin", &stdin, msg::HELP_STDIN).noneg(),
            Opt::string(
                None,
                "merge-base",
                &merge_base,
                "tree-ish",
                msg::HELP_MERGE_BASE,
            ),
            Opt::string_list(
                'X',
                "strategy-option",
                &xopts,
                "option=value",
                msg::HELP_STRATEGY_OPTION,
            ),
        ],
    );
    let args = parser.parse(argv)?.args;
    let mut mode = mode.get();
    if mode == 0 {
        mode = match args.len() {
            3 => MODE_TRIVIAL,
            _ => MODE_WRITE_TREE,
        };
    }
    let wanted = match mode {
        MODE_TRIVIAL => 3,
        _ => 2,
    };
    if !stdin.get() && args.len() != wanted {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let merge_base = merge_base.into_inner();
    if stdin.get() && merge_base.is_some() {
        return Err(Die::Fatal(crate::messages::merge_base::incompatible(
            "--merge-base",
            "--stdin",
        )));
    }
    if quiet.get() && name_only.get() {
        return Err(Die::Fatal(crate::messages::merge_base::incompatible(
            "--quiet",
            "--name-only",
        )));
    }
    if mode == MODE_TRIVIAL {
        return Err(Die::Fatal(msg::TRIVIAL_MERGE_NOT_SUPPORTED.into()));
    }
    if stdin.get() {
        return Err(Die::Fatal(msg::STDIN_NOT_SUPPORTED.into()));
    }

    let mut repo = Repo::open(ctx, Mode::Required)?;
    let mut opts = Options {
        branch1: args[0].as_bytes().to_vec(),
        branch2: args[1].as_bytes().to_vec(),
        style: crate::commands::merge_file::config_style(&repo.config)?,
        allow_unrelated_histories: allow_unrelated.get(),
        ..Options::default()
    };
    for x in xopts.into_inner() {
        strategy_option(&mut opts, &x)?;
    }

    let result = match merge_base {
        Some(base) => {
            let base_tree = tree_arg(&mut repo, &base)?;
            let ours = tree_arg(&mut repo, &args[0])?;
            let theirs = tree_arg(&mut repo, &args[1])?;
            opts.ancestor = base.into_bytes();
            rgit_merge::merge_trees(repo.odb(), &base_tree, &ours, &theirs, &opts)
        }
        None => {
            let ours = commit_arg(&mut repo, &args[0])?;
            let theirs = commit_arg(&mut repo, &args[1])?;
            let shallow = std::fs::read_to_string(repo.setup.common_dir.join("shallow"))
                .unwrap_or_default()
                .lines()
                .filter_map(|l| ObjectId::from_hex(repo.hash, l.trim().as_bytes()).ok())
                .collect();
            let len = repo.default_abbrev();
            let commits = ObjectCommits {
                objects: repo.odb(),
                shallow,
                label: Box::new(|id| repo.abbrev(id, len).into_bytes()),
            };
            rgit_merge::merge_commits(repo.odb(), &commits, &ours, &theirs, &opts)
        }
    }
    .map_err(|e| Die::Fatal(e.to_string()))?;

    advise_submodules(ctx, &repo, &result)?;
    if quiet.get() {
        return Ok(if result.clean { 0 } else { 1 });
    }
    let show_messages = match messages.get() {
        -1 => !result.clean,
        v => v != 0,
    };
    let out = render(
        &result,
        nul.get(),
        name_only.get(),
        show_messages,
        repo.quote_path(),
    );
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(&out);
    let _ = stdout.flush();
    Ok(if result.clean { 0 } else { 1 })
}

/// Applies one `-X` option.
fn strategy_option(opts: &mut Options, x: &str) -> Result<(), Die> {
    let unknown = || Die::Fatal(msg::unknown_strategy_option(x));
    let unsupported = || Die::Fatal(msg::strategy_option_not_supported(x));
    let (name, value) = match x.split_once('=') {
        Some((n, v)) => (n, Some(v)),
        None => (x, None),
    };
    match (name, value) {
        ("ours", None) => opts.favor = Favor::Ours,
        ("theirs", None) => opts.favor = Favor::Theirs,
        // Rename detection is not implemented (#151): every merge runs as
        // with `no-renames`.
        ("no-renames", None) | ("find-renames", None) => {}
        ("find-renames" | "rename-threshold", Some(v)) => {
            if !valid_similarity(v) {
                return Err(unknown());
            }
        }
        ("diff-algorithm", Some(v)) => match Algorithm::parse(v) {
            Some((algorithm, minimal)) => {
                opts.algorithm = algorithm;
                opts.minimal = minimal;
            }
            None => return Err(unknown()),
        },
        ("patience", None) => (opts.algorithm, opts.minimal) = (Algorithm::Patience, false),
        ("histogram", None) => (opts.algorithm, opts.minimal) = (Algorithm::Histogram, false),
        (
            "ignore-space-change"
            | "ignore-all-space"
            | "ignore-space-at-eol"
            | "ignore-cr-at-eol"
            | "renormalize"
            | "no-renormalize",
            None,
        ) => return Err(unsupported()),
        ("subtree", _) => return Err(unsupported()),
        _ => return Err(unknown()),
    }
    Ok(())
}

/// A similarity score as `find-renames=<n>` takes it: digits, optionally
/// with a fraction or a percent sign.
fn valid_similarity(v: &str) -> bool {
    let digits = v.strip_suffix('%').unwrap_or(v);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit() || b == b'.')
}

/// Resolves a branch argument to a commit; git's wording on failure.
fn commit_arg(repo: &mut Repo, name: &str) -> Result<ObjectId, Die> {
    let mut r = Resolver::new(repo);
    let found = r
        .resolve(name)
        .ok()
        .and_then(|res| r.peel_to(res.oid, ObjectKind::Commit, Some(name)));
    found.ok_or_else(|| {
        eprintln!("{}", msg::not_something_we_can_merge(name));
        Die::Exit(1)
    })
}

/// Resolves a tree-ish argument (`--merge-base` mode).
fn tree_arg(repo: &mut Repo, name: &str) -> Result<ObjectId, Die> {
    let mut r = Resolver::new(repo);
    let oid = r
        .resolve(name)
        .ok()
        .map(|res| res.oid)
        .ok_or_else(|| Die::Fatal(msg::could_not_parse_as_tree(name)))?;
    // A blob is reported when the merge reads it.
    Ok(r.peel_to(oid, ObjectKind::Tree, None).unwrap_or(oid))
}

/// `advice.submoduleMergeConflict` for submodules left in conflict.
fn advise_submodules(ctx: &Context, repo: &Repo, result: &MergeResult) -> Result<(), Die> {
    let paths: Vec<String> = result
        .messages
        .iter()
        .filter(|m| m.kind == MessageKind::SubmoduleNotInitialized)
        .map(|m| String::from_utf8_lossy(&m.paths[0]).into_owned())
        .collect();
    if paths.is_empty() {
        return Ok(());
    }
    let advisor = ctx.advisor(&repo.config)?;
    advisor
        .advise(
            rgit_messages::Advice::SubmoduleMergeConflict,
            &msg::submodule_merge_conflict_advice(&paths.join(" ")),
        )
        .map_err(Die::Fatal)
}

/// The output of a merge: tree, conflicted files, messages.
fn render(
    result: &MergeResult,
    nul: bool,
    name_only: bool,
    show_messages: bool,
    quote_high: bool,
) -> Vec<u8> {
    let end = if nul { b'\0' } else { b'\n' };
    let mut out = Vec::new();
    out.extend_from_slice(result.tree.to_hex().as_bytes());
    out.push(end);
    if !result.clean {
        for c in &result.conflicts {
            let mut path = Vec::new();
            if nul {
                path.extend_from_slice(&c.path);
            } else {
                crate::quote::write_quoted(&mut path, &c.path, quote_high);
            }
            for (i, v) in c.stages.iter().enumerate() {
                let Some(v) = v else { continue };
                if !name_only {
                    out.extend_from_slice(format!("{:06o} {} {}\t", v.mode, v.oid, i + 1).as_bytes());
                }
                out.extend_from_slice(&path);
                out.push(end);
                if name_only {
                    break;
                }
            }
        }
    }
    if show_messages {
        out.push(end);
        for m in &result.messages {
            if nul {
                out.extend_from_slice(m.paths.len().to_string().as_bytes());
                out.push(0);
                for p in &m.paths {
                    out.extend_from_slice(p);
                    out.push(0);
                }
                out.extend_from_slice(m.kind.as_str().as_bytes());
                out.push(0);
                out.extend_from_slice(&m.text);
                out.extend_from_slice(b"\n\0");
            } else {
                out.extend_from_slice(&m.text);
                out.push(b'\n');
            }
        }
    }
    out
}
