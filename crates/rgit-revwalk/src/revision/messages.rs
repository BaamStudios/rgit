// SPDX-License-Identifier: MIT
//! User-visible texts of the revision parser, worded like git's (without the
//! `fatal: ` / `error: ` / `warning: ` / `hint: ` prefixes).

/// A name matches several references.
pub fn refname_ambiguous(name: &str) -> String {
    format!("refname '{name}' is ambiguous.")
}

/// `--symbolic-full-name` / `--abbrev-ref` of an ambiguous name (`error:`).
pub fn refname_ambiguous_error(name: &str) -> String {
    format!("refname '{name}' is ambiguous")
}

/// Advice after a full hex id that is also a ref name.
pub const OBJECT_NAME_WARNING_ADVICE: &str = "\
Git normally never creates a ref that ends with 40 hex characters
because it will be ignored when you just specify 40-hex. These refs
may be created by mistake. For example,

  git switch -c $br $(git rev-parse ...)

where \"$br\" is somehow empty and a 40-hex ref is created. Please
examine these refs and maybe delete them. Turn this message off by
running \"git config set advice.objectNameWarning false\"";

/// An abbreviated id matching several objects.
pub fn short_id_ambiguous(hex: &str) -> String {
    format!("short object ID {hex} is ambiguous")
}

/// First hint line before the candidates.
pub const CANDIDATES: &str = "The candidates are:";

/// `^{<type>}` that cannot be satisfied.
pub fn expected_type(name: &str, expected: &str, actual: &str) -> String {
    format!("{name}: expected {expected} type, but the object dereferences to {actual} type")
}

/// `~`, `^`, `^@` on something that is not a commit.
pub fn not_a_commit(hex: &str, kind: &str) -> String {
    format!("object {hex} is a {kind}, not a commit")
}

/// `<ref>@{<n>}` beyond the reflog.
pub fn log_only_has(name: &str, n: usize) -> String {
    format!("log for '{name}' only has {n} entries")
}

/// `<ref>@{<date>}` before the oldest reflog entry.
pub fn log_only_goes_back(name: &str, date: &str) -> String {
    format!("log for '{name}' only goes back to {date}")
}

/// `@{upstream}` / `@{push}` on a detached `HEAD`.
pub const HEAD_NOT_BRANCH: &str = "HEAD does not point to a branch";

/// `<branch>@{upstream}` for a branch that does not exist.
pub fn no_such_branch(name: &str) -> String {
    format!("no such branch: '{name}'")
}

/// `<branch>@{upstream}` without `branch.<name>.merge`.
pub fn no_upstream(name: &str) -> String {
    format!("no upstream configured for branch '{name}'")
}

/// The upstream branch has no remote-tracking counterpart.
pub fn upstream_not_tracked(merge: &str) -> String {
    format!("upstream branch '{merge}' not stored as a remote-tracking branch")
}

/// `@{push}` with `push.default=nothing`.
pub const PUSH_NOTHING: &str = "push has no destination (push.default is 'nothing')";

/// `@{push}` whose destination has no remote-tracking branch.
pub fn push_no_tracking(dst: &str, remote: &str) -> String {
    format!("push destination '{dst}' on remote '{remote}' has no local tracking branch")
}

/// `@{push}` with `push.default=simple` and different push and upstream.
pub const PUSH_SIMPLE_AMBIGUOUS: &str = "cannot resolve 'simple' push to a single destination";

/// `remote.<name>.push` does not cover the branch.
pub fn push_refspecs_exclude(remote: &str, branch: &str) -> String {
    format!("push refspecs for '{remote}' do not include '{branch}'")
}

/// `<rev>:<path>` with a revision that does not resolve.
pub fn invalid_object_name(name: &str) -> String {
    format!("invalid object name '{name}'.")
}

/// `<rev>:<path>` with a missing path.
pub fn path_not_in_rev(path: &str, rev: &str) -> String {
    format!("path '{path}' does not exist in '{rev}'")
}

/// `<rev>:<path>` with a path that exists only in the work tree.
pub fn path_on_disk_not_in_rev(path: &str, rev: &str) -> String {
    format!("path '{path}' exists on disk, but not in '{rev}'")
}

/// `<rev>:<path>` from a subdirectory where `<prefix><path>` exists.
pub fn path_exists_but_not(full: &str, path: &str) -> String {
    format!("path '{full}' exists, but not '{path}'")
}

/// Hint after [`path_exists_but_not`].
pub fn did_you_mean_rev_path(rev: &str, full: &str, path: &str) -> String {
    format!("Did you mean '{rev}:{full}' aka '{rev}:./{path}'?")
}

/// `:<path>` with a path that is neither in the index nor on disk.
pub fn path_not_anywhere(path: &str) -> String {
    format!("path '{path}' does not exist (neither on disk nor in the index)")
}

/// `:<path>` with a path that exists only on disk.
pub fn path_on_disk_not_in_index(path: &str) -> String {
    format!("path '{path}' exists on disk, but not in the index")
}

/// `:<n>:<path>` where the path is at stage 0 only.
pub fn path_not_at_stage(path: &str, stage: u8) -> String {
    format!("path '{path}' is in the index, but not at stage {stage}")
}

/// Hint after [`path_not_at_stage`].
pub fn did_you_mean_stage0(path: &str) -> String {
    format!("Did you mean ':0:{path}'?")
}

/// `:<path>` from a subdirectory where `<prefix><path>` is in the index.
pub fn path_in_index_but_not(full: &str, path: &str) -> String {
    format!("path '{full}' is in the index, but not '{path}'")
}

/// Hint after [`path_in_index_but_not`].
pub fn did_you_mean_index_path(full: &str, path: &str) -> String {
    format!("Did you mean ':0:{full}' aka ':0:./{path}'?")
}

/// `./` or `../` paths that leave the work tree.
pub fn outside_repository(path: &str, work_tree: &str) -> String {
    format!("'{path}' is outside repository at '{work_tree}'")
}

/// `./` paths while the current directory is outside the work tree.
pub const RELATIVE_OUTSIDE_WORK_TREE: &str =
    "relative path syntax can't be used outside working tree";
