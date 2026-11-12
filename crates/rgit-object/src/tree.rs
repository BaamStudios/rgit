// SPDX-License-Identifier: MIT

//! Tree objects: a sorted list of `<mode> SP <name> NUL <raw id>` entries.

use std::cmp::Ordering;
use std::fmt;

use rgit_hash::{HashKind, ObjectId};

use crate::{Error, ObjectKind};

/// The mode of a tree entry.
///
/// The numeric value is kept together with its exact textual form when that
/// form is not the canonical one (zero padded like `040000`, or a mode such
/// as `100664` written by old versions of git), so that serialising an entry
/// reproduces the original bytes.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct EntryMode {
    bits: u32,
    /// The original digits when they differ from `format!("{bits:o}")`.
    raw: Option<Box<[u8]>>,
}

impl EntryMode {
    /// A regular file, `100644`.
    pub const BLOB: EntryMode = EntryMode::from_bits(0o100644);
    /// An executable file, `100755`.
    pub const BLOB_EXECUTABLE: EntryMode = EntryMode::from_bits(0o100755);
    /// A symbolic link, `120000`.
    pub const LINK: EntryMode = EntryMode::from_bits(0o120000);
    /// A subdirectory, `40000`.
    pub const TREE: EntryMode = EntryMode::from_bits(0o040000);
    /// A submodule commit ("gitlink"), `160000`.
    pub const COMMIT: EntryMode = EntryMode::from_bits(0o160000);

    /// A mode with the given numeric value, written in canonical form.
    pub const fn from_bits(bits: u32) -> EntryMode {
        EntryMode { bits, raw: None }
    }

    /// Parse the octal digits of a mode as found in a tree (or given to
    /// `git mktree`). Fails on an empty string, a non-octal digit or a value
    /// that does not fit in 32 bits.
    pub fn from_octal(digits: &[u8]) -> Option<EntryMode> {
        if digits.is_empty() {
            return None;
        }
        let mut bits: u32 = 0;
        for &d in digits {
            if !(b'0'..=b'7').contains(&d) {
                return None;
            }
            bits = bits.checked_mul(8)?.checked_add(u32::from(d - b'0'))?;
        }
        let canonical = EntryMode::from_bits(bits);
        if canonical.canonical_digits().as_bytes() == digits {
            Some(canonical)
        } else {
            Some(EntryMode {
                bits,
                raw: Some(digits.into()),
            })
        }
    }

    /// The numeric value.
    pub const fn bits(&self) -> u32 {
        self.bits
    }

    /// Whether the textual form is the canonical one (no zero padding).
    pub fn is_canonical_text(&self) -> bool {
        self.raw.is_none()
    }

    /// The digits as stored (the original text for a non-canonical form).
    pub fn digits(&self) -> std::borrow::Cow<'_, [u8]> {
        match &self.raw {
            Some(raw) => std::borrow::Cow::Borrowed(raw),
            None => std::borrow::Cow::Owned(self.canonical_digits().into_bytes()),
        }
    }

    fn canonical_digits(&self) -> String {
        format!("{:o}", self.bits)
    }

    /// The same mode without its original spelling (zero padding removed).
    pub fn with_canonical_text(&self) -> EntryMode {
        EntryMode::from_bits(self.bits)
    }

    /// Whether the entry is a subdirectory (`(mode & 0o170000) == 0o040000`).
    ///
    /// This decides the sort position of the entry (see [`entry_cmp`]).
    pub const fn is_tree(&self) -> bool {
        self.bits & 0o170000 == 0o040000
    }

    /// Whether the mode is one of the five that git writes
    /// (`100644`, `100755`, `120000`, `40000`, `160000`).
    pub const fn is_standard(&self) -> bool {
        matches!(
            self.bits,
            0o100644 | 0o100755 | 0o120000 | 0o040000 | 0o160000
        )
    }

    /// The mode git uses for this entry when reading it, mapping
    /// non-standard modes from old repositories onto the five standard ones:
    /// regular files become `100755` when any execute bit is set, else
    /// `100644`; symlinks and directories keep their type; everything else
    /// is treated as a submodule commit.
    pub const fn canonical(&self) -> EntryMode {
        EntryMode::from_bits(self.canonical_bits())
    }

    const fn canonical_bits(&self) -> u32 {
        match self.bits & 0o170000 {
            0o100000 => {
                if self.bits & 0o111 != 0 {
                    0o100755
                } else {
                    0o100644
                }
            }
            0o120000 => 0o120000,
            0o040000 => 0o040000,
            _ => 0o160000,
        }
    }

    /// The object type an entry with this mode points to
    /// (after [canonicalisation](EntryMode::canonical)).
    pub const fn object_kind(&self) -> ObjectKind {
        match self.canonical_bits() {
            0o040000 => ObjectKind::Tree,
            0o160000 => ObjectKind::Commit,
            _ => ObjectKind::Blob,
        }
    }

    /// Whether this is a symbolic link (`120000`).
    pub const fn is_link(&self) -> bool {
        self.bits & 0o170000 == 0o120000
    }
}

