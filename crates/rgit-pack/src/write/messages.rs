// SPDX-License-Identifier: MIT

//! User-visible message texts of the pack writer. "git" marks wording
//! observed from the git binary, "rgit" our own.

use std::fmt;
use std::io;

use rgit_hash::ObjectId;

/// rgit: writing the pack failed.
pub fn write_failed(f: &mut fmt::Formatter<'_>, e: &io::Error) -> fmt::Result {
    write!(f, "failed to write pack: {e}")
}

/// git (`pack-objects` with an object list naming a missing object).
pub fn unable_to_read(f: &mut fmt::Formatter<'_>, id: &ObjectId) -> fmt::Result {
    write!(f, "unable to read {id}")
}

/// rgit: an object exists but cannot be read.
pub fn read_failed(f: &mut fmt::Formatter<'_>, id: &ObjectId, detail: &str) -> fmt::Result {
    write!(f, "unable to read {id}: {detail}")
}

/// git: an object has an unexpected type.
pub fn wrong_object_type(
    f: &mut fmt::Formatter<'_>,
    id: &ObjectId,
    found: &str,
    expected: &str,
) -> fmt::Result {
    write!(f, "object {id} is a {found}, not a {expected}")
}

/// rgit: a commit, tree or tag cannot be parsed.
pub fn bad_object_of_kind(f: &mut fmt::Formatter<'_>, id: &ObjectId, kind: &str) -> fmt::Result {
    write!(f, "bad {kind} object {id}")
}

/// rgit: an object's content does not match its id.
pub fn hash_mismatch(f: &mut fmt::Formatter<'_>, id: &ObjectId) -> fmt::Result {
    write!(
        f,
        "object {id} is corrupt: its content does not match its id"
    )
}

/// rgit: the pack header counts objects in 32 bits.
pub fn too_many_objects(f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "too many objects for one pack")
}

/// rgit: the SHA-1 of the pack hit a collision attack pattern.
pub fn trailer_collision(f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "SHA-1 collision detected while hashing the pack")
}

/// git: progress meter over the delta search.
pub const PROGRESS_COMPRESSING: &str = "Compressing objects";
/// git: progress meter while writing the pack.
pub const PROGRESS_WRITING: &str = "Writing objects";

/// git: before the delta search when it may use several threads.
pub fn delta_threads(n: usize) -> String {
    format!("Delta compression using up to {n} threads")
}

/// git: a filter spec that is none.
pub fn invalid_filter_spec(spec: &str) -> String {
    format!("invalid filter-spec '{spec}'")
}

/// git: `tree:` without a depth.
pub const EXPECTED_TREE_DEPTH: &str = "expected 'tree:<depth>'";

/// git: `object:type=` with an unknown type.
pub fn invalid_filter_object_type(name: &str) -> String {
    format!("'{name}' for 'object:type=<type>' is not a valid object type")
}

/// git: `sparse:path=` filters.
pub const SPARSE_PATH_DROPPED: &str = "sparse:path filters support has been dropped";

/// git: `combine:` without filters.
pub const EXPECTED_AFTER_COMBINE: &str = "expected something after combine:";

/// git: a reserved character in a filter inside `combine:`.
pub fn must_escape_char(c: char) -> String {
    format!("must escape char in sub-filter-spec: '{c}'")
}

/// git (upload-pack's wording for a filter it does not serve).
pub fn filter_not_supported(spec: &str) -> String {
    format!("filter '{spec}' not supported")
}
