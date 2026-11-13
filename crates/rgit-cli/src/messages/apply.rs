// SPDX-License-Identifier: MIT
//! Messages, usage lines and option help of `git apply` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here; they
//! were taken from the reference git's output).

pub const USAGE: &[&str] = &["git apply [<options>] [<patch>...]"];

pub const HELP_EXCLUDE: &str = "don't apply changes matching the given path";
pub const HELP_INCLUDE: &str = "apply changes matching the given path";
pub const HELP_P: &str = "remove <num> leading slashes from traditional diff paths";
pub const HELP_NO_ADD: &str = "ignore additions made by the patch";
pub const HELP_ADD: &str = "opposite of --no-add";
pub const HELP_STAT: &str = "instead of applying the patch, output diffstat for the input";
pub const HELP_NUMSTAT: &str = "show number of added and deleted lines in decimal notation";
pub const HELP_SUMMARY: &str = "instead of applying the patch, output a summary for the input";
pub const HELP_CHECK: &str = "instead of applying the patch, see if the patch is applicable";
pub const HELP_INDEX: &str = "make sure the patch is applicable to the current index";
pub const HELP_ITA: &str = "mark new files with `git add --intent-to-add`";
pub const HELP_CACHED: &str = "apply a patch without touching the working tree";
pub const HELP_UNSAFE: &str = "accept a patch that touches outside the working area";
pub const HELP_APPLY: &str = "also apply the patch (use with --stat/--summary/--check)";
pub const HELP_3WAY: &str = "attempt three-way merge, fall back on normal patch if that fails";
pub const HELP_OURS: &str = "for conflicts, use our version";
pub const HELP_THEIRS: &str = "for conflicts, use their version";
pub const HELP_UNION: &str = "for conflicts, use a union version";
pub const HELP_FAKE_ANCESTOR: &str = "build a temporary index based on embedded index information";
pub const HELP_Z: &str = "paths are separated with NUL character";
pub const HELP_C: &str = "ensure at least <n> lines of context match";
pub const HELP_WHITESPACE: &str = "detect new or modified lines that have whitespace errors";
pub const HELP_IGNORE_WS: &str = "ignore changes in whitespace when finding context";
pub const HELP_REVERSE: &str = "apply the patch in reverse";
pub const HELP_UNIDIFF_ZERO: &str = "don't expect at least one line of context";
pub const HELP_REJECT: &str = "leave the rejected hunks in corresponding *.rej files";
pub const HELP_OVERLAP: &str = "allow overlapping hunks";
pub const HELP_VERBOSE: &str = "be more verbose";
pub const HELP_QUIET: &str = "be more quiet";
pub const HELP_INACCURATE_EOF: &str =
    "tolerate incorrectly detected missing new-line at the end of file";
pub const HELP_RECOUNT: &str = "do not trust the line counts in the hunk headers";
pub const HELP_DIRECTORY: &str = "prepend <root> to all filenames";
pub const HELP_ALLOW_EMPTY: &str = "don't return error for empty patches";

pub const NO_VALID_PATCHES: &str = "No valid patches in input (allow with \"--allow-empty\")";
pub const OURS_NEEDS_3WAY: &str = "--ours, --theirs, and --union require --3way";
pub const REJECT_AND_3WAY: &str = "options '--reject' and '--3way' cannot be used together";
pub const REMOVAL_LEAVES_CONTENTS: &str = "removal patch leaves file contents";
pub const WHILE_SEARCHING: &str = "while searching for:";

/// Our own message: `--3way` is not implemented yet.
pub const THREEWAY_UNSUPPORTED: &str = "--3way is not supported by rgit yet";

pub fn outside_repository(option: &str) -> String {
    format!("'{option}' outside a repository")
}

pub fn bad_p_value(arg: &str) -> String {
    format!("option -p expects a non-negative integer, got '{arg}'")
}

pub fn bad_directory(arg: &str) -> String {
    format!("unable to normalize directory: '{arg}'")
}

pub fn bad_whitespace(arg: &str) -> String {
    format!("unrecognized whitespace option '{arg}'")
}

pub fn cannot_open(name: &str, err: &str) -> String {
    format!("can't open patch '{name}': {err}")
}

pub fn checking(name: &str) -> String {
    format!("Checking patch {name}...")
}

pub fn applied_cleanly(name: &str) -> String {
    format!("Applied patch {name} cleanly.")
}

pub fn skipped(name: &str) -> String {
    format!("Skipped patch '{name}'.")
}

pub fn hunk_offset(n: usize, line: usize, offset: i64) -> String {
    let s = if offset == 1 { "" } else { "s" };
    format!("Hunk #{n} succeeded at {line} (offset {offset} line{s}).")
}

pub fn context_reduced(leading: usize, trailing: usize, line: usize) -> String {
    format!("Context reduced to ({leading}/{trailing}) to apply fragment at {line}")
}

pub fn patch_failed(name: &str, line: usize) -> String {
    format!("patch failed: {name}:{line}")
}

pub fn does_not_apply(name: &str) -> String {
    format!("{name}: patch does not apply")
}

pub fn exists_in_worktree(name: &str) -> String {
    format!("{name}: already exists in working directory")
}

pub fn exists_in_index(name: &str) -> String {
    format!("{name}: already exists in index")
}

pub fn not_in_index(name: &str) -> String {
    format!("{name}: does not exist in index")
}

pub fn does_not_match_index(name: &str) -> String {
    format!("{name}: does not match index")
}

pub fn renamed_or_deleted(name: &str) -> String {
    format!("path {name} has been renamed/deleted")
}

pub fn path_error(name: &str, err: &str) -> String {
    format!("{name}: {err}")
}

pub fn beyond_symlink(name: &str) -> String {
    format!("affected file '{name}' is beyond a symbolic link")
}

pub fn invalid_path(name: &str) -> String {
    format!("invalid path '{name}'")
}

pub fn wrong_mode(name: &str, actual: u32, expected: u32) -> String {
    format!("{name} has type {actual:o}, expected {expected:o}")
}

pub fn applying_with_rejects(name: &str, n: usize) -> String {
    let s = if n == 1 { "" } else { "s" };
    format!("Applying patch {name} with {n} reject{s}...")
}

pub fn hunk_applied(n: usize) -> String {
    format!("Hunk #{n} applied cleanly.")
}

pub fn hunk_rejected(n: usize) -> String {
    format!("Rejected hunk #{n}.")
}

pub fn unable_to_write(name: &str, mode: u32, err: &str) -> String {
    format!("unable to write file '{name}' mode {mode:o}: {err}")
}

pub fn binary_without_index(name: &str) -> String {
    format!("cannot apply binary patch to '{name}' without full index line")
}

/// Our own message: binary patches are not applied yet (#150).
pub fn binary_unsupported(name: &str) -> String {
    format!("cannot apply binary patch to '{name}': not supported by rgit yet")
}

pub fn sha1_lacking(name: &str) -> String {
    format!("sha1 information is lacking or useless ({name}).")
}

/// The header of a `.rej` file.
pub fn rej_header(name: &str) -> String {
    format!("diff a/{name} b/{name}\t(rejected hunks)\n")
}

pub fn unable_to_add(name: &str) -> String {
    format!("unable to add cache entry for {name}")
}
