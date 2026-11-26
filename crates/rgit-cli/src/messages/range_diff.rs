// SPDX-License-Identifier: MIT
//! Messages and option help of `git range-diff` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE_HEAD: &str = "\
usage: git range-diff [<options>] <old-base>..<old-tip> <new-base>..<new-tip>
   or: git range-diff [<options>] <old-tip>...<new-tip>
   or: git range-diff [<options>] <base> <old-tip> <new-tip>

    --[no-]creation-factor <n>
                          percentage by which creation is weighted
    --no-dual-color       use simple diff colors
    --dual-color          opposite of --no-dual-color
    --[no-]notes[=<notes>]
                          passed to 'git log'
    --[no-]diff-merges <style>
                          passed to 'git log'
    --[no-]max-memory <size>
                          maximum memory for cost matrix (default 4G)
    --[no-]remerge-diff   passed to 'git log'
    --[no-]left-only      only emit output related to the first range
    --[no-]right-only     only emit output related to the second range

";

/// The full usage: the options of `range-diff`, then the diff options.
pub fn usage() -> String {
    format!("{USAGE_HEAD}{}", super::diff::OPTIONS_GROUP)
}

pub const NEED_TWO_RANGES: &str = "need two commit ranges";

pub fn not_a_commit_range(arg: &str) -> String {
    format!("not a commit range: '{arg}'")
}

pub fn not_a_symmetric_range(arg: &str) -> String {
    format!("not a symmetric range: '{arg}'")
}

pub fn not_a_revision(arg: &str) -> String {
    format!("not a revision: '{arg}'")
}

pub fn bad_revision(arg: &str) -> String {
    format!("bad revision '{arg}'")
}

pub fn could_not_parse_log(range: &str) -> String {
    format!("could not parse log for '{range}'")
}

pub fn cannot_be_used_together(a: &str, b: &str) -> String {
    format!("options '{a}' and '{b}' cannot be used together")
}

pub fn expects_integer(option: &str) -> String {
    format!("option `{option}' expects an integer value with an optional k/m/g suffix")
}

/// The cost matrix would need `needed` bytes, more than `limit`.
pub fn exceeds_max_memory(needed: u64, limit: u64) -> String {
    format!(
        "range-diff: unable to compute the range-diff, since it exceeds the maximum memory for the cost matrix: {} ({needed} bytes) needed, limited to {} ({limit} bytes)",
        rgit_progress::messages::bytes(needed),
        rgit_progress::messages::bytes(limit)
    )
}
