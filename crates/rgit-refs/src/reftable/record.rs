// SPDX-License-Identifier: MIT
//! Records of a reftable (`Documentation/technical/reftable.adoc`): how
//! reference, log, object and index records are laid out inside a block.
//!
//! Every record starts with a prefix-compressed key:
//! `varint(prefix_len) varint((suffix_len << 3) | extra) suffix`. The three
//! `extra` bits hold the value type of a reference or log record and the
//! small position count of an object record.

use rgit_hash::{HashKind, ObjectId};

use super::FormatError;

/// Appends `value` in the variable-length encoding of the format (the one
/// pack files use for the base offset of an offset delta): groups of seven
/// bits, most significant group first, every byte but the last with the
/// high bit set. Reading adds one before each shift, so every group in
/// front of the last one stores one less than the bits above it.
pub(crate) fn put_varint(out: &mut Vec<u8>, value: u64) {
    // What is left of the value at each byte, last byte first.
    let mut left = [0u64; 10];
    let mut count = 0;
    let mut v = value;
    loop {
        left[count] = v;
        count += 1;
        if v < 0x80 {
            break;
        }
        v = (v >> 7) - 1;
    }
    for (i, &part) in left[..count].iter().enumerate().rev() {
        let more = if i == 0 { 0 } else { 0x80 };
        out.push(more | (part & 0x7f) as u8);
    }
}

/// Reads a varint from the start of `data`: the value and its length.
pub(crate) fn get_varint(data: &[u8]) -> Option<(u64, usize)> {
    let mut at = 0;
    let mut byte = *data.first()?;
    let mut value = u64::from(byte & 0x7f);
    while byte & 0x80 != 0 {
        at += 1;
        byte = *data.get(at)?;
        // More than 64 bits is not a value any writer produces.
        value = value.checked_add(1)?.checked_mul(128)? | u64::from(byte & 0x7f);
    }
    Some((value, at + 1))
}

/// A cursor over the bytes of one block.
pub(crate) struct Cursor<'a> {
    pub data: &'a [u8],
    pub at: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(data: &'a [u8], at: usize) -> Self {
        Cursor { data, at }
    }

    pub fn varint(&mut self) -> Result<u64, FormatError> {
        let rest = self.data.get(self.at..).ok_or(FormatError::Truncated)?;
        let (v, n) = get_varint(rest).ok_or(FormatError::Truncated)?;
        self.at += n;
        Ok(v)
    }

    pub fn bytes(&mut self, len: usize) -> Result<&'a [u8], FormatError> {
        let end = self.at.checked_add(len).ok_or(FormatError::Truncated)?;
        let out = self.data.get(self.at..end).ok_or(FormatError::Truncated)?;
        self.at = end;
        Ok(out)
    }

    pub fn string(&mut self) -> Result<&'a [u8], FormatError> {
        let len = self.varint()?;
        let len = usize::try_from(len).map_err(|_| FormatError::Truncated)?;
        self.bytes(len)
    }

    pub fn oid(&mut self, kind: HashKind) -> Result<ObjectId, FormatError> {
        let raw = self.bytes(kind.byte_len())?;
        ObjectId::from_bytes(kind, raw).map_err(|_| FormatError::Truncated)
    }

    /// Reads the key header of a record: rebuilds the key in `key` (which
    /// holds the previous key) and returns the `extra` bits.
    pub fn key(&mut self, key: &mut Vec<u8>) -> Result<u8, FormatError> {
        let prefix = self.varint()?;
        let packed = self.varint()?;
        let suffix_len = usize::try_from(packed >> 3).map_err(|_| FormatError::Truncated)?;
        let prefix = usize::try_from(prefix).map_err(|_| FormatError::Truncated)?;
        if prefix > key.len() {
            return Err(FormatError::Corrupt("key prefix longer than previous key"));
        }
        let suffix = self.bytes(suffix_len)?;
        key.truncate(prefix);
        key.extend_from_slice(suffix);
        Ok((packed & 7) as u8)
    }
}

/// The value of a reference record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefValue {
    /// The reference was deleted (hides older tables' records).
    Deletion,
    /// A direct reference.
    Direct(ObjectId),
    /// A direct reference to an annotated tag and the object it peels to.
    Peeled(ObjectId, ObjectId),
    /// A symbolic reference.
    Symbolic(Vec<u8>),
}

/// One reference record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefRecord {
    /// The reference name.
    pub name: Vec<u8>,
    /// The transaction that wrote the record.
    pub update_index: u64,
    /// The value.
    pub value: RefValue,
}

