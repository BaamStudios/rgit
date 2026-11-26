// SPDX-License-Identifier: MIT
//! Notes trees written by `notemodify`: a note for object `<hex>` is the
//! blob at `<hex>` split into two-digit directories ("fan-out") as the
//! number of notes grows. `N` commands work at the fan-out the commit
//! started with; when the number of notes at the end of the commit calls
//! for another fan-out, every note is moved there. Other files in the
//! tree stay where they are.

use rgit_hash::{HashKind, ObjectId};

use super::tree::{self, Found, Loader, Tree};
use crate::Die;

/// The number of two-digit directory levels for `count` notes.
pub fn fanout_for(count: usize) -> usize {
    let mut fanout = 0;
    let mut n = count;
    while n > 0xff {
        n >>= 8;
        fanout += 1;
    }
    fanout
}

/// The components of the path of the note for `hex` at `fanout`.
pub fn note_path(hex: &str, fanout: usize) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let b = hex.as_bytes();
    for i in 0..fanout {
        out.push(b[2 * i..2 * i + 2].to_vec());
    }
    out.push(b[2 * fanout..].to_vec());
    out
}

/// A note in the tree: the annotated object's hex id, its fan-out and the
/// entry.
struct Note {
    hex: String,
    fanout: usize,
    mode: u32,
    oid: ObjectId,
}

/// Whether `path` (with `/`) names a note: two-digit hex directories and
/// a hex rest, together a full object id.
fn as_note(hash: HashKind, path: &[u8]) -> Option<(String, usize)> {
    let parts: Vec<&[u8]> = path.split(|&c| c == b'/').collect();
    let (last, dirs) = parts.split_last()?;
    if dirs.iter().any(|d| d.len() != 2) {
        return None;
    }
    let hex: Vec<u8> = parts.concat();
    if hex.len() != hash.hex_len() || !hex.iter().all(u8::is_ascii_hexdigit) || last.is_empty() {
        return None;
    }
    Some((String::from_utf8(hex).ok()?.to_ascii_lowercase(), dirs.len()))
}

fn collect(hash: HashKind, root: &mut Tree, loader: &mut dyn Loader) -> Result<Vec<Note>, Die> {
    let mut notes = Vec::new();
    let mut prefix = Vec::new();
    tree::walk(hash, root, &mut prefix, loader, &mut |path, mode, oid| {
        if let Some((hex, fanout)) = as_note(hash, path) {
            notes.push(Note {
                hex,
                fanout,
                mode,
                oid: *oid,
            });
        }
    })?;
    Ok(notes)
}

fn comps(path: &[Vec<u8>]) -> Vec<&[u8]> {
    path.iter().map(Vec::as_slice).collect()
}

/// The number of notes in the tree.
pub fn count(hash: HashKind, root: &mut Tree, loader: &mut dyn Loader) -> Result<usize, Die> {
    Ok(collect(hash, root, loader)?.len())
}

/// Remove the note for `hex` at `fanout`.
pub fn remove(
    hash: HashKind,
    root: &mut Tree,
    hex: &str,
    fanout: usize,
    loader: &mut dyn Loader,
) -> Result<(), Die> {
    let path = note_path(hex, fanout);
    tree::delete(hash, root, &comps(&path), loader)?;
    Ok(())
}

/// Add the note `blob` for `hex` at `fanout`.
pub fn add(
    hash: HashKind,
    root: &mut Tree,
    hex: &str,
    blob: ObjectId,
    fanout: usize,
    loader: &mut dyn Loader,
) -> Result<(), Die> {
    let path = note_path(hex, fanout);
    tree::set(
        hash,
        root,
        &comps(&path),
        Found {
            mode: 0o100644,
            oid: blob,
            sub: None,
        },
        loader,
    )
}

/// Move all notes to `fanout`.
pub fn relayout(
    hash: HashKind,
    root: &mut Tree,
    fanout: usize,
    loader: &mut dyn Loader,
) -> Result<(), Die> {
    let notes = collect(hash, root, loader)?;
    for n in notes.iter().filter(|n| n.fanout != fanout) {
        let old = note_path(&n.hex, n.fanout);
        tree::delete(hash, root, &comps(&old), loader)?;
    }
    for n in notes.iter().filter(|n| n.fanout != fanout) {
        let new = note_path(&n.hex, fanout);
        tree::set(
            hash,
            root,
            &comps(&new),
            Found {
                mode: n.mode,
                oid: n.oid,
                sub: None,
            },
            loader,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fanout() {
        assert_eq!(fanout_for(0), 0);
        assert_eq!(fanout_for(255), 0);
        assert_eq!(fanout_for(256), 1);
        assert_eq!(fanout_for(65535), 1);
        assert_eq!(fanout_for(65536), 2);
    }

    #[test]
    fn notes_are_recognised() {
        let h = HashKind::Sha1;
        let hex = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(as_note(h, hex.as_bytes()), Some((hex.to_string(), 0)));
        let split = format!("01/{}", &hex[2..]);
        assert_eq!(as_note(h, split.as_bytes()), Some((hex.to_string(), 1)));
        assert_eq!(as_note(h, b"de/adbeef"), None);
        assert_eq!(as_note(h, b"foobar/non-note.txt"), None);
    }
}
