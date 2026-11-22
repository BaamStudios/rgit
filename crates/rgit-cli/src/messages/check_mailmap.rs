// SPDX-License-Identifier: MIT
//! Messages and usage of `git check-mailmap` (CLEANROOM.md: user-visible
//! strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &["git check-mailmap [<options>] <contact>..."];
pub const HELP_STDIN: &str = "also read contacts from stdin";
pub const HELP_FILE: &str = "read additional mailmap entries from file";
pub const HELP_BLOB: &str = "read additional mailmap entries from blob";
pub const NO_CONTACTS: &str = "no contacts specified";
