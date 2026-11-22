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

pub const TRIVIAL_INCOMPATIBLE: &str = "--trivial-merge is incompatible with all other options";

// Headings of the trivial-merge mode.
pub const TRIVIAL_ADDED_IN_REMOTE: &str = "added in remote";
pub const TRIVIAL_ADDED_IN_BOTH: &str = "added in both";
pub const TRIVIAL_REMOVED_IN_REMOTE: &str = "removed in remote";
pub const TRIVIAL_REMOVED_IN_LOCAL: &str = "removed in local";
pub const TRIVIAL_CHANGED_IN_BOTH: &str = "changed in both";
pub const TRIVIAL_MERGED: &str = "merged";

/// `advice.submoduleMergeConflict`, naming the conflicted submodules.
/// `steps` are the lines for submodules that are checked out
/// ([`submodule_merge_step`]).
pub fn malformed_input_line(line: &str) -> String {
    format!("malformed input line: '{line}'.")
}

pub fn submodule_merge_step(path: &str, commit: &str) -> String {
    format!(
        " - go to submodule ({path}), and either merge commit {commit}\n   \
         or update to an existing commit which has merged those changes\n"
    )
}

pub fn submodule_merge_conflict_advice(paths: &str, steps: &str) -> String {
    format!(
        "Recursive merging with submodules currently only supports trivial cases.
Please manually handle the merging of each conflicted submodule.
This can be accomplished with the following steps:
{steps} - come back to superproject and run:

      git add {paths}

   to record the above merge or update
 - resolve any other conflicts in the superproject
 - commit the resulting index in the superproject
"
    )
}
