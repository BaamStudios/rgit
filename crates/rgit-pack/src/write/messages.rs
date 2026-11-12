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

/// rgit: the pack header counts objects in 32 bits.
pub fn too_many_objects(f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "too many objects for one pack")
}

/// rgit: the SHA-1 of the pack hit a collision attack pattern.
pub fn trailer_collision(f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "SHA-1 collision detected while hashing the pack")
}
