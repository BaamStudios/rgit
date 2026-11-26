// SPDX-License-Identifier: MIT
//! Messages and usage of `git repo` (CLEANROOM.md: user-visible strings are
//! reproduced verbatim and collected here).

pub const USAGE_INFO: &str = "git repo info [--format=(lines|nul) | -z] [--all | <key>...]";
pub const USAGE_KEYS: &str = "git repo info --keys [--format=(lines|nul) | -z]";
pub const USAGE_STRUCTURE: &str = "git repo structure [--format=(table|lines|nul) | -z]";

pub const H_FORMAT: &str = "output format";
pub const H_Z: &str = "synonym for --format=nul";
pub const H_ALL: &str = "print all keys/values";
pub const H_KEYS: &str = "show keys";
pub const H_PROGRESS: &str = "show progress";

pub const UNSUPPORTED_FORMAT: &str = "unsupported output format";
pub const ALL_AND_KEY: &str = "--all and <key> cannot be used together";
pub const KEYS_WITH_KEY: &str = "--keys cannot be used with a <key> or --all";
pub const KEYS_FORMAT: &str = "--keys can only be used with --format=lines or --format=nul";
pub const TOO_MANY_ARGUMENTS: &str = "too many arguments";

pub const COUNTING_REFERENCES: &str = "Counting references";
pub const COUNTING_OBJECTS: &str = "Counting objects";

pub fn invalid_format(f: &str) -> String {
    format!("invalid format '{f}'")
}

pub fn key_not_found(k: &str) -> String {
    format!("key '{k}' not found")
}

pub const TABLE_HEADER: &str = "Repository structure";
pub const TABLE_VALUE: &str = "Value";
pub const REFERENCES: &str = "References";
pub const COUNT: &str = "Count";
pub const BRANCHES: &str = "Branches";
pub const TAGS: &str = "Tags";
pub const REMOTES: &str = "Remotes";
pub const OTHERS: &str = "Others";
pub const REACHABLE_OBJECTS: &str = "Reachable objects";
pub const COMMITS: &str = "Commits";
pub const TREES: &str = "Trees";
pub const BLOBS: &str = "Blobs";
pub const INFLATED_SIZE: &str = "Inflated size";
pub const DISK_SIZE: &str = "Disk size";
pub const LARGEST_OBJECTS: &str = "Largest objects";
pub const MAXIMUM_SIZE: &str = "Maximum size";
pub const MAXIMUM_PARENTS: &str = "Maximum parents";
pub const MAXIMUM_ENTRIES: &str = "Maximum entries";
