// SPDX-License-Identifier: MIT
//! `git merge-octopus <base>... -- <head> <remote>...` (Documentation/
//! merge-strategies.adoc, `octopus`): the strategy program behind
//! `merge -s octopus`. The remotes are merged one after the other into
//! the index and work tree: a remote that is already contained is
//! skipped, one that only moves forward is fast-forwarded while nothing
//! was merged yet, the others get a trivial three-way `read-tree`
//! followed by `merge-index -o git-merge-one-file -a`. Only the last
//! remote may leave conflicts. Exit 0 when clean, 1 with conflicts, 2 when
//! the strategy does not apply. The messages were taken from the behaviour
//! of the `git` binary.

use crate::commands::merge_one_file::Plumbing;
use crate::commands::merge_resolve::{StrategyArgs, local_changes, write_tree};
use crate::messages::merge as mmsg;
use crate::messages::merge_plumbing as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    if argv.get(1).is_some_and(|a| a == "-h") {
        println!("{}", msg::script_usage("merge-octopus"));
        return Ok(0);
    }
    let repo = Repo::open(ctx, Mode::Required)?;
    let git = Plumbing::at_top(ctx, &repo);
    let a = StrategyArgs::parse(&argv[1..]);
    if !a.separated || a.remotes.len() < 2 {
        return Ok(2);
    }
    if local_changes(&git)? {
        return Ok(2);
    }
    let lines = |out: &[u8]| -> Vec<String> {
        String::from_utf8_lossy(out)
            .lines()
            .map(str::to_owned)
            .collect()
    };
    let head = a.head.clone().unwrap_or_default();
    let (code, out) = git.output(["rev-parse", "--verify", &format!("{head}^0")])?;
    if code != 0 {
        return Ok(2);
    }
    let mut merged = lines(&out);
    let Some(mut tree) = write_tree(&git)? else {
        return Ok(2);
    };
    let mut non_ff = false;
    let mut failed = false;
    for remote in &a.remotes {
        if failed {
            print!("{}", mmsg::OCTOPUS_STOP);
            return Ok(2);
        }
        let (code, out) = git.output(["rev-parse", "--verify", &format!("{remote}^0")])?;
        if code != 0 {
            return Ok(2);
        }
        let sha = String::from_utf8_lossy(&out).trim().to_owned();
        let pretty = std::env::var(format!("GITHEAD_{sha}")).unwrap_or_else(|_| remote.clone());
        let mut mb = vec!["merge-base", "--all", sha.as_str()];
        mb.extend(merged.iter().map(String::as_str));
        let (code, out) = git.output(&mb)?;
        if code != 0 {
            return Ok(2);
        }
        let common = lines(&out);
        if common.len() == 1 && common[0] == sha {
            print!("{}", mmsg::octopus_up_to_date(&pretty));
            continue;
        }
        if common == merged && !non_ff {
            print!("{}", mmsg::octopus_fast_forward(&pretty));
            if git.run(["read-tree", "-u", "-m", tree.as_str(), sha.as_str()])? != 0 {
                return Ok(2);
            }
            let Some(t) = write_tree(&git)? else {
                return Ok(2);
            };
            tree = t;
            merged = vec![sha];
            continue;
        }
        non_ff = true;
        print!("{}", mmsg::octopus_trying(&pretty));
        let mut rt = vec!["read-tree", "-u", "-m", "--aggressive"];
        rt.extend(common.iter().map(String::as_str));
        rt.push(&tree);
        rt.push(&sha);
        if git.run(&rt)? != 0 {
            return Ok(2);
        }
        let next = match write_tree(&git)? {
            Some(t) => t,
            None => {
                println!("{}", msg::SIMPLE_MERGE_FAILED_OCTOPUS);
                if git.run(["merge-index", "-o", "git-merge-one-file", "-a"])? != 0 {
                    failed = true;
                }
                write_tree(&git)?.unwrap_or_default()
            }
        };
        merged.push(sha);
        tree = next;
    }
    Ok(if failed { 1 } else { 0 })
}
