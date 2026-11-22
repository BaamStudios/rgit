// SPDX-License-Identifier: MIT
//! Messages, usage and option help of `git column` and the `column.*`
//! settings (CLEANROOM.md: user-visible strings are reproduced verbatim and
//! collected here).

pub const USAGE: &[&str] = &["git column [<options>]"];

pub const HELP_COMMAND: &str = "lookup config vars";
pub const HELP_MODE: &str = "layout to use";
pub const HELP_WIDTH: &str = "maximum width";
pub const HELP_INDENT: &str = "padding space on left border";
pub const HELP_NL: &str = "padding space on right border";
pub const HELP_PADDING: &str = "padding space between columns";

pub const PADDING_NEGATIVE: &str = "--padding must be non-negative";

/// `unsupported option '<token>'`.
pub fn unsupported_option(token: &str) -> String {
    format!("unsupported option '{token}'")
}

/// `invalid column.<cmd> mode <value>` (after [`unsupported_option`]).
pub fn invalid_mode(key: &str, value: &str) -> String {
    format!("invalid {key} mode {value}")
}
