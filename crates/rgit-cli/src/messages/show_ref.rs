// SPDX-License-Identifier: MIT
//! Messages of `git show-ref`, as printed by the reference git.

pub const USAGE: &[&str] = &[
    "git show-ref [--head] [-d | --dereference]
                    [-s | --hash[=<n>]] [--abbrev[=<n>]] [--branches] [--tags]
                    [--] [<pattern>...]",
    "git show-ref --verify [-q | --quiet] [-d | --dereference]
                    [-s | --hash[=<n>]] [--abbrev[=<n>]]
                    [--] [<ref>...]",
    "git show-ref --exclude-existing[=<pattern>]",
    "git show-ref --exists <ref>",
];

pub const HELP_TAGS: &str = "only show tags (can be combined with --branches)";
pub const HELP_BRANCHES: &str = "only show branches (can be combined with --tags)";
pub const HELP_EXISTS: &str = "check for reference existence without resolving";
pub const HELP_VERIFY: &str = "stricter reference checking, requires exact ref path";
pub const HELP_HEAD: &str = "show the HEAD reference, even if it would be filtered out";
pub const HELP_DEREFERENCE: &str = "dereference tags into object IDs";
pub const HELP_HASH: &str = "only show SHA1 hash using <n> digits";
pub const HELP_ABBREV: &str = "use <n> digits to display object names";
pub const HELP_QUIET: &str = "do not print results to stdout (useful with --verify)";
pub const HELP_EXCLUDE_EXISTING: &str = "show refs from stdin that aren't in local repository";

pub const VERIFY_REQUIRES_REF: &str = "--verify requires a reference";
pub const EXISTS_REQUIRES_REF: &str = "--exists requires a reference";
pub const EXISTS_ONE_REF: &str = "--exists requires exactly one reference";
pub const DOES_NOT_EXIST: &str = "reference does not exist";

pub fn bad_ref(name: &str, oid: &str) -> String {
    format!("git show-ref: bad ref {name} ({oid})")
}

pub fn not_a_valid_ref(name: &str) -> String {
    format!("'{name}' - not a valid ref")
}

pub fn failed_lookup(reason: &str) -> String {
    format!("failed to look up reference: {reason}")
}

pub fn bad_ref_format(line: &str) -> String {
    format!("warning: ref '{line}' ignored")
}

pub fn expects_number(opt: &str) -> String {
    format!("option `{opt}' expects a numerical value")
}

pub fn incompatible(a: &str, b: &str) -> String {
    format!("options '{a}' and '{b}' cannot be used together")
}
