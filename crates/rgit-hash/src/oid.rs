// SPDX-License-Identifier: MIT

//! Object ids and abbreviated id prefixes.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use crate::{Error, HashKind};

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Value of one ASCII hex digit (either case).
fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Decode `hex` (even length, either case) into `out[..hex.len() / 2]`;
/// an odd trailing digit fills the high nibble of the next byte.
fn decode_hex_into(hex: &[u8], out: &mut [u8]) -> Result<(), Error> {
    for (i, &c) in hex.iter().enumerate() {
        let v = hex_val(c).ok_or(Error::InvalidHexDigit { position: i })?;
        if i % 2 == 0 {
            out[i / 2] = v << 4;
        } else {
            out[i / 2] |= v;
        }
    }
    Ok(())
}

/// The name of an object: a SHA-1 (20 bytes) or SHA-256 (32 bytes) digest.
///
/// `ObjectId` is `Copy` and cheap to pass by value. Ordering compares the
/// kind first (`Sha1 < Sha256`), then the raw bytes, which is the same as
/// comparing the lowercase hex strings within one kind (the order used by
/// pack indexes and sorted id lists).
///
/// [`Display`](fmt::Display) prints lowercase hex; a precision abbreviates,
/// so `format!("{id:.7}")` gives the first seven hex digits.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ObjectId {
    /// A SHA-1 id.
    Sha1([u8; 20]),
    /// A SHA-256 id.
    Sha256([u8; 32]),
}

impl ObjectId {
    /// Build an id of `kind` from raw bytes; `bytes.len()` must equal `kind.byte_len()`.
    pub fn from_bytes(kind: HashKind, bytes: &[u8]) -> Result<ObjectId, Error> {
        if bytes.len() != kind.byte_len() {
            return Err(Error::InvalidLength {
                kind,
                expected: kind.byte_len(),
                actual: bytes.len(),
            });
        }
        let mut id = kind.null();
        id.as_bytes_mut().copy_from_slice(bytes);
        Ok(id)
    }

    /// Build an id from raw bytes, deriving the kind from the length (20 or 32).
    pub fn from_bytes_any(bytes: &[u8]) -> Result<ObjectId, Error> {
        let kind = HashKind::from_byte_len(bytes.len()).ok_or(Error::UnknownLength(bytes.len()))?;
        ObjectId::from_bytes(kind, bytes)
    }

    /// Parse a full hex id of `kind` (exactly `kind.hex_len()` digits, either case).
    pub fn from_hex(kind: HashKind, hex: &[u8]) -> Result<ObjectId, Error> {
        if hex.len() != kind.hex_len() {
            return Err(Error::InvalidLength {
                kind,
                expected: kind.hex_len(),
                actual: hex.len(),
            });
        }
        let mut id = kind.null();
        decode_hex_into(hex, id.as_bytes_mut())?;
        Ok(id)
    }

    /// Parse a full hex id, deriving the kind from the length (40 or 64 digits).
    ///
    /// This is also what [`FromStr`] does.
    pub fn from_hex_any(hex: &[u8]) -> Result<ObjectId, Error> {
        let kind = HashKind::from_hex_len(hex.len()).ok_or(Error::UnknownLength(hex.len()))?;
        ObjectId::from_hex(kind, hex)
    }

    /// The all-zero id of `kind`. Same as [`HashKind::null`].
    pub const fn null(kind: HashKind) -> ObjectId {
        kind.null()
    }

    /// Whether this is the all-zero id.
    pub fn is_null(&self) -> bool {
        self.as_bytes().iter().all(|&b| b == 0)
    }

    /// Id of the empty blob of `kind`. Same as [`HashKind::empty_blob`].
    pub const fn empty_blob(kind: HashKind) -> ObjectId {
        kind.empty_blob()
    }

    /// Id of the empty tree of `kind`. Same as [`HashKind::empty_tree`].
    pub const fn empty_tree(kind: HashKind) -> ObjectId {
        kind.empty_tree()
    }

    /// The hash algorithm of this id.
    pub const fn kind(&self) -> HashKind {
        match self {
            ObjectId::Sha1(_) => HashKind::Sha1,
            ObjectId::Sha256(_) => HashKind::Sha256,
        }
    }

