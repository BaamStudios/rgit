// SPDX-License-Identifier: MIT

//! The four object types.

use std::fmt;
use std::str::FromStr;

use crate::Error;

/// The type of an object, as named in its header and in tag `type` lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ObjectKind {
    /// File contents.
    Blob,
    /// A directory listing.
    Tree,
    /// A snapshot with history.
    Commit,
    /// An annotated tag.
    Tag,
}

impl ObjectKind {
    /// All kinds, in `Ord` order.
    pub const ALL: [ObjectKind; 4] = [
        ObjectKind::Blob,
        ObjectKind::Tree,
        ObjectKind::Commit,
        ObjectKind::Tag,
    ];

    /// The name used in object headers: `"blob"`, `"tree"`, `"commit"`, `"tag"`.
    pub const fn name(self) -> &'static str {
        match self {
            ObjectKind::Blob => "blob",
            ObjectKind::Tree => "tree",
            ObjectKind::Commit => "commit",
            ObjectKind::Tag => "tag",
        }
    }

    /// Same as [`ObjectKind::name`].
    pub const fn as_str(self) -> &'static str {
        self.name()
    }

    /// Parse a type name (exact, case-sensitive match).
    pub fn from_name(name: &[u8]) -> Option<ObjectKind> {
        match name {
            b"blob" => Some(ObjectKind::Blob),
            b"tree" => Some(ObjectKind::Tree),
            b"commit" => Some(ObjectKind::Commit),
            b"tag" => Some(ObjectKind::Tag),
            _ => None,
        }
    }

    /// The numeric type used in pack files (commit 1, tree 2, blob 3, tag 4).
    pub const fn pack_type(self) -> u8 {
        match self {
            ObjectKind::Commit => 1,
            ObjectKind::Tree => 2,
            ObjectKind::Blob => 3,
            ObjectKind::Tag => 4,
        }
    }

    /// The inverse of [`ObjectKind::pack_type`]; delta types (6, 7) and
    /// reserved values give `None`.
    pub const fn from_pack_type(t: u8) -> Option<ObjectKind> {
        match t {
            1 => Some(ObjectKind::Commit),
            2 => Some(ObjectKind::Tree),
            3 => Some(ObjectKind::Blob),
            4 => Some(ObjectKind::Tag),
            _ => None,
        }
    }
}

impl fmt::Display for ObjectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.name())
    }
}

impl FromStr for ObjectKind {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ObjectKind::from_name(s.as_bytes()).ok_or_else(|| Error::UnknownKind(s.as_bytes().to_vec()))
    }
}
