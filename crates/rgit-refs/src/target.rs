// SPDX-License-Identifier: MIT
//! Values stored in references.

use rgit_hash::ObjectId;

/// What a reference points to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// A direct reference to an object.
    Object(ObjectId),
    /// A symbolic reference naming another reference (`ref: <name>`).
    Symbolic(String),
}

impl Target {
    /// The object id of a direct reference.
    pub fn as_object(&self) -> Option<&ObjectId> {
        match self {
            Target::Object(id) => Some(id),
            Target::Symbolic(_) => None,
        }
    }

    /// The referent of a symbolic reference.
    pub fn as_symbolic(&self) -> Option<&str> {
        match self {
            Target::Object(_) => None,
            Target::Symbolic(name) => Some(name),
        }
    }
}

/// What is known about the peeled value of a reference (the object reached
/// by dereferencing annotated tags until a non-tag object is found).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Peeled {
    /// The store does not know; the caller has to read the object.
    #[default]
    Unknown,
    /// The reference does not point at a tag; it peels to itself.
    NotTag,
    /// The reference points at a tag that peels to this object.
    Tag(ObjectId),
}

/// Where a reference value was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A loose file below `$GIT_DIR`.
    Loose,
    /// An entry of `packed-refs`.
    Packed,
}

/// One reference as returned by iteration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    /// Full name, e.g. `refs/heads/main`.
    pub name: String,
    /// The stored value.
    pub target: Target,
    /// The object the reference finally resolves to; `None` for a dangling
    /// or too deeply nested symbolic reference.
    pub oid: Option<ObjectId>,
    /// Peeled value, as far as the store knows it.
    pub peeled: Peeled,
    /// Where the value came from.
    pub source: Source,
}

/// Result of resolving a reference through its symbolic links.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    /// The name that was looked up followed by every referent, in order.
    /// The last element is the reference that holds `oid` (or would hold
    /// it, for an unborn branch).
    pub chain: Vec<String>,
    /// The object id at the end of the chain, `None` when the last
    /// reference does not exist (dangling symbolic reference).
    pub oid: Option<ObjectId>,
}

impl Resolved {
    /// The name of the reference that holds (or would hold) the value.
    pub fn final_name(&self) -> &str {
        self.chain.last().map(String::as_str).unwrap_or("")
    }

    /// Whether the looked-up name was a symbolic reference.
    pub fn is_symbolic(&self) -> bool {
        self.chain.len() > 1
    }
}
