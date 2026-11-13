// SPDX-License-Identifier: MIT
//! Usage, option help and messages of `git interpret-trailers`
//! (CLEANROOM.md: user-visible strings are reproduced verbatim and
//! collected here).

pub const USAGE: &[&str] = &["git interpret-trailers [--in-place] [--trim-empty]\n\
     [(--trailer (<key>|<key-alias>)[(=|:)<value>])...]\n\
     [--parse] [<file>...]"];
pub const HELP_IN_PLACE: &str = "edit files in place";
pub const HELP_TRIM_EMPTY: &str = "trim empty trailers";
pub const HELP_WHERE: &str = "where to place the new trailer";
pub const HELP_IF_EXISTS: &str = "action if trailer already exists";
pub const HELP_IF_MISSING: &str = "action if trailer is missing";
pub const HELP_ONLY_TRAILERS: &str = "output only the trailers";
pub const HELP_ONLY_INPUT: &str = "do not apply trailer.<key-alias> configuration variables";
pub const HELP_UNFOLD: &str = "reformat multiline trailer values as single-line values";
pub const HELP_PARSE: &str = "alias for --only-trailers --only-input --unfold";
pub const HELP_NO_DIVIDER: &str = "do not treat \"---\" as the end of input";
pub const HELP_TRAILER: &str = "trailer(s) to add";
pub const TRAILER_WITH_ONLY_INPUT: &str = "--trailer with --only-input does not make sense";
pub const NO_INPUT_FILE: &str = "no input file given for in-place editing";

pub fn could_not_read(file: &str, reason: &str) -> String {
    format!("could not read input file '{file}': {reason}")
}

pub fn could_not_write(file: &str, reason: &str) -> String {
    format!("could not write to temporary file '{file}': {reason}")
}

pub fn could_not_rename(from: &str, to: &str, reason: &str) -> String {
    format!("could not rename temporary file to {to} from {from}: {reason}")
}
