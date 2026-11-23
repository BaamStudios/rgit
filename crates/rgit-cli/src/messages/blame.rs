// SPDX-License-Identifier: MIT
//! Messages, usage and option help of `git blame` and `git annotate`
//! (CLEANROOM.md: user-visible strings are reproduced verbatim and
//! collected here).

pub const USAGE: &[&str] = &[
    "git blame [<options>] [<rev-opts>] [<rev>] [--] <file>",
    "",
    "<rev-opts> are documented in git-rev-list(1)",
];

pub const USAGE_ANNOTATE: &[&str] = &[
    "git annotate [<options>] [<rev-opts>] [<rev>] [--] <file>",
    "",
    "<rev-opts> are documented in git-rev-list(1)",
];

pub const HELP_INCREMENTAL: &str = "show blame entries as we find them, incrementally";
pub const HELP_BLANK_BOUNDARY: &str = "do not show object names of boundary commits (Default: off)";
pub const HELP_ROOT: &str = "do not treat root commits as boundaries (Default: off)";
pub const HELP_SHOW_STATS: &str = "show work cost statistics";
pub const HELP_PROGRESS: &str = "force progress reporting";
pub const HELP_SCORE_DEBUG: &str = "show output score for blame entries";
pub const HELP_SHOW_NAME: &str = "show original filename (Default: auto)";
pub const HELP_SHOW_NUMBER: &str = "show original linenumber (Default: off)";
pub const HELP_PORCELAIN: &str = "show in a format designed for machine consumption";
pub const HELP_LINE_PORCELAIN: &str = "show porcelain format with per-line commit information";
pub const HELP_ANNOTATE: &str = "use the same output mode as git-annotate (Default: off)";
pub const HELP_RAW_TIME: &str = "show raw timestamp (Default: off)";
pub const HELP_LONG: &str = "show long commit SHA1 (Default: off)";
pub const HELP_NO_AUTHOR: &str = "suppress author name and timestamp (Default: off)";
pub const HELP_SHOW_EMAIL: &str = "show author email instead of name (Default: off)";
pub const HELP_IGNORE_WS: &str = "ignore whitespace differences";
pub const HELP_DIFF_ALGORITHM: &str = "choose a diff algorithm";
pub const HELP_IGNORE_REV: &str = "ignore <rev> when blaming";
pub const HELP_IGNORE_REVS_FILE: &str = "ignore revisions from <file>";
pub const HELP_COLOR_LINES: &str = "color redundant metadata from previous line differently";
pub const HELP_COLOR_BY_AGE: &str = "color lines by age";
pub const HELP_REVS_FILE: &str = "use revisions from <file> instead of calling git-rev-list";
pub const HELP_CONTENTS: &str = "use <file>'s contents as the final image";
pub const HELP_COPIES: &str = "find line copies within and across files";
pub const HELP_MOVES: &str = "find line movements within and across files";
pub const HELP_RANGE: &str = "process only line range <start>,<end> or function :<funcname>";
pub const HELP_ABBREV: &str = "use <n> digits to display object names";

/// Author of the lines that differ in the work tree.
pub const NOT_COMMITTED_YET: &str = "Not Committed Yet";
pub const NOT_COMMITTED_YET_MAIL: &str = "<not.committed.yet>";
/// Author of the lines that differ in the `--contents` file.
pub const EXTERNAL_FILE: &str = "External file (--contents)";
pub const EXTERNAL_FILE_MAIL: &str = "<external.file>";

pub const INVALID_EMPTY_RANGE: &str = "-L invalid empty range";
pub const REVERSE_NOT_SUPPORTED: &str = "--reverse is not supported yet";
pub const REVS_FILE_NOT_SUPPORTED: &str = "-S is not supported yet";

pub fn summary(path: &str, source: &str) -> String {
    format!("Version of {path} from {source}")
}

pub fn invalid_line_number(n: &str) -> String {
    format!("-L invalid line number: {n}")
}

pub fn no_match(re: &str, line: usize) -> String {
    format!("-L parameter '{re}' starting at line {line}: No match")
}

pub fn no_funcname_match(re: &str, line: usize) -> String {
    format!("-L parameter '{re}' starting at line {line}: no match")
}

pub fn invalid_regex(re: &str, reason: &str) -> String {
    format!("-L parameter '{re}': {reason}")
}

pub fn only_lines(path: &str, n: usize) -> String {
    let unit = if n == 1 { "line" } else { "lines" };
    format!("file {path} has only {n} {unit}")
}

pub fn no_such_path_in_head(path: &str) -> String {
    format!("no such path '{path}' in HEAD")
}

pub fn no_such_path_in(path: &str, rev: &str) -> String {
    format!("no such path {path} in {rev}")
}

pub fn cannot_lstat(path: &str, err: &str) -> String {
    format!("Cannot lstat '{path}': {err}")
}

pub fn cannot_stat(path: &str, err: &str) -> String {
    format!("Cannot stat '{path}': {err}")
}

pub const PROGRESS_WITH_PORCELAIN: &str =
    "--progress can't be used with --incremental or porcelain formats";

pub fn progress_done(n: usize) -> String {
    format!("Blaming lines: 100% ({n}/{n}), done.")
}

pub fn cannot_read(path: &str) -> String {
    format!("cannot open or read '{path}'")
}

pub fn bad_revision(rev: &str) -> String {
    format!("bad revision '{rev}'")
}

pub fn more_than_one(a: &str, b: &str) -> String {
    format!("More than one commit to dig from {a} and {b}?")
}

pub fn unknown_option(opt: &str) -> String {
    format!("unknown option `{opt}'")
}

pub const INVALID_DIFF_ALGORITHM: &str =
    "option diff-algorithm accepts \"myers\", \"minimal\", \"patience\" and \"histogram\"";

pub fn invalid_color(value: &str) -> String {
    format!("invalid color '{value}' in color.blame.highlightRecent")
}

pub fn unable_to_read(hex: &str) -> String {
    format!("unable to read {hex}")
}

pub const HELP_TEXTCONV: &str = "use textconv filters for the blamed file";

pub fn cannot_find_ignored(rev: &str) -> String {
    format!("cannot find revision {rev} to ignore")
}

pub fn cannot_open_revs_file(path: &str) -> String {
    format!("could not open object name list: {path}")
}

pub fn invalid_object_name(name: &str) -> String {
    format!("invalid object name: {name}")
}
