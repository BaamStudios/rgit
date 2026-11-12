// SPDX-License-Identifier: MIT
//! User-visible texts of this crate, verbatim as git prints them (without the
//! `fatal: ` / `warning: ` prefix).

use std::path::Path;

pub fn unable_to_append(path: &Path, err: &str) -> String {
    format!("unable to append to '{}': {err}", path.display())
}

pub fn logs_under(path: &Path) -> String {
    format!("there are still logs under '{}'", path.display())
}

/// rgit's own wording (git's text for this rare failure is not known).
pub fn unable_to_rename(from: &Path, to: &Path, err: &str) -> String {
    format!(
        "unable to rename reflog '{}' to '{}': {err}",
        from.display(),
        to.display()
    )
}

pub fn gap_after(refname: &str, date: &str) -> String {
    format!("log for ref {refname} has gap after {date}")
}

pub fn unexpectedly_ended(refname: &str, date: &str) -> String {
    format!("log for ref {refname} unexpectedly ended on {date}")
}

pub fn only_goes_back(display: &str, date: &str) -> String {
    format!("log for '{display}' only goes back to {date}")
}

pub fn only_has_entries(display: &str, count: usize) -> String {
    format!("log for '{display}' only has {count} entries")
}

pub fn log_is_empty(refname: &str) -> String {
    format!("log for {refname} is empty")
}
