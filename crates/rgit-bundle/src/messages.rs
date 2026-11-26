// SPDX-License-Identifier: MIT
//! User-visible messages about bundles, reproduced verbatim from the output
//! of git 2.56 because scripts and tests compare them. Without the
//! `fatal: ` / `error: ` / `warning: ` prefix.

/// `error:` for a file whose first line is no bundle signature.
pub fn not_a_bundle(path: &str) -> String {
    format!("'{path}' does not look like a v2 or v3 bundle file")
}

/// `error:` for a v3 capability that is not known.
pub fn unknown_capability(cap: &str) -> String {
    format!("unknown capability '{cap}'")
}

/// `error:` for an `@object-format` value that is not a hash name.
pub fn unrecognized_hash(name: &str) -> String {
    format!("unrecognized bundle hash algorithm: {name}")
}

/// `error:` for a header line that is neither prerequisite nor reference.
pub fn unrecognized_header(line: &str) -> String {
    format!("unrecognized header: {line} ({})", line.len())
}

/// `fatal:` for a filter spec that cannot be parsed.
pub fn invalid_filter_spec(spec: &str) -> String {
    format!("invalid filter-spec '{spec}'")
}

/// `fatal:` for `tree:<depth>` with a bad depth.
pub const EXPECTED_TREE_DEPTH: &str = "expected 'tree:<depth>'";

/// `fatal:` for `sparse:path=...`.
pub const SPARSE_PATH_DROPPED: &str = "sparse:path filters support has been dropped";

/// `fatal:` for `object:type=` with an unknown type.
pub fn invalid_object_type(t: &str) -> String {
    format!("'{t}' for 'object:type=<type>' is not a valid object type")
}

/// `error:` when a bundle list line has no key or no value.
pub const EMPTY_KEY_OR_VALUE: &str = "bundle-uri: line has empty key or value";

/// `error:` for an empty bundle list line.
pub const EMPTY_LINE: &str = "bundle-uri: got an empty line";

/// `error:` after a bundle list line that could not be used.
pub fn bad_line(line: &str) -> String {
    format!("bad line: '{line}'")
}

/// `warning:` for a `creationToken` that is not a number.
pub fn bad_creation_token(value: &str) -> String {
    format!("could not parse bundle list key creationToken with value '{value}'")
}

/// `error:` for a bundle of a list without `uri`.
pub fn bundle_has_no_uri(id: &str) -> String {
    format!("bundle '{id}' has no uri")
}

/// `fatal:` when a relative URI goes above the root of its base.
pub fn cannot_strip_component(url: &str) -> String {
    format!("cannot strip one component off url '{url}'")
}