    /// The raw digest bytes (20 or 32).
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            ObjectId::Sha1(b) => b,
            ObjectId::Sha256(b) => b,
        }
    }

    /// Mutable access to the raw digest bytes.
    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        match self {
            ObjectId::Sha1(b) => b,
            ObjectId::Sha256(b) => b,
        }
    }

    /// Write the full lowercase hex form into `out` and return it as `&str`.
    ///
    /// `out` must hold at least `self.kind().hex_len()` bytes; use a
    /// `[u8; HashKind::MAX_HEX_LEN]` buffer to avoid allocating.
    ///
    /// # Panics
    /// If `out` is too short.
    pub fn hex_to_buf<'a>(&self, out: &'a mut [u8]) -> &'a str {
        let n = self.kind().hex_len();
        for (i, b) in self.as_bytes().iter().enumerate() {
            out[2 * i] = HEX[usize::from(b >> 4)];
            out[2 * i + 1] = HEX[usize::from(b & 0xf)];
        }
        std::str::from_utf8(&out[..n]).expect("hex digits are ASCII")
    }

    /// The full lowercase hex form.
    pub fn to_hex(&self) -> String {
        let mut buf = [0u8; HashKind::MAX_HEX_LEN];
        self.hex_to_buf(&mut buf).to_owned()
    }

    /// The first `len` lowercase hex digits (clamped to the full length).
    ///
    /// This only truncates; choosing a length that is unique in a repository
    /// (`core.abbrev`, `--abbrev`) is the object database's job.
    pub fn to_hex_with_len(&self, len: usize) -> String {
        let mut buf = [0u8; HashKind::MAX_HEX_LEN];
        let s = self.hex_to_buf(&mut buf);
        s[..len.min(s.len())].to_owned()
    }

    /// Whether this id starts with `prefix` (and has the same kind).
    pub fn starts_with(&self, prefix: &Prefix) -> bool {
        prefix.cmp_oid(self) == Ordering::Equal
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut buf = [0u8; HashKind::MAX_HEX_LEN];
        let s = self.hex_to_buf(&mut buf);
        let s = match f.precision() {
            Some(p) => &s[..p.min(s.len())],
            None => s,
        };
        f.pad(s)
    }
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut buf = [0u8; HashKind::MAX_HEX_LEN];
        let name = match self {
            ObjectId::Sha1(_) => "Sha1",
            ObjectId::Sha256(_) => "Sha256",
        };
        write!(f, "{name}({})", self.hex_to_buf(&mut buf))
    }
}

impl FromStr for ObjectId {
    type Err = Error;

    /// Parse a full hex id of either kind; see [`ObjectId::from_hex_any`].
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ObjectId::from_hex_any(s.as_bytes())
    }
}

impl AsRef<[u8]> for ObjectId {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

/// An abbreviated object id: the first `hex_len` hex digits of an id.
///
/// Used to resolve short names such as `e69de29` and to binary-search sorted
/// id lists. The digits beyond `hex_len` are stored as zero.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Prefix {
    id: ObjectId,
    hex_len: usize,
}

impl Prefix {
    /// Parse 1 to `kind.hex_len()` hex digits (either case) as a prefix of a
    /// `kind` id. The minimum length that commands accept (4) is the caller's
    /// policy, not enforced here.
    pub fn from_hex(kind: HashKind, hex: &[u8]) -> Result<Prefix, Error> {
        if hex.is_empty() || hex.len() > kind.hex_len() {
            return Err(Error::InvalidPrefixLength {
                kind,
                actual: hex.len(),
            });
        }
        let mut id = kind.null();
        decode_hex_into(hex, id.as_bytes_mut())?;
        Ok(Prefix {
            id,
            hex_len: hex.len(),
        })
    }

    /// The prefix made of the first `hex_len` digits of `id`.
    pub fn new(id: ObjectId, hex_len: usize) -> Result<Prefix, Error> {
        let kind = id.kind();
        if hex_len == 0 || hex_len > kind.hex_len() {
            return Err(Error::InvalidPrefixLength {
                kind,
                actual: hex_len,
            });
        }
        let mut id = id;
        let bytes = id.as_bytes_mut();
        let full = hex_len / 2;
        if hex_len % 2 == 1 {
            bytes[full] &= 0xf0;
            bytes[full + 1..].fill(0);
        } else {
            bytes[full..].fill(0);
        }
        Ok(Prefix { id, hex_len })
    }

    /// Number of hex digits in the prefix.
    pub fn hex_len(&self) -> usize {
        self.hex_len
    }

    /// The hash kind of ids this prefix can match.
    pub fn kind(&self) -> HashKind {
        self.id.kind()
    }

    /// The prefix padded with zeros to a full id (the smallest id it matches).
    pub fn as_id(&self) -> &ObjectId {
        &self.id
    }

    /// Compare `oid`'s leading `hex_len` digits with this prefix.
    ///
    /// Returns `Equal` when `oid` starts with the prefix, otherwise the order
    /// of the prefix relative to `oid`, so a sorted id list can be binary
    /// searched with `ids.binary_search_by(|id| prefix.cmp_oid(id).reverse())`.
    /// Ids of a different kind compare by kind.
    pub fn cmp_oid(&self, oid: &ObjectId) -> Ordering {
        if self.kind() != oid.kind() {
            return self.kind().cmp(&oid.kind());
        }
        let a = self.id.as_bytes();
        let b = oid.as_bytes();
        let full = self.hex_len / 2;
        match a[..full].cmp(&b[..full]) {
            Ordering::Equal if self.hex_len % 2 == 1 => a[full].cmp(&(b[full] & 0xf0)),
            other => other,
        }
    }
}

impl fmt::Display for Prefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut buf = [0u8; HashKind::MAX_HEX_LEN];
        f.pad(&self.id.hex_to_buf(&mut buf)[..self.hex_len])
    }
}

impl fmt::Debug for Prefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Prefix({:?}, {self})", self.kind())
    }
}

impl From<ObjectId> for Prefix {
    /// A prefix covering the whole id.
    fn from(id: ObjectId) -> Prefix {
        Prefix {
            id,
            hex_len: id.kind().hex_len(),
        }
    }
}
