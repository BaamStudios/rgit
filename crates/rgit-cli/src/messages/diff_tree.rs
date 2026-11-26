// SPDX-License-Identifier: MIT
//! Messages and option help of `git diff-tree` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &[
    "git diff-tree [--stdin] [-m] [-s] [-v] [--no-commit-id] [--pretty]\n              [-t] [-r] [-c | --cc] [--combined-all-paths] [--root] [--merge-base]\n              [<common-diff-options>] <tree-ish> [<tree-ish>] [<path>...]",
];

/// The options listed by `-h` before the common diff options.
pub const OPTIONS: &str = "\
\x20 -r            diff recursively
  -c            show combined diff for merge commits
  --cc          show combined diff for merge commits removing uninteresting hunks
  --combined-all-paths
                show name of file in all parents for combined diffs
  --root        include the initial commit as diff against /dev/null
";

/// `error:` for a `--stdin` tree line without a second tree.
pub const NEED_TWO_TREES: &str = "Need exactly two trees, separated by a space";

/// `error:` for a `--stdin` line naming another kind of object.
pub fn not_commit_or_tree(hex: &str, kind: &str) -> String {
    format!("Object {hex} is a {kind}, not a commit or tree")
}

pub fn bad_object(name: &str) -> String {
    format!("ambiguous argument '{name}': unknown revision or path not in the working tree.")
}
