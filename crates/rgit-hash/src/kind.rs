// SPDX-License-Identifier: MIT

//! The hash algorithm used to name objects.

use std::fmt;
use std::str::FromStr;

use crate::{Error, ObjectId};

/// The hash algorithm ("object format") of a repository.
///
/// The `Ord` order is `Sha1 < Sha256`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HashKind {
    /// SHA-1 with collision detection, 20 byte ids. The default.
    #[default]
    Sha1,
    /// SHA-256, 32 byte ids.
    Sha256,
}

impl HashKind {
    /// All supported kinds, in `Ord` order.
    pub const ALL: [HashKind; 2] = [HashKind::Sha1, HashKind::Sha256];

    /// The longest id of any kind, in bytes.
    pub const MAX_LEN: usize = 32;

    /// The longest id of any kind, in hex digits.
    pub const MAX_HEX_LEN: usize = 64;

    /// Length of an id in bytes (20 or 32).
    pub const fn byte_len(self) -> usize {
        match self {
            HashKind::Sha1 => 20,
            HashKind::Sha256 => 32,
        }
    }

    /// Length of an id in hex digits (40 or 64).
    pub const fn hex_len(self) -> usize {
        self.byte_len() * 2
    }

    /// The name used in configuration and on the command line
    /// (`extensions.objectFormat`, `--object-format`): `"sha1"` or `"sha256"`.
    pub const fn name(self) -> &'static str {
        match self {
            HashKind::Sha1 => "sha1",
            HashKind::Sha256 => "sha256",
        }
    }

    /// Parse a name as returned by [`HashKind::name`]. Exact match only.
    pub fn from_name(name: &str) -> Option<HashKind> {
        match name {
            "sha1" => Some(HashKind::Sha1),
            "sha256" => Some(HashKind::Sha256),
            _ => None,
        }
    }

    /// The 4-byte format identifier stored in some binary file headers,
    /// as a big-endian `u32`: `"sha1"` for SHA-1, `"s256"` for SHA-256.
    pub const fn format_id(self) -> u32 {
        match self {
            HashKind::Sha1 => u32::from_be_bytes(*b"sha1"),
            HashKind::Sha256 => u32::from_be_bytes(*b"s256"),
        }
    }

    /// Inverse of [`HashKind::format_id`].
    pub fn from_format_id(id: u32) -> Option<HashKind> {
        HashKind::ALL.into_iter().find(|k| k.format_id() == id)
    }

    /// The kind whose ids are `len` bytes long, if any.
    pub fn from_byte_len(len: usize) -> Option<HashKind> {
        HashKind::ALL.into_iter().find(|k| k.byte_len() == len)
    }

    /// The kind whose full hex ids are `hex_len` digits long, if any.
    pub fn from_hex_len(hex_len: usize) -> Option<HashKind> {
        HashKind::ALL.into_iter().find(|k| k.hex_len() == hex_len)
    }

    /// The all-zero id of this kind.
    pub const fn null(self) -> ObjectId {
        match self {
            HashKind::Sha1 => ObjectId::Sha1([0; 20]),
            HashKind::Sha256 => ObjectId::Sha256([0; 32]),
        }
    }

    /// Id of the empty blob (`blob 0\0`).
    pub const fn empty_blob(self) -> ObjectId {
        match self {
            // e69de29bb2d1d6434b8b29ae775ad8c2e48c5391
            HashKind::Sha1 => ObjectId::Sha1([
                0xe6, 0x9d, 0xe2, 0x9b, 0xb2, 0xd1, 0xd6, 0x43, 0x4b, 0x8b, 0x29, 0xae, 0x77, 0x5a,
                0xd8, 0xc2, 0xe4, 0x8c, 0x53, 0x91,
            ]),
            // 473a0f4c3be8a93681a267e3b1e9a7dcda1185436fe141f7749120a303721813
            HashKind::Sha256 => ObjectId::Sha256([
                0x47, 0x3a, 0x0f, 0x4c, 0x3b, 0xe8, 0xa9, 0x36, 0x81, 0xa2, 0x67, 0xe3, 0xb1, 0xe9,
                0xa7, 0xdc, 0xda, 0x11, 0x85, 0x43, 0x6f, 0xe1, 0x41, 0xf7, 0x74, 0x91, 0x20, 0xa3,
                0x03, 0x72, 0x18, 0x13,
            ]),
        }
    }

    /// Id of the empty tree (`tree 0\0`).
    pub const fn empty_tree(self) -> ObjectId {
        match self {
            // 4b825dc642cb6eb9a060e54bf8d69288fbee4904
            HashKind::Sha1 => ObjectId::Sha1([
                0x4b, 0x82, 0x5d, 0xc6, 0x42, 0xcb, 0x6e, 0xb9, 0xa0, 0x60, 0xe5, 0x4b, 0xf8, 0xd6,
                0x92, 0x88, 0xfb, 0xee, 0x49, 0x04,
            ]),
            // 6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321
            HashKind::Sha256 => ObjectId::Sha256([
                0x6e, 0xf1, 0x9b, 0x41, 0x22, 0x5c, 0x53, 0x69, 0xf1, 0xc1, 0x04, 0xd4, 0x5d, 0x8d,
                0x85, 0xef, 0xa9, 0xb0, 0x57, 0xb5, 0x3b, 0x14, 0xb4, 0xb9, 0xb9, 0x39, 0xdd, 0x74,
                0xde, 0xcc, 0x53, 0x21,
            ]),
        }
    }
}

impl fmt::Display for HashKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for HashKind {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        HashKind::from_name(s).ok_or_else(|| Error::UnknownHashKind(s.to_owned()))
    }
}
