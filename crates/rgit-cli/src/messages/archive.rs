// SPDX-License-Identifier: MIT
//! Messages, usage and option help of `git archive`, `git upload-archive`
//! and `git get-tar-commit-id` (CLEANROOM.md: user-visible strings are
//! reproduced verbatim and collected here).

pub const USAGE: &[&str] = &[
    "git archive [<options>] <tree-ish> [<path>...]",
    "git archive --list",
    "git archive --remote <repo> [--exec <cmd>] [<options>] <tree-ish> [<path>...]",
    "git archive --remote <repo> [--exec <cmd>] --list",
];

pub const UPLOAD_ARCHIVE_USAGE: &str = "usage: git upload-archive <repository>";
pub const GET_TAR_COMMIT_ID_USAGE: &str = "usage: git get-tar-commit-id";

pub const HELP_FORMAT: &str = "archive format";
pub const HELP_PREFIX: &str = "prepend prefix to each pathname in the archive";
pub const HELP_ADD_FILE: &str = "add untracked file to archive";
pub const HELP_OUTPUT: &str = "write the archive to this file";
pub const HELP_WORKTREE_ATTRIBUTES: &str = "read .gitattributes in working directory";
pub const HELP_VERBOSE: &str = "report archived files on stderr";
pub const HELP_MTIME: &str = "set modification time of archive entries";
pub const HELP_LEVEL: &str = "set compression level";
pub const HELP_LIST: &str = "list supported archive formats";
pub const HELP_REMOTE: &str = "retrieve the archive from remote repository <repo>";
pub const HELP_EXEC: &str = "path to the remote git-upload-archive command";

pub const EOF_BEFORE_HEADER: &str =
    "git get-tar-commit-id: EOF before reading tar header: No such file or directory";

pub fn unknown_format(fmt: &str) -> String {
    format!("Unknown archive format '{fmt}'")
}

pub fn argument_not_supported(fmt: &str, level: &str) -> String {
    format!("Argument not supported for format '{fmt}': -{level}")
}

pub fn extra_parameter(arg: &str) -> String {
    format!("extra command line parameter '{arg}'")
}

pub fn not_valid_object_name(name: &str) -> String {
    format!("not a valid object name: {name}")
}

pub fn not_a_tree(id: &str) -> String {
    format!("not a tree object: {id}")
}

pub fn no_such_ref(name: &str) -> String {
    format!("no such ref: {name}")
}

pub fn pathspec_did_not_match(spec: &str) -> String {
    format!("pathspec '{spec}' did not match any files")
}

pub fn pathspec_outside(spec: &str) -> String {
    format!("pathspec '{spec}' matches files outside the current directory")
}

pub fn file_not_found(path: &str) -> String {
    format!("File not found: {path}")
}

pub fn not_a_regular_file(path: &str) -> String {
    format!("Not a regular file: {path}")
}

pub fn not_valid_utf8(path: &str) -> String {
    format!("path is not valid UTF-8: {path}")
}

pub fn unable_to_start_filter(cmd: &str, err: &str) -> String {
    format!("unable to start '{cmd}' filter: {err}")
}

pub fn filter_failed(cmd: &str) -> String {
    format!("'{cmd}' filter reported error")
}

pub fn deflate_level(level: &str) -> String {
    let _ = level;
    "deflateInit2: stream consistency error (no message)".to_string()
}

pub fn could_not_read(id: &str) -> String {
    format!("cannot read '{id}'")
}

pub fn not_a_repository(dir: &str) -> String {
    format!("'{dir}' does not appear to be a git repository")
}

pub fn could_not_create(path: &str, err: &str) -> String {
    format!("could not create archive file '{path}': {err}")
}

pub const EXEC_REQUIRES_REMOTE: &str = "the option '--exec' requires '--remote'";
pub const NOT_A_REPOSITORY: &str = "not a git repository (or any of the parent directories): .git";

pub fn unexpected_option(opt: &str) -> String {
    format!("Unexpected option {opt}")
}

pub fn missing_colon(arg: &str) -> String {
    format!("missing colon: '{arg}'")
}

pub fn unclosed_quote(arg: &str) -> String {
    format!("unclosed quote: '{arg}'")
}
