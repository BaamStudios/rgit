// SPDX-License-Identifier: MIT
//! Combining a new note with an existing one.
//!
//! Observed behaviour of the `git` binary:
//!
//! - `concatenate`: the existing note without its trailing newlines, a
//!   blank line, then the new note unchanged. An empty note on either
//!   side leaves the other one; identical notes stay one.
//! - `cat_sort_uniq`: the non-empty lines of both notes, sorted byte-wise,
//!   duplicates removed, each ending with a newline.

use rgit_hash::ObjectId;
use rgit_object::ObjectKind;
use rgit_odb::ObjectDatabase;

use crate::Error;

/// How a note is combined with an existing note of the same object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combine {
    /// The new note replaces the existing one.
    Overwrite,
    /// The existing note stays.
    Ignore,
    /// The new note is appended after a blank line.
    Concatenate,
    /// The lines of both, sorted and without duplicates.
    CatSortUniq,
}

impl Combine {
    /// The mode named `name` (`notes.rewriteMode`).
    pub fn from_rewrite_mode(name: &str) -> Option<Combine> {
        Some(match name {
            "overwrite" => Combine::Overwrite,
            "ignore" => Combine::Ignore,
            "concatenate" => Combine::Concatenate,
            "cat_sort_uniq" => Combine::CatSortUniq,
            _ => return None,
        })
    }

    /// The note that results from `new` meeting `cur`.
    pub fn apply(
        self,
        odb: &ObjectDatabase,
        cur: ObjectId,
        new: ObjectId,
    ) -> Result<ObjectId, Error> {
        match self {
            Combine::Overwrite => Ok(new),
            Combine::Ignore => Ok(cur),
            Combine::Concatenate => concatenate(odb, cur, new),
            Combine::CatSortUniq => cat_sort_uniq(odb, cur, new),
        }
    }
}

fn read_blob(odb: &ObjectDatabase, id: &ObjectId) -> Result<Vec<u8>, Error> {
    match odb.read(id)? {
        Some(raw) if raw.kind == ObjectKind::Blob => Ok(raw.data),
        _ => Err(Error::BadNote(*id)),
    }
}

/// `new` appended to `cur` after a blank line.
pub fn concatenate(odb: &ObjectDatabase, cur: ObjectId, new: ObjectId) -> Result<ObjectId, Error> {
    if cur == new {
        return Ok(cur);
    }
    let a = read_blob(odb, &cur)?;
    let b = read_blob(odb, &new)?;
    if b.is_empty() {
        return Ok(cur);
    }
    if a.is_empty() {
        return Ok(new);
    }
    let end = a.iter().rposition(|&c| c != b'\n').map_or(0, |p| p + 1);
    let mut out = a[..end].to_vec();
    out.extend_from_slice(b"\n\n");
    out.extend_from_slice(&b);
    Ok(odb.write(ObjectKind::Blob, &out)?)
}

/// The non-empty lines of `cur` and `new`, sorted and unique.
pub fn cat_sort_uniq(
    odb: &ObjectDatabase,
    cur: ObjectId,
    new: ObjectId,
) -> Result<ObjectId, Error> {
    if cur == new {
        return Ok(cur);
    }
    let a = read_blob(odb, &cur)?;
    let b = read_blob(odb, &new)?;
    let mut lines: Vec<&[u8]> = a
        .split(|&c| c == b'\n')
        .chain(b.split(|&c| c == b'\n'))
        .filter(|l| !l.is_empty())
        .collect();
    lines.sort();
    lines.dedup();
    let mut out = Vec::new();
    for l in lines {
        out.extend_from_slice(l);
        out.push(b'\n');
    }
    Ok(odb.write(ObjectKind::Blob, &out)?)
}
