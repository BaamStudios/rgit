// SPDX-License-Identifier: MIT
//! Messages, usage lines and option help of `git merge-tree` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &[
    "git merge-tree [--write-tree] [<options>] <branch1> <branch2>",
    "git merge-tree [--trivial-merge] <base-tree> <branch1> <branch2>",
];
pub const HELP_WRITE_TREE: &str = "do a real merge instead of a trivial merge";
pub const HELP_TRIVIAL_MERGE: &str = "do a trivial merge only";
pub const HELP_MESSAGES: &str = "also show informational/conflict messages";
pub const HELP_QUIET: &str = "suppress all output; only exit status wanted";
pub const HELP_Z: &str = "separate paths with the NUL character";
pub const HELP_NAME_ONLY: &str = "list filenames without modes/oids/stages";
pub const HELP_ALLOW_UNRELATED: &str = "allow merging unrelated histories";
pub const HELP_STDIN: &str = "perform multiple merges, one per line of input";
pub const HELP_MERGE_BASE: &str = "specify a merge-base for the merge";
pub const HELP_STRATEGY_OPTION: &str = "option for selected merge strategy";

pub fn not_something_we_can_merge(name: &str) -> String {
    format!("merge-tree: {name} - not something we can merge")
}

pub fn could_not_parse_as_tree(name: &str) -> String {
    format!("could not parse as tree '{name}'")
}

pub fn unknown_strategy_option(opt: &str) -> String {
    format!("unknown strategy option: -X{opt}")
}

pub fn strategy_option_not_supported(opt: &str) -> String {
    format!("rgit merge-tree: -X{opt} is not supported yet")
}

pub const TRIVIAL_MERGE_NOT_SUPPORTED: &str =
    "rgit merge-tree: --trivial-merge is not supported yet";
pub const STDIN_NOT_SUPPORTED: &str = "rgit merge-tree: --stdin is not supported yet";

/// `advice.submoduleMergeConflict`, naming the conflicted submodules.
pub fn submodule_merge_conflict_advice(paths: &str) -> String {
    format!(
        "Recursive merging with submodules currently only supports trivial cases.
Please manually handle the merging of each conflicted submodule.
This can be accomplished with the following steps:
 - come back to superproject and run:

      git add {paths}

   to record the above merge or update
 - resolve any other conflicts in the superproject
 - commit the resulting index in the superproject
"
    )
}
