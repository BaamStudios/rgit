// SPDX-License-Identifier: MIT
//! Usage and option help of `git stripspace` (CLEANROOM.md: user-visible
//! strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &[
    "git stripspace [-s | --strip-comments]",
    "git stripspace [-c | --comment-lines]",
];
pub const HELP_STRIP: &str = "skip and remove all lines starting with comment character";
pub const HELP_COMMENT: &str = "prepend comment character and space to each line";
