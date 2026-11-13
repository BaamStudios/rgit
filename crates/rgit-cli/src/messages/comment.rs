// SPDX-License-Identifier: MIT
//! Messages about the comment string and other configuration values
//! (CLEANROOM.md: user-visible strings are reproduced verbatim and
//! collected here).

pub fn missing_value(name: &str) -> String {
    format!("missing value for '{name}'")
}

pub fn empty_comment(name: &str) -> String {
    format!("{name} must have at least one character")
}

pub fn newline_in_comment(name: &str) -> String {
    format!("{name} cannot contain newline")
}
