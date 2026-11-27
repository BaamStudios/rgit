// SPDX-License-Identifier: MIT
//! Messages of the file system monitor client and of `update-index
//! --fsmonitor` (CLEANROOM.md: user-visible strings are reproduced
//! verbatim from the `git` binary and collected here).

pub const EMPTY_TOKEN: &str = "Empty last update token.";
pub const UNSET_WARNING: &str =
    "core.fsmonitor is unset; set it if you really want to enable fsmonitor";
pub const SET_WARNING: &str =
    "core.fsmonitor is set; remove it if you really want to disable fsmonitor";

pub fn invalid_hook_version(v: i64) -> String {
    format!("Invalid hook version '{v}' in core.fsmonitorhookversion. Must be 1 or 2.")
}

pub fn cannot_exec(cmd: &str, err: &str) -> String {
    format!("cannot exec '{cmd}': {err}")
}

pub fn bare_incompatible(dir: &str) -> String {
    format!("bare repository '{dir}' is incompatible with fsmonitor")
}