impl RefRecord {
    /// The value type stored in the key's `extra` bits.
    pub(crate) fn value_type(&self) -> u8 {
        match self.value {
            RefValue::Deletion => 0,
            RefValue::Direct(_) => 1,
            RefValue::Peeled(..) => 2,
            RefValue::Symbolic(_) => 3,
        }
    }

    /// Whether the record is a deletion.
    pub fn is_deletion(&self) -> bool {
        self.value == RefValue::Deletion
    }

    /// Appends everything after the key. `None` when `update_index` is
    /// below the table's minimum.
    pub(crate) fn encode_value(&self, out: &mut Vec<u8>, min_update_index: u64) -> Option<()> {
        put_varint(out, self.update_index.checked_sub(min_update_index)?);
        match &self.value {
            RefValue::Deletion => {}
            RefValue::Direct(id) => out.extend_from_slice(id.as_bytes()),
            RefValue::Peeled(id, peeled) => {
                out.extend_from_slice(id.as_bytes());
                out.extend_from_slice(peeled.as_bytes());
            }
            RefValue::Symbolic(target) => {
                put_varint(out, target.len() as u64);
                out.extend_from_slice(target);
            }
        }
        Some(())
    }

    pub(crate) fn decode_value(
        cur: &mut Cursor<'_>,
        name: &[u8],
        value_type: u8,
        kind: HashKind,
        min_update_index: u64,
    ) -> Result<RefRecord, FormatError> {
        let delta = cur.varint()?;
        let value = match value_type {
            0 => RefValue::Deletion,
            1 => RefValue::Direct(cur.oid(kind)?),
            2 => {
                let id = cur.oid(kind)?;
                RefValue::Peeled(id, cur.oid(kind)?)
            }
            3 => RefValue::Symbolic(cur.string()?.to_vec()),
            _ => return Err(FormatError::Corrupt("unknown reference value type")),
        };
        Ok(RefRecord {
            name: name.to_vec(),
            update_index: min_update_index.wrapping_add(delta),
            value,
        })
    }
}

/// The data of a log record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogUpdate {
    /// Value before the update (null: created).
    pub old: ObjectId,
    /// Value after the update (null: deleted).
    pub new: ObjectId,
    /// Committer name.
    pub name: Vec<u8>,
    /// Committer email, without the angle brackets.
    pub email: Vec<u8>,
    /// Seconds since the epoch.
    pub time: u64,
    /// Offset from UTC in minutes.
    pub tz_offset: i16,
    /// The message, as stored (git ends a non-empty one with a line feed).
    pub message: Vec<u8>,
}

/// One log record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogRecord {
    /// The reference the entry belongs to.
    pub refname: Vec<u8>,
    /// The transaction that wrote the entry; unique per reference.
    pub update_index: u64,
    /// The entry; `None` deletes the entry with this key in older tables.
    pub update: Option<LogUpdate>,
}

impl LogRecord {
    /// The key: the name, a NUL byte and the inverted update index, so
    /// that the entries of one reference sort newest first.
    pub fn key(&self) -> Vec<u8> {
        log_key(&self.refname, self.update_index)
    }

    pub(crate) fn value_type(&self) -> u8 {
        u8::from(self.update.is_some())
    }

    pub(crate) fn encode_value(&self, out: &mut Vec<u8>) {
        let Some(u) = &self.update else { return };
        out.extend_from_slice(u.old.as_bytes());
        out.extend_from_slice(u.new.as_bytes());
        put_varint(out, u.name.len() as u64);
        out.extend_from_slice(&u.name);
        put_varint(out, u.email.len() as u64);
        out.extend_from_slice(&u.email);
        put_varint(out, u.time);
        out.extend_from_slice(&u.tz_offset.to_be_bytes());
        put_varint(out, u.message.len() as u64);
        out.extend_from_slice(&u.message);
    }

