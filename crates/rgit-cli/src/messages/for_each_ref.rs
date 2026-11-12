// SPDX-License-Identifier: MIT
//! Messages of `git for-each-ref`, as printed by the reference git.

pub const USAGE: &[&str] = &[
    "git for-each-ref [--count=<count>] [--shell|--perl|--python|--tcl]
                                [(--sort=<key>)...] [--format=<format>]
                                [--include-root-refs] [--points-at=<object>]
                                [--merged[=<object>]] [--no-merged[=<object>]]
                                [--contains[=<object>]] [--no-contains[=<object>]]
                                [(--exclude=<pattern>)...] [--start-after=<marker>]
                                [ --stdin | (<pattern>...)]",
];

pub const HELP_SHELL: &str = "quote placeholders suitably for shells";
pub const HELP_PERL: &str = "quote placeholders suitably for perl";
pub const HELP_PYTHON: &str = "quote placeholders suitably for python";
pub const HELP_TCL: &str = "quote placeholders suitably for Tcl";
pub const HELP_OMIT_EMPTY: &str = "do not output a newline after empty formatted refs";
pub const HELP_COUNT: &str = "show only <n> matched refs";
pub const HELP_FORMAT: &str = "format to use for the output";
pub const HELP_START_AFTER: &str = "start iteration after the provided marker";
pub const HELP_EXCLUDE: &str = "exclude refs which match pattern";
pub const HELP_SORT: &str = "field name to sort on";
pub const HELP_POINTS_AT: &str = "print only refs which points at the given object";
pub const HELP_COLOR: &str = "respect format colors";
pub const HELP_MERGED: &str = "print only refs that are merged";
pub const HELP_NO_MERGED: &str = "print only refs that are not merged";
pub const HELP_CONTAINS: &str = "print only refs which contain the commit";
pub const HELP_NO_CONTAINS: &str = "print only refs which don't contain the commit";
pub const HELP_IGNORE_CASE: &str = "sorting and filtering are case insensitive";
pub const HELP_STDIN: &str = "read reference patterns from stdin";
pub const HELP_INCLUDE_ROOT_REFS: &str = "also include HEAD ref and pseudorefs";

pub const DEFAULT_FORMAT: &str = "%(objectname) %(objecttype)\t%(refname)";

pub const MORE_THAN_ONE_QUOTING: &str = "more than one quoting style?";
pub const STDIN_AND_PATTERNS: &str = "unknown arguments supplied with --stdin";

pub fn unknown_field(name: &str) -> String {
    format!("unknown field name: {name}")
}

pub fn malformed_format(fmt: &str) -> String {
    format!("malformed format string {fmt}")
}

pub fn unrecognized_arg(atom: &str, arg: &str) -> String {
    format!("unrecognized %({atom}) argument: {arg}")
}

pub fn no_arguments(atom: &str) -> String {
    format!("%({atom}) does not take arguments")
}

pub fn invalid_count(arg: &str) -> String {
    format!("invalid --count argument: `{arg}'")
}

pub fn missing_object(oid: &str, name: &str) -> String {
    format!("missing object {oid} for {name}")
}

pub fn malformed_object_name(name: &str) -> String {
    format!("malformed object name {name}")
}

pub fn ignoring_broken_ref(name: &str) -> String {
    format!("ignoring broken ref {name}")
}

/// rgit wording: an option that is not implemented yet.
pub fn not_supported(opt: &str) -> String {
    format!("rgit for-each-ref: {opt} is not supported yet")
}
