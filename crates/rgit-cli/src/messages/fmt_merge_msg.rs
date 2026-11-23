// SPDX-License-Identifier: MIT
//! Messages of `git fmt-merge-msg`.

pub const USAGE: &[&str] =
    &["git fmt-merge-msg [-m <message>] [--log[=<n>] | --no-log] [--file <file>]"];
pub const HELP_LOG: &str = "populate log with at most <n> entries from shortlog";
pub const HELP_SUMMARY: &str = "alias for --log (deprecated)";
pub const HELP_MESSAGE: &str = "use <text> as start of message";
pub const HELP_INTO_NAME: &str = "use <name> instead of the real target branch";
pub const HELP_FILE: &str = "file to read from";
pub const LOG_NOT_A_NUMBER: &str =
    "option `log' expects an integer value with an optional k/m/g suffix";

pub fn cannot_open(path: &str, err: &str) -> String {
    format!("cannot open '{path}': {err}")
}

pub fn error_in_line(n: usize, line: &str) -> String {
    format!("error in line {n}: {line}")
}
