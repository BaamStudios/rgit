// SPDX-License-Identifier: MIT
//! `git merge-resolve <base>... -- <head> <remote>` (Documentation/
//! merge-strategies.adoc, `resolve`): the strategy program behind
//! `merge -s resolve`, built from the plumbing as the documentation
//! describes it: a three-way `read-tree -m -u --aggressive`, and when the
//! trivial merge leaves unmerged paths, `merge-index -o
//! git-merge-one-file -a`. Exit 0 when clean, 1 with conflicts, 2 when
//! the strategy cannot handle the merge (no base, several remotes, local
//! changes). The messages were taken from the behaviour of the `git`
//! binary.

use crate::commands::merge_one_file::Plumbing;
use crate::messages::merge as mmsg;
use crate::messages::merge_plumbing as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    if argv.get(1).is_some_and(|a| a == "-h") {
        println!("{}", msg::script_usage("merge-resolve"));
        return Ok(0);
    }
    let repo = Repo::open(ctx, Mode::Required)?;
    let git = Plumbing::at_top(ctx, &repo);
    let a = StrategyArgs::parse(&argv[1..]);
    if a.bases.is_empty() || a.remotes.len() > 1 {
        return Ok(2);
    }
    if local_changes(&git)? {
        return Ok(2);
    }
    println!("{}", mmsg::TRYING_SIMPLE);
    let mut rt = vec!["read-tree", "-u", "-m", "--aggressive"];
    rt.extend(a.bases.iter().map(String::as_str));
    rt.extend(a.head.as_deref());
    rt.extend(a.remotes.iter().map(String::as_str));
    if git.run(&rt)? != 0 {
        return Ok(2);
    }
    if write_tree(&git)?.is_some() {
        return Ok(0);
    }
    println!("{}", mmsg::SIMPLE_FAILED);
    let code = git.run(["merge-index", "-o", "git-merge-one-file", "-a"])?;
    Ok(if code == 0 { 0 } else { 1 })
}

/// The arguments of a strategy program: `<base>... -- <head> <remote>...`.
pub(crate) struct StrategyArgs {
    pub bases: Vec<String>,
    pub head: Option<String>,
    pub remotes: Vec<String>,
    /// Whether `--` was given.
    pub separated: bool,
}

impl StrategyArgs {
    pub fn parse(args: &[String]) -> StrategyArgs {
        let mut a = StrategyArgs {
            bases: Vec::new(),
            head: None,
            remotes: Vec::new(),
            separated: false,
        };
        let mut it = args.iter();
        for x in it.by_ref() {
            if x == "--" {
                a.separated = true;
                break;
            }
            a.bases.push(x.clone());
        }
        a.head = it.next().cloned();
        a.remotes = it.cloned().collect();
        a
    }
}

/// Refreshes the index and refuses (with git's message) when it differs
/// from `HEAD`.
pub(crate) fn local_changes(git: &Plumbing) -> Result<bool, Die> {
    git.run(["update-index", "-q", "--refresh"])?;
    let (code, out) = git.output(["diff-index", "--cached", "--name-only", "HEAD", "--"])?;
    if code != 0 {
        return Ok(true);
    }
    let paths: Vec<String> = String::from_utf8_lossy(&out)
        .lines()
        .map(str::to_owned)
        .collect();
    if paths.is_empty() {
        return Ok(false);
    }
    println!("{}", mmsg::resolve_local_changes(&paths));
    Ok(true)
}

/// `git write-tree`, quietly; the tree when the index has no unmerged
/// paths.
pub(crate) fn write_tree(git: &Plumbing) -> Result<Option<String>, Die> {
    let (code, out) = git.output_quiet(["write-tree"])?;
    if code != 0 {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&out).trim().to_owned()))
}
