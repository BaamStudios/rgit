// SPDX-License-Identifier: MIT
//! Messages, usage and option help of `git grep` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &["git grep [<options>] [-e] <pattern> [<rev>...] [[--] <path>...]"];

pub const HELP_CACHED: &str = "search in index instead of in the work tree";
pub const HELP_NO_INDEX: &str = "find in contents not managed by git";
pub const HELP_INDEX: &str = "opposite of --no-index";
pub const HELP_UNTRACKED: &str = "search in both tracked and untracked files";
pub const HELP_EXCLUDE_STANDARD: &str = "ignore files specified via '.gitignore'";
pub const HELP_RECURSE_SUBMODULES: &str = "recursively search in each submodule";
pub const HELP_INVERT: &str = "show non-matching lines";
pub const HELP_ICASE: &str = "case insensitive matching";
pub const HELP_WORD: &str = "match patterns only at word boundaries";
pub const HELP_TEXT: &str = "process binary files as text";
pub const HELP_I: &str = "don't match patterns in binary files";
pub const HELP_TEXTCONV: &str = "process binary files with textconv filters";
pub const HELP_RECURSIVE: &str = "search in subdirectories (default)";
pub const HELP_MAX_DEPTH: &str = "descend at most <n> levels";
pub const HELP_ERE: &str = "use extended POSIX regular expressions";
pub const HELP_BRE: &str = "use basic POSIX regular expressions (default)";
pub const HELP_FIXED: &str = "interpret patterns as fixed strings";
pub const HELP_PERL: &str = "use Perl-compatible regular expressions";
pub const HELP_LINE_NUMBER: &str = "show line numbers";
pub const HELP_COLUMN: &str = "show column number of first match";
pub const HELP_NO_FILENAME: &str = "don't show filenames";
pub const HELP_FILENAME: &str = "show filenames";
pub const HELP_FULL_NAME: &str = "show filenames relative to top directory";
pub const HELP_FILES_WITH_MATCHES: &str = "show only filenames instead of matching lines";
pub const HELP_NAME_ONLY: &str = "synonym for --files-with-matches";
pub const HELP_FILES_WITHOUT_MATCH: &str = "show only the names of files without match";
pub const HELP_NULL: &str = "print NUL after filenames";
pub const HELP_ONLY_MATCHING: &str = "show only matching parts of a line";
pub const HELP_COUNT: &str = "show the number of matches instead of matching lines";
pub const HELP_COLOR: &str = "highlight matches";
pub const HELP_BREAK: &str = "print empty line between matches from different files";
pub const HELP_HEADING: &str = "show filename only once above matches from same file";
pub const HELP_CONTEXT: &str = "show <n> context lines before and after matches";
pub const HELP_BEFORE: &str = "show <n> context lines before matches";
pub const HELP_AFTER: &str = "show <n> context lines after matches";
pub const HELP_THREADS: &str = "use <n> worker threads";
pub const HELP_NUM: &str = "shortcut for -C NUM";
pub const HELP_SHOW_FUNCTION: &str = "show a line with the function name before matches";
pub const HELP_FUNCTION_CONTEXT: &str = "show the surrounding function";
pub const HELP_FILE: &str = "read patterns from file";
pub const HELP_PATTERN: &str = "match <pattern>";
pub const HELP_AND: &str = "combine patterns specified with -e";
pub const HELP_QUIET: &str = "indicate hit with exit status without output";
pub const HELP_ALL_MATCH: &str = "show only matches from files that match all patterns";
pub const HELP_OPEN_PAGER: &str = "show matching files in the pager";
pub const HELP_EXT_GREP: &str = "allow calling of grep(1) (ignored by this build)";
pub const HELP_MAX_COUNT: &str = "maximum number of results per file";

pub const NO_PATTERN: &str = "no pattern given";
pub const CACHED_AND_TREES: &str = "both --cached and trees are given";
pub const NO_INDEX_WITH_REVS: &str = "--no-index or --untracked cannot be used with revs";
pub const NUL_IN_PATTERN: &str = "given pattern contains NULL byte (via -f <file>). This is only supported with -P under PCRE v2";
/// PCRE2's reason for an unknown `(*VERB)` at the start of a `-P` pattern.
pub const PCRE_BAD_VERB: &str = "(*VERB) not recognized or malformed";
pub const ATTR_SOURCE_WITHOUT_REPO: &str =
    "cannot use --attr-source or GIT_ATTR_SOURCE without repo";
pub const UNTRACKED_WITH_SUBMODULES: &str = "--untracked not supported with --recurse-submodules";
pub const OPEN_ONLY_WORKTREE: &str = "--open-files-in-pager only works on the worktree";
pub const AND_NOT_PRECEDED: &str = "--and not preceded by pattern expression";
pub const AND_NOT_FOLLOWED: &str = "--and not followed by pattern expression";
pub const NOT_NOT_FOLLOWED: &str = "--not not followed by pattern expression";
pub const UNMATCHED_PAREN: &str = "unmatched ( for expression group";
pub const NOT_A_PATTERN: &str = "not a pattern expression";

pub fn incomplete_group(token: &str) -> String {
    format!("incomplete pattern expression group: {token}")
}

pub fn cannot_open(file: &str, reason: &str) -> String {
    format!("cannot open '{file}': {reason}")
}

pub fn bad_threads(n: i64) -> String {
    format!("invalid number of threads specified ({n})")
}

pub fn bad_config_threads(n: i64) -> String {
    format!("invalid number of threads specified ({n}) for grep.threads")
}

pub fn options_together(a: &str, b: &str) -> String {
    format!("options '{a}' and '{b}' cannot be used together")
}

pub fn unresolved_revision(name: &str) -> String {
    format!("unable to resolve revision: {name}")
}

pub fn outside_directory(path: &str) -> String {
    format!("'{path}' is outside the directory tree")
}

/// The pattern that did not compile, as git reports it.
pub fn bad_pattern(pattern: &str, reason: &str) -> String {
    format!("command line, '{pattern}': {reason}")
}

/// `Binary file <name> matches` around the (coloured) name.
pub const BINARY_BEFORE: &str = "Binary file ";
pub const BINARY_AFTER: &str = " matches\n";

pub fn unable_to_read(path: &str) -> String {
    format!("unable to read {path}")
}

pub fn textconv_failed(cmd: &str) -> String {
    format!("unable to read files to diff: textconv {cmd} failed")
}

pub fn expects_numerical(switch: char) -> String {
    format!("switch `{switch}' expects a numerical value")
}