impl fmt::Debug for EntryMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EntryMode({})", String::from_utf8_lossy(&self.digits()))
    }
}

impl fmt::Display for EntryMode {
    /// The six-digit form used by `git ls-tree` and `git cat-file -p`
    /// (`040000` for trees).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:06o}", self.bits)
    }
}

/// One entry of a tree.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TreeEntry {
    /// The mode.
    pub mode: EntryMode,
    /// The file name (any bytes except NUL; normally no `/`).
    pub name: Vec<u8>,
    /// The id of the blob, tree or commit.
    pub oid: ObjectId,
}

impl TreeEntry {
    /// An entry.
    pub fn new(mode: EntryMode, name: impl Into<Vec<u8>>, oid: ObjectId) -> TreeEntry {
        TreeEntry {
            mode,
            name: name.into(),
            oid,
        }
    }

    /// Compare two entries in tree order (see [`entry_cmp`]).
    pub fn cmp_tree_order(&self, other: &TreeEntry) -> Ordering {
        entry_cmp(
            &self.name,
            self.mode.is_tree(),
            &other.name,
            other.mode.is_tree(),
        )
    }
}

/// Compare two tree entries in git's tree order: byte-wise by name, where a
/// subdirectory compares as if its name ended in `/`.
///
/// So `a.c` (file) sorts before `a` (directory, compared as `a/`), while the
/// file `a` sorts before `a.c`.
pub fn entry_cmp(a: &[u8], a_is_tree: bool, b: &[u8], b_is_tree: bool) -> Ordering {
    let n = a.len().min(b.len());
    match a[..n].cmp(&b[..n]) {
        Ordering::Equal => {}
        other => return other,
    }
    let ca = a
        .get(n)
        .copied()
        .or(if a_is_tree { Some(b'/') } else { None });
    let cb = b
        .get(n)
        .copied()
        .or(if b_is_tree { Some(b'/') } else { None });
    // A missing byte (end of a non-tree name) sorts first.
    ca.cmp(&cb)
}

/// A tree object.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Tree {
    /// The entries, in stored order. [`Tree::parse`] keeps the order found;
    /// call [`Tree::sort`] before serialising a tree built by hand.
    pub entries: Vec<TreeEntry>,
}

impl Tree {
    /// An empty tree.
    pub fn new() -> Tree {
        Tree::default()
    }

    /// Parse tree content (without the object header). Entry order, modes
    /// and names are kept exactly, so [`Tree::serialize`] reproduces `data`.
    pub fn parse(hash: HashKind, data: &[u8]) -> Result<Tree, Error> {
        let mut entries = Vec::new();
        for entry in TreeIter::new(hash, data) {
            entries.push(entry?.to_owned());
        }
        Ok(Tree { entries })
    }

    /// Sort the entries into tree order (see [`entry_cmp`]).
    pub fn sort(&mut self) {
        self.entries.sort_by(TreeEntry::cmp_tree_order);
    }

    /// Append the serialised content to `out`.
    pub fn write_to(&self, out: &mut Vec<u8>) {
        for e in &self.entries {
            out.extend_from_slice(&e.mode.digits());
            out.push(b' ');
            out.extend_from_slice(&e.name);
            out.push(0);
            out.extend_from_slice(e.oid.as_bytes());
        }
    }

    /// The serialised content (without the object header).
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_to(&mut out);
        out
    }
}

