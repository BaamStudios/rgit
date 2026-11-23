// SPDX-License-Identifier: MIT
//! Usage, option help and messages of `git fsck` (CLEANROOM.md: user-visible
//! strings are reproduced verbatim from the git binary's output and
//! collected here).

pub const USAGE: &[&str] = &[
    "git fsck [--tags] [--root] [--unreachable] [--cache] [--no-reflogs]\n         [--[no-]full] [--strict] [--verbose] [--lost-found]\n         [--[no-]dangling] [--[no-]progress] [--connectivity-only]\n         [--[no-]name-objects] [--[no-]references] [<object>...]",
];
pub const H_VERBOSE: &str = "be verbose";
pub const H_UNREACHABLE: &str = "show unreachable objects";
pub const H_DANGLING: &str = "show dangling objects";
pub const H_TAGS: &str = "report tags";
pub const H_ROOT: &str = "report root nodes";
pub const H_CACHE: &str = "make index objects head nodes";
pub const H_REFLOGS: &str = "make reflogs head nodes (default)";
pub const H_FULL: &str = "also consider packs and alternate objects";
pub const H_CONNECTIVITY: &str = "check only connectivity";
pub const H_STRICT: &str = "enable more strict checking";
pub const H_LOST_FOUND: &str = "write dangling objects in .git/lost-found";
pub const H_PROGRESS: &str = "show progress";
pub const H_NAME_OBJECTS: &str = "show verbose names for reachable objects";
pub const H_REFERENCES: &str = "check reference database consistency";

pub const NO_DEFAULT_REFERENCES: &str = "notice: No default references";

pub fn finding(level: &str, kind: &str, oid: &str, text: &str) -> String {
    format!("{level} in {kind} {oid}: {text}")
}

pub fn broken_links(kind: &str, oid: &str) -> String {
    format!("error in {kind} {oid}: broken links")
}

pub fn invalid_parameter(arg: &str) -> String {
    format!("invalid parameter: expected sha1, got '{arg}'")
}

pub fn invalid_sha1_pointer(name: &str, oid: &str) -> String {
    format!("{name}: invalid sha1 pointer {oid}")
}

pub fn invalid_reflog_entry(name: &str, oid: &str) -> String {
    format!("{name}: invalid reflog entry {oid}")
}

pub fn not_a_commit(name: &str) -> String {
    format!("{name}: not a commit")
}

pub fn invalid_cache_tree(oid: &str, index: &str) -> String {
    format!("{oid}: invalid sha1 pointer in cache-tree of {index}")
}

pub fn object_corrupt(oid: &str, path: &str) -> String {
    format!("{oid}: object corrupt or missing: {path}")
}

pub fn unable_to_unpack_header(path: &str) -> String {
    format!("unable to unpack header of {path}")
}

pub fn unable_to_unpack_contents(path: &str) -> String {
    format!("unable to unpack contents of {path}")
}

pub fn unable_to_parse_type(found: &str, path: &str) -> String {
    format!("unable to parse type from header '{found}' of {path}")
}

pub fn missing(kind: &str, oid: &str) -> String {
    format!("missing {kind} {oid}")
}

pub fn broken_link(from_kind: &str, from: &str, to_kind: &str, to: &str) -> String {
    format!("broken link from {from_kind:>7} {from}\n              to {to_kind:>7} {to}")
}

pub fn dangling(kind: &str, oid: &str) -> String {
    format!("dangling {kind} {oid}")
}

pub fn unreachable(kind: &str, oid: &str) -> String {
    format!("unreachable {kind} {oid}")
}

pub fn root(oid: &str) -> String {
    format!("root {oid}")
}

pub fn tagged(kind: &str, target: &str, name: &str, tag: &str) -> String {
    format!("tagged {kind} {target} ({name}) in {tag}")
}

pub fn checking(what: &str) -> String {
    format!("Checking {what}")
}

pub fn checking_connectivity(n: usize) -> String {
    format!("Checking connectivity ({n} objects)")
}

pub const CHECKING_OBJECT_DIRECTORY: &str = "Checking object directory";
pub const CHECKING_OBJECT_DIRECTORIES: &str = "Checking object directories";
/// git: progress meter over the packed objects.
pub const CHECKING_OBJECTS: &str = "Checking objects";
pub const CHECKING_REFS_CONSISTENCY: &str = "Checking references consistency";
pub const CHECKING_REF_DATABASE: &str = "Checking ref database";

pub fn could_not_create_lost_found(path: &str) -> String {
    format!("could not create lost-found file {path}")
}

pub const GITATTRIBUTES_LINE_LENGTH: &str = ".gitattributes has too long lines to parse";
pub const GITATTRIBUTES_LARGE: &str = ".gitattributes too large to parse";

pub fn bad_ref_oid(oid: &str) -> String {
    format!("badRefOid: points to invalid object ID '{oid}'")
}

pub fn bad_head_target(target: &str) -> String {
    format!("badHeadTarget: HEAD points to non-branch '{target}'")
}

pub fn bad_ref_content(content: &str) -> String {
    format!("badRefContent: {content}")
}

pub const REF_MISSING_NEWLINE: &str = "refMissingNewline: misses LF at the end";

pub fn trailing_ref_content(rest: &str) -> String {
    format!("trailingRefContent: has trailing garbage: '{rest}'")
}

pub fn ref_finding(level: &str, name: &str, text: &str) -> String {
    format!("{level}: {name}: {text}")
}

pub fn packed_hash_mismatch(oid: &str, pack: &str, offset: u64) -> String {
    format!("{oid}: hash mismatch at offset {offset} in {pack}")
}

pub fn could_not_open_skiplist(path: &str) -> String {
    format!("could not open object name list: {path}")
}

pub fn invalid_skiplist_line(line: &str) -> String {
    format!("invalid object name: {line}")
}

pub fn missing_config_value(name: &str) -> String {
    format!("missing value for '{name}'")
}

pub fn unable_to_parse_config(name: &str) -> String {
    format!("unable to parse '{name}' from command-line config")
}

pub fn unhandled_fsck_id(id: &str) -> String {
    format!("Unhandled message id: {id}")
}

pub fn unknown_severity(value: &str) -> String {
    format!("Unknown fsck message type: '{value}'")
}

pub fn cannot_demote(id: &str, to: &str) -> String {
    format!("Cannot demote {id} to {to}")
}

pub fn type_mismatch(oid: &str, is: &str, not: &str) -> String {
    format!("object {oid} is a {is}, not a {not}")
}

pub const GITATTRIBUTES_BLOB: &str = "non-blob found at .gitattributes";
pub const GITMODULES_BLOB: &str = "non-blob found at .gitmodules";
pub const GITMODULES_PARSE: &str = "could not parse gitmodules blob";

pub fn disallowed_name(name: &str) -> String {
    format!("disallowed submodule name: {name}")
}

pub fn disallowed_url(url: &str) -> String {
    format!("disallowed submodule url: {url}")
}

pub fn disallowed_path(path: &str) -> String {
    format!("disallowed submodule path: {path}")
}

pub fn disallowed_update(value: &str) -> String {
    format!("disallowed submodule update setting: {value}")
}
