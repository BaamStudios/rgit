// SPDX-License-Identifier: MIT
//! Usage, option help and messages of `git mailsplit` and `git mailinfo`
//! (CLEANROOM.md: user-visible strings are reproduced verbatim and
//! collected here).

pub const MAILSPLIT_USAGE: &str = "usage: git mailsplit [-d<prec>] [-f<n>] [-b] [--keep-cr] -o<directory> [(<mbox>|<Maildir>)...]\n";

pub const CORRUPT_MAILBOX: &str = "corrupt mailbox";

pub fn unknown_option(arg: &str) -> String {
    format!("unknown option: {arg}")
}

pub fn cannot_stat(path: &str, reason: &str) -> String {
    format!("cannot stat {path}: {reason}")
}

pub fn empty_mbox(path: &str) -> String {
    format!("empty mbox: '{path}'")
}

pub fn cannot_split(path: &str) -> String {
    format!("cannot split patches from {path}")
}

pub fn cannot_open_mail(path: &str, reason: &str) -> String {
    format!("cannot open mail {path}: {reason}")
}

pub fn cannot_read_mail(path: &str, reason: &str) -> String {
    format!("cannot read mail {path}: {reason}")
}

pub fn unable_to_create(path: &str, reason: &str) -> String {
    format!("unable to create '{path}': {reason}")
}

pub fn cannot_read(path: &str, reason: &str) -> String {
    format!("could not read '{path}': {reason}")
}

pub const MAILINFO_USAGE: &[&str] = &["git mailinfo [<options>] <msg> <patch> < mail >info"];
pub const HELP_K: &str = "keep subject";
pub const HELP_B: &str = "keep non patch brackets in subject";
pub const HELP_M: &str = "copy Message-ID to the end of commit message";
pub const HELP_U: &str = "re-code metadata to i18n.commitEncoding";
pub const HELP_N: &str = "disable charset re-coding of metadata";
pub const HELP_ENCODING: &str = "re-code metadata to this encoding";
pub const HELP_SCISSORS: &str = "use scissors";
pub const HELP_QUOTED_CR: &str = "action when quoted CR is found";
pub const HELP_INBODY: &str = "use headers in message's body";

pub fn bad_quoted_cr(action: &str, option: &str) -> String {
    format!("bad action '{action}' for '{option}'")
}

pub fn empty_patch(path: &str) -> String {
    format!("empty patch: '{path}'")
}

pub fn no_such_file(path: &str, reason: &str) -> String {
    format!("{path}: {reason}")
}