    pub(crate) fn decode_value(
        cur: &mut Cursor<'_>,
        key: &[u8],
        value_type: u8,
        kind: HashKind,
    ) -> Result<LogRecord, FormatError> {
        let (refname, update_index) =
            split_log_key(key).ok_or(FormatError::Corrupt("malformed log record key"))?;
        let update = match value_type {
            0 => None,
            1 => {
                let old = cur.oid(kind)?;
                let new = cur.oid(kind)?;
                let name = cur.string()?.to_vec();
                let email = cur.string()?.to_vec();
                let time = cur.varint()?;
                let tz = cur.bytes(2)?;
                let message = cur.string()?.to_vec();
                Some(LogUpdate {
                    old,
                    new,
                    name,
                    email,
                    time,
                    tz_offset: i16::from_be_bytes([tz[0], tz[1]]),
                    message,
                })
            }
            _ => return Err(FormatError::Corrupt("unknown log record type")),
        };
        Ok(LogRecord {
            refname: refname.to_vec(),
            update_index,
            update,
        })
    }
}

/// The key of the log record of `refname` at `update_index`.
pub fn log_key(refname: &[u8], update_index: u64) -> Vec<u8> {
    let mut key = Vec::with_capacity(refname.len() + 9);
    key.extend_from_slice(refname);
    key.push(0);
    key.extend_from_slice(&(u64::MAX - update_index).to_be_bytes());
    key
}

/// Splits a log record key into the name and the update index.
pub(crate) fn split_log_key(key: &[u8]) -> Option<(&[u8], u64)> {
    let split = key.len().checked_sub(9)?;
    if key[split] != 0 {
        return None;
    }
    let mut raw = [0u8; 8];
    raw.copy_from_slice(&key[split + 1..]);
    Some((&key[..split], u64::MAX - u64::from_be_bytes(raw)))
}

/// One object record: the reference blocks holding references to the
/// objects whose id starts with `prefix`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjRecord {
    /// The abbreviated object id.
    pub prefix: Vec<u8>,
    /// File positions of reference blocks, ascending. Empty when the list
    /// was too long to store: every reference block may hold a match.
    pub offsets: Vec<u64>,
}

impl ObjRecord {
    pub(crate) fn value_type(&self) -> u8 {
        match self.offsets.len() {
            n @ 1..=7 => n as u8,
            _ => 0,
        }
    }

    pub(crate) fn encode_value(&self, out: &mut Vec<u8>) {
        if self.value_type() == 0 {
            put_varint(out, self.offsets.len() as u64);
        }
        let mut last = 0;
        for (i, &off) in self.offsets.iter().enumerate() {
            put_varint(out, if i == 0 { off } else { off - last });
            last = off;
        }
    }

    pub(crate) fn decode_value(
        cur: &mut Cursor<'_>,
        key: &[u8],
        value_type: u8,
    ) -> Result<ObjRecord, FormatError> {
        let count = match value_type {
            0 => cur.varint()?,
            n => u64::from(n),
        };
        let mut offsets = Vec::new();
        let mut last = 0u64;
        for i in 0..count {
            let v = cur.varint()?;
            last = if i == 0 { v } else { last.wrapping_add(v) };
            offsets.push(last);
        }
        Ok(ObjRecord {
            prefix: key.to_vec(),
            offsets,
        })
    }
}

/// One index record: the last key of the block at `offset`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexRecord {
    /// The last key of the block.
    pub last_key: Vec<u8>,
    /// Where the block starts in the file.
    pub offset: u64,
}

impl IndexRecord {
    pub(crate) fn encode_value(&self, out: &mut Vec<u8>) {
        put_varint(out, self.offset);
    }

    pub(crate) fn decode_value(cur: &mut Cursor<'_>, key: &[u8]) -> Result<Self, FormatError> {
        Ok(IndexRecord {
            last_key: key.to_vec(),
            offset: cur.varint()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_round_trip() {
        for v in [
            0u64,
            1,
            127,
            128,
            129,
            16383,
            16384,
            16511,
            16512,
            1 << 32,
            u64::MAX,
        ] {
            let mut out = Vec::new();
            put_varint(&mut out, v);
            assert_eq!(get_varint(&out), Some((v, out.len())), "{v}");
        }
        // The encodings of the offset-delta varint: no value has two.
        let mut out = Vec::new();
        put_varint(&mut out, 128);
        assert_eq!(out, [0x80, 0x00]);
        out.clear();
        put_varint(&mut out, 16511);
        assert_eq!(out, [0xff, 0x7f]);
        assert_eq!(get_varint(&[0x80]), None);
    }

    #[test]
    fn log_key_sorts_newest_first() {
        let a = log_key(b"refs/heads/a", 5);
        let b = log_key(b"refs/heads/a", 6);
        assert!(b < a);
        assert_eq!(split_log_key(&a), Some((&b"refs/heads/a"[..], 5)));
        assert_eq!(split_log_key(b"short"), None);
    }
}
