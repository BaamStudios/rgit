// SPDX-License-Identifier: MIT
//! Messages, usage lines and option help of `git cat-file` (CLEANROOM.md: user-visible
//! strings are reproduced verbatim and collected here).

pub use super::objects::*;

pub const CAT_FILE_USAGE: &[&str] = &[
    "git cat-file <type> <object>",
    "git cat-file (-e | -p | -t | -s) <object>",
    "git cat-file (--textconv | --filters)\n                    [<rev>:<path|tree-ish> | --path=<path|tree-ish> <rev>]",
    "git cat-file (--batch | --batch-check | --batch-command) [--batch-all-objects]\n                    [--buffer] [--follow-symlinks] [--unordered]\n                    [--textconv | --filters] [-Z]",
];
pub const CAT_FILE_GROUP_CHECK: &str = "Check object existence or emit object contents";
pub const CAT_FILE_HELP_E: &str = "check if <object> exists";
pub const CAT_FILE_HELP_P: &str = "pretty-print <object> content";
pub const CAT_FILE_GROUP_ATTRS: &str = "Emit [broken] object attributes";
pub const CAT_FILE_HELP_T: &str = "show object type (one of 'blob', 'tree', 'commit', 'tag', ...)";
pub const CAT_FILE_HELP_S: &str = "show object size";
pub const CAT_FILE_HELP_UNKNOWN_TYPE: &str = "historical option -- no-op";
pub const CAT_FILE_HELP_MAILMAP: &str = "use mail map file";
pub const CAT_FILE_GROUP_BATCH: &str = "Batch objects requested on stdin (or --batch-all-objects)";
pub const CAT_FILE_HELP_BATCH: &str = "show full <object> or <rev> contents";
pub const CAT_FILE_HELP_BATCH_CHECK: &str = "like --batch, but don't emit <contents>";
pub const CAT_FILE_HELP_Z_UPPER: &str = "stdin and stdout is NUL-terminated";
pub const CAT_FILE_HELP_Z_LOWER: &str = "stdin is NUL-terminated";
pub const CAT_FILE_HELP_BATCH_COMMAND: &str = "read commands from stdin";
pub const CAT_FILE_HELP_ALL: &str =
    "with --batch[-check]: ignores stdin, batches all known objects";
pub const CAT_FILE_GROUP_OPTIMIZE: &str = "Change or optimize batch output";
pub const CAT_FILE_HELP_BUFFER: &str = "buffer --batch output";
pub const CAT_FILE_HELP_FOLLOW: &str = "follow in-tree symlinks";
pub const CAT_FILE_HELP_UNORDERED: &str = "do not order objects before emitting them";
pub const CAT_FILE_GROUP_FILTER: &str =
    "Emit object (blob or tree) with conversion or filter (stand-alone, or with batch)";
pub const CAT_FILE_HELP_TEXTCONV: &str = "run textconv on object's content";
pub const CAT_FILE_HELP_FILTERS: &str = "run filters on object's content";
pub const CAT_FILE_HELP_PATH: &str = "use a <path> for (--textconv | --filters); Not with 'batch'";
pub const CAT_FILE_HELP_FILTER: &str = "object filtering";
pub const ONLY_ONE_BATCH: &str = "only one batch option may be specified";
pub const COULD_NOT_GET_INFO: &str = "git cat-file: could not get object info";
pub const BATCH_NO_ARGS: &str = "batch modes take no arguments";
pub const TOO_MANY_ARGS: &str = "too many arguments";
pub const PATH_NEEDS_FILTERS: &str = "'--path=<path|tree-ish>' needs '--filters' or '--textconv'";
pub const EMPTY_COMMAND: &str = "empty command in input";

pub fn bad_file(name: &str) -> String {
    format!("git cat-file {name}: bad file")
}

pub fn incompatible_with_batch(opt: &str) -> String {
    format!("'{opt}' is incompatible with batch mode")
}

pub fn requires_batch(opt: &str) -> String {
    format!("'{opt}' requires a batch mode")
}

pub fn object_required(what: &str, opt: &str) -> String {
    format!("{what} required with '{opt}'")
}

pub fn only_two_args(n: usize) -> String {
    format!("only two arguments allowed in <type> <object> mode, not {n}")
}

pub fn bad_format(what: &str) -> String {
    format!("bad cat-file format: {what}")
}

pub fn bad_format_unterminated(element: &str) -> String {
    format!("bad cat-file format: element '{element}' does not end in ')'")
}

pub fn whitespace_before_command(line: &str) -> String {
    format!("whitespace before command: '{line}'")
}

pub fn unknown_command(line: &str) -> String {
    format!("unknown command: '{line}'")
}

pub fn requires_arguments(cmd: &str) -> String {
    format!("{cmd} requires arguments")
}

pub fn takes_no_arguments(cmd: &str) -> String {
    format!("{cmd} takes no arguments")
}

pub fn only_for_buffer(cmd: &str) -> String {
    format!("{cmd} is only for --buffer mode")
}

/// rgit wording: a cat-file feature that is not implemented yet.
pub fn cat_file_not_supported(what: &str) -> String {
    format!("rgit cat-file: {what} is not supported yet")
}

/// Writing the output failed (e.g. a full disk).
pub fn unable_to_write_stdout(err: &str) -> String {
    format!("unable to write to stdout: {err}")
}
