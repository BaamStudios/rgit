// SPDX-License-Identifier: MIT
//! Messages, usage lines and option help of `git mktag` (CLEANROOM.md: user-visible
//! strings are reproduced verbatim and collected here).

pub use super::objects::*;

pub const MKTAG_USAGE: &[&str] = &["git mktag"];
pub const MKTAG_HELP_STRICT: &str = "enable more strict checking";
pub const MKTAG_FAILED_FSCK: &str = "tag on stdin did not pass our strict fsck check";

pub fn tag_fails_fsck(finding: &str) -> String {
    format!("tag input does not pass fsck: {finding}")
}

pub fn could_not_read_tagged(oid: &str) -> String {
    format!("could not read tagged object '{oid}'")
}

pub fn tagged_type_mismatch(oid: &str, tagged: &str, actual: &str) -> String {
    format!("object '{oid}' tagged as '{tagged}', but is a '{actual}' type")
}

pub fn could_not_open_skiplist(path: &str) -> String {
    format!("could not open object name list: {path}")
}

pub fn missing_config_value(name: &str) -> String {
    format!("missing value for '{name}'")
}

pub fn unable_to_parse_config(name: &str) -> String {
    format!("unable to parse '{name}' from command-line config")
}
