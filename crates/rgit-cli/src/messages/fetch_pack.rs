// SPDX-License-Identifier: MIT
//! Usage and messages of `git fetch-pack` (CLEANROOM.md: user-visible
//! strings are reproduced verbatim and collected here).

pub const USAGE: &str = "usage: git fetch-pack [--all] [--stdin] [--quiet | -q] [--keep | -k] [--thin] [--include-tag] [--upload-pack=<git-upload-pack>] [--depth=<n>] [--no-progress] [--diag-url] [-v] [<host>:]<directory> [<refs>...]";

pub fn no_such_remote_ref(name: &str) -> String {
    format!("no such remote ref {name}")
}

pub fn server_version(agent: &str) -> String {
    format!("Server version is {agent}")
}

pub fn server_supports(cap: &str) -> String {
    format!("Server supports {cap}")
}

pub fn want(oid: &str, name: &str) -> String {
    format!("want {oid} ({name})")
}

pub fn already_have(oid: &str, name: &str) -> String {
    format!("already have {oid} ({name})")
}

pub fn marking_complete(oid: &str) -> String {
    format!("Marking {oid} as complete")
}