/// A borrowed tree entry, as returned by [`TreeIter`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeEntryRef<'a> {
    /// The mode digits exactly as stored.
    pub mode_digits: &'a [u8],
    /// The parsed mode value.
    pub mode_bits: u32,
    /// The file name.
    pub name: &'a [u8],
    /// The id.
    pub oid: ObjectId,
}

impl TreeEntryRef<'_> {
    /// Whether the entry is a subdirectory.
    pub fn is_tree(&self) -> bool {
        self.mode_bits & 0o170000 == 0o040000
    }

    /// Convert to an owned entry.
    pub fn to_owned(&self) -> TreeEntry {
        TreeEntry {
            mode: EntryMode::from_octal(self.mode_digits).expect("validated while parsing"),
            name: self.name.to_vec(),
            oid: self.oid,
        }
    }
}

/// Iterate over the entries of serialised tree content without allocating.
///
/// After the first error the iterator is exhausted.
#[derive(Clone, Debug)]
pub struct TreeIter<'a> {
    hash: HashKind,
    data: &'a [u8],
}

impl<'a> TreeIter<'a> {
    /// Iterate over `data`, tree content of an object named with `hash`.
    pub fn new(hash: HashKind, data: &'a [u8]) -> TreeIter<'a> {
        TreeIter { hash, data }
    }

    fn next_entry(&mut self) -> Result<TreeEntryRef<'a>, Error> {
        let data = self.data;
        let hash_len = self.hash.byte_len();
        let nul = data
            .iter()
            .position(|&b| b == 0)
            .ok_or(Error::TreeTooShort)?;
        if data.len() - nul - 1 < hash_len {
            return Err(Error::TreeTooShort);
        }
        let head = &data[..nul];
        let sp = head
            .iter()
            .position(|&b| b == b' ')
            .ok_or(Error::TreeMalformedMode)?;
        let mode_digits = &head[..sp];
        let mode = EntryMode::from_octal(mode_digits).ok_or(Error::TreeMalformedMode)?;
        let name = &head[sp + 1..];
        if name.is_empty() {
            return Err(Error::TreeEmptyFilename);
        }
        let oid = ObjectId::from_bytes(self.hash, &data[nul + 1..nul + 1 + hash_len])
            .expect("length checked");
        self.data = &data[nul + 1 + hash_len..];
        Ok(TreeEntryRef {
            mode_digits,
            mode_bits: mode.bits(),
            name,
            oid,
        })
    }
}

impl<'a> Iterator for TreeIter<'a> {
    type Item = Result<TreeEntryRef<'a>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.data.is_empty() {
            return None;
        }
        let r = self.next_entry();
        if r.is_err() {
            self.data = &[];
        }
        Some(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order() {
        assert_eq!(entry_cmp(b"a.c", false, b"a", true), Ordering::Less);
        assert_eq!(entry_cmp(b"a", false, b"a.c", false), Ordering::Less);
        assert_eq!(entry_cmp(b"a", false, b"a", true), Ordering::Less);
        assert_eq!(entry_cmp(b"a", true, b"a", true), Ordering::Equal);
        assert_eq!(entry_cmp(b"a/", false, b"a", true), Ordering::Equal);
        assert_eq!(entry_cmp(b"a0", false, b"a", true), Ordering::Greater);
    }

    #[test]
    fn modes() {
        let m = EntryMode::from_octal(b"040000").unwrap();
        assert!(m.is_tree());
        assert!(!m.is_canonical_text());
        assert_eq!(&*m.digits(), b"040000");
        assert_eq!(EntryMode::from_octal(b"40000").unwrap(), EntryMode::TREE);
        assert_eq!(
            EntryMode::from_octal(b"100664").unwrap().canonical(),
            EntryMode::BLOB
        );
        assert_eq!(
            EntryMode::from_octal(b"644").unwrap().canonical(),
            EntryMode::COMMIT
        );
        assert!(EntryMode::from_octal(b"").is_none());
        assert!(EntryMode::from_octal(b"8").is_none());
        assert!(EntryMode::from_octal(b"77777777777").is_none());
        assert_eq!(EntryMode::TREE.to_string(), "040000");
    }
}
