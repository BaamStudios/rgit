// SPDX-License-Identifier: MIT
//! Messages of the pager and of the colour options (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub fn cannot_run(program: &str, reason: &str) -> String {
    format!("cannot run {program}: {reason}")
}

pub fn unable_to_execute_pager(pager: &str) -> String {
    format!("unable to execute pager '{pager}'")
}

/// `--color=<when>` with an unknown value.
pub fn color_expects(option: &str) -> String {
    format!("option `{option}' expects \"always\", \"auto\", or \"never\"")
}

/// `-h` text of `--color[=<when>]`.
pub const HELP_COLOR: &str = "use colored output";
