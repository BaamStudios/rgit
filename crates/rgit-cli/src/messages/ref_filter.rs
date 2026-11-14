// SPDX-License-Identifier: MIT
//! Messages of the reference format language (`for-each-ref`, `tag` and
//! `branch` listings), as printed by the reference git.

pub fn unknown_field(name: &str) -> String {
    format!("unknown field name: {name}")
}

pub fn malformed_field(name: &str) -> String {
    format!("malformed field name: {name}")
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

pub fn integer_expected(arg: &str) -> String {
    format!("Integer value expected refname:{arg}")
}

pub fn positive_in(value: &str, atom: &str) -> String {
    format!("positive value expected '{value}' in %({atom})")
}

pub fn positive_lines(arg: &str) -> String {
    format!("positive value expected contents:lines={arg}")
}

pub const ALIGN_EXPECTED: &str = "expected format: %(align:<width>,<position>)";
pub const ALIGN_WIDTH: &str = "positive width expected with the %(align) atom";

pub fn unrecognized_position(p: &str) -> String {
    format!("unrecognized position:{p}")
}

pub fn unrecognized_width(w: &str) -> String {
    format!("unrecognized width:{w}")
}

pub const COLOR_EXPECTED: &str = "expected format: %(color:<color>)";

pub fn unrecognized_color(c: &str) -> String {
    format!("unrecognized color: %(color:{c})")
}

pub fn unknown_trailers_arg(arg: &str) -> String {
    format!("unknown %(trailers) argument: {arg}")
}

pub const TRAILERS_KEY: &str = "expected %(trailers:key=<value>)";

pub fn expected_committish(atom: &str) -> String {
    format!("expected format: %({atom}:<committish>)")
}

pub fn failed_to_find(name: &str) -> String {
    format!("failed to find '{name}'")
}

pub fn rejected_atom(atom: &str) -> String {
    format!("this command reject atom %({atom})")
}

pub const RAW_QUOTED: &str = "--format=raw cannot be used with --python, --shell, --tcl";

pub const END_MISSING: &str = "format: %(end) atom missing";
pub const END_WITHOUT: &str = "format: %(end) atom used without corresponding atom";
pub const THEN_WITHOUT_IF: &str = "format: %(then) atom used without a %(if) atom";
pub const THEN_TWICE: &str = "format: %(then) atom used more than once";
pub const ELSE_WITHOUT_IF: &str = "format: %(else) atom used without a %(if) atom";
pub const ELSE_WITHOUT_THEN: &str = "format: %(else) atom used without a %(then) atom";
pub const ELSE_TWICE: &str = "format: %(else) atom used more than once";
pub const IF_WITHOUT_THEN: &str = "format: %(if) atom used without a %(then) atom";

pub fn missing_object(oid: &str, name: &str) -> String {
    format!("missing object {oid} for {name}")
}

pub fn not_a_commit(oid: &str, kind: &str) -> String {
    format!("object {oid} is a {kind}, not a commit")
}

pub const BAD_TAG: &str = "bad tag";

pub fn malformed_object_name(name: &str) -> String {
    format!("malformed object name {name}")
}

pub fn malformed_object_name_quoted(name: &str) -> String {
    format!("malformed object name '{name}'")
}

pub fn no_such_commit(name: &str) -> String {
    format!("no such commit {name}")
}

pub fn must_point_to_commit(opt: &str) -> String {
    format!("option `{opt}' must point to a commit")
}

pub fn missing_value(key: &str) -> String {
    format!("missing value for '{key}'")
}

pub const GONE: &str = "gone";

pub fn ahead(n: usize) -> String {
    format!("ahead {n}")
}

pub fn behind(n: usize) -> String {
    format!("behind {n}")
}

pub fn ahead_behind(a: usize, b: usize) -> String {
    format!("ahead {a}, behind {b}")
}

/// rgit wording: an atom that is not implemented yet.
pub fn atom_not_supported(atom: &str) -> String {
    format!("rgit: %({atom}) is not supported yet")
}

pub fn could_not_read(oid: &str) -> String {
    format!("Could not read {oid}")
}

pub fn could_not_parse_commit(oid: &str) -> String {
    format!("could not parse commit {oid}")
}

pub const REACHABILITY_FAILED: &str = "failed to check reachability";
