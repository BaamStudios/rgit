// SPDX-License-Identifier: MIT
//! Messages of `git request-pull`.

pub const USAGE: &str = "\
usage: git request-pull [-p] <start> <url> [<end>]

    -p                    include the patch text
";

pub fn not_a_revision(rev: &str) -> String {
    format!("fatal: Not a valid revision: {rev}")
}

pub fn no_common_commits(start: &str, end: &str) -> String {
    format!("fatal: No commits in common between {start} and {end}")
}

pub fn no_match(commit: &str, url: &str) -> String {
    format!("warn: No match for commit {commit} found at {url}")
}

pub fn different_object(reference: &str, url: &str) -> String {
    format!("warn: {reference} found at {url} but points to a different object")
}

pub fn pushed_there(name: &str) -> String {
    format!("warn: Are you sure you pushed '{name}' there?")
}

pub fn since_commit(commit: &str) -> String {
    format!("The following changes since commit {commit}:")
}

pub const AVAILABLE_AT: &str = "are available in the Git repository at:";

pub fn fetch_up_to(commit: &str) -> String {
    format!("for you to fetch changes up to {commit}:")
}

pub fn from_description(branch: &str) -> String {
    format!("(from the branch description for {branch} local branch)")
}

pub const RULE: &str = "----------------------------------------------------------------";
