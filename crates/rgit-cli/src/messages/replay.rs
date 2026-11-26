// SPDX-License-Identifier: MIT
//! Messages and usage of `git replay` (CLEANROOM.md: user-visible strings
//! are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &[
    "(EXPERIMENTAL!) git replay ([--contained] --onto=<newbase> | --advance=<branch> | --revert=<branch>)\n       [--ref=<ref>] [--ref-action=<mode>] [--linearize] <revision-range>",
];
pub const H_CONTAINED: &str = "update all branches that point at commits in <revision-range>";
pub const H_ONTO: &str = "replay onto given commit";
pub const H_ADVANCE: &str = "make replay advance given branch";
pub const H_REVERT: &str = "revert commits onto given branch";
pub const H_REF: &str = "reference to update with result";
pub const H_REF_ACTION: &str = "control ref update behavior (update|print)";
pub const H_LINEARIZE: &str = "drop merge commits, replaying only non-merge commits";

pub const NEED_MODE: &str = "exactly one of --onto, --advance, or --revert is required";
pub const ADVANCE_NOT_REF: &str = "argument to --advance must be a reference";
pub const REVERT_NOT_REF: &str = "argument to --revert must be a reference";
pub const MERGES_UNSUPPORTED: &str = "replaying merge commits is not supported yet!";
pub const NEED_COMMITS: &str = "need some commits to replay";
pub const REF_MULTIPLE: &str = "'--ref' cannot be used with multiple revision ranges";
pub const LINEARIZE_MULTIPLE: &str = "'--linearize' cannot be used with multiple branches";

pub fn together(a: &str, b: &str) -> String {
    format!("options '{a}' and '{b}' cannot be used together")
}

pub fn bad_onto(rev: &str) -> String {
    format!("'{rev}' is not a valid commit-ish for --onto")
}

pub fn ill_defined(opt: &str) -> String {
    format!(
        "'{opt}' cannot be used with multiple revision ranges because the ordering would be ill-defined"
    )
}

pub fn invalid_ref_action(v: &str) -> String {
    format!("invalid --ref-action value: '{v}'")
}

pub fn invalid_ref_action_config(v: &str) -> String {
    format!("invalid replay.refAction value: '{v}'")
}

pub fn invalid_refname(r: &str) -> String {
    format!("'{r}' is not a valid refname")
}

pub fn reflog_onto(onto: &str) -> String {
    format!("replay --onto {onto}")
}

pub fn reflog_branch(mode: &str, branch: &str) -> String {
    format!("replay --{mode} {branch}")
}
