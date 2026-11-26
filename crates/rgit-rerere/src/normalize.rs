// SPDX-License-Identifier: MIT
//! Conflict normalisation and the conflict id
//! (Documentation/technical/rerere.adoc; details checked against the `git`
//! binary).
//!
//! A conflict starts with a line of `<<<<<<<` followed by a space, may have
//! a common-ancestor part from a `|||||||` line, switches to the other side
//! at a `=======` line and ends with a `>>>>>>>` line followed by a space.
//! `|||||||` and `=======` only need to be followed by white space (a line
//! end counts). A marker is exactly `marker_size` characters long: one
//! character more makes it an ordinary line.
//!
//! The normalised image drops the labels and the common-ancestor part and
//! puts the two sides of every conflict in byte order. Conflicts nested in
//! a side are normalised the same way, recursively, and stay in the side as
//! text. The conflict id hashes, for each outermost conflict in file order,
//! the first side, a NUL, the second side and a NUL.

use rgit_hash::{HashKind, Hasher, ObjectId};

/// A file with its conflicts normalised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Normalized {
    /// The file with normalised conflicts (the `preimage`).
    pub image: Vec<u8>,
    /// Number of outermost conflicts.
    pub hunks: usize,
    /// The conflict id; `None` without conflicts.
    pub id: Option<ObjectId>,
}

/// The conflict markers are unbalanced or out of order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseError;

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("could not parse conflict hunks")
    }
}

impl std::error::Error for ParseError {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Marker {
    Begin,
    Base,
    Middle,
    End,
}

fn marker(line: &[u8], size: usize) -> Option<Marker> {
    let first = *line.first()?;
    let (kind, want_space) = match first {
        b'<' => (Marker::Begin, true),
        b'|' => (Marker::Base, false),
        b'=' => (Marker::Middle, false),
        b'>' => (Marker::End, true),
        _ => return None,
    };
    if line.len() <= size || line[..size].iter().any(|&c| c != first) {
        return None;
    }
    let next = line[size];
    let ok = if want_space {
        next == b' '
    } else {
        matches!(next, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
    };
    ok.then_some(kind)
}

fn lines(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    data.split_inclusive(|&b| b == b'\n')
}

fn push_marker(out: &mut Vec<u8>, c: u8, size: usize) {
    out.extend(std::iter::repeat_n(c, size));
    out.push(b'\n');
}

/// Reads one conflict after its `<<<<<<<` line; returns the two sides in
/// order.
fn conflict<'a, I: Iterator<Item = &'a [u8]>>(
    it: &mut I,
    size: usize,
) -> Result<(Vec<u8>, Vec<u8>), ParseError> {
    #[derive(PartialEq)]
    enum Part {
        One,
        Base,
        Two,
    }
    let mut part = Part::One;
    let mut one = Vec::new();
    let mut two = Vec::new();
    loop {
        let line = it.next().ok_or(ParseError)?;
        match marker(line, size) {
            Some(Marker::Begin) => {
                let (a, b) = conflict(it, size)?;
                let target = match part {
                    Part::One => &mut one,
                    Part::Two => &mut two,
                    Part::Base => continue,
                };
                write_conflict(target, &a, &b, size);
            }
            Some(Marker::Base) => {
                if part != Part::One {
                    return Err(ParseError);
                }
                part = Part::Base;
            }
            Some(Marker::Middle) => {
                if part == Part::Two {
                    return Err(ParseError);
                }
                part = Part::Two;
            }
            Some(Marker::End) => {
                if part != Part::Two {
                    return Err(ParseError);
                }
                break;
            }
            None => match part {
                Part::One => one.extend_from_slice(line),
                Part::Two => two.extend_from_slice(line),
                Part::Base => {}
            },
        }
    }
    if one > two {
        std::mem::swap(&mut one, &mut two);
    }
    Ok((one, two))
}

fn write_conflict(out: &mut Vec<u8>, one: &[u8], two: &[u8], size: usize) {
    push_marker(out, b'<', size);
    out.extend_from_slice(one);
    push_marker(out, b'=', size);
    out.extend_from_slice(two);
    push_marker(out, b'>', size);
}

/// Normalises the conflicts in `data` (markers of `marker_size`
/// characters) and computes the conflict id with `kind`.
///
/// # Errors
/// Unbalanced or misplaced conflict markers.
pub fn normalize(
    data: &[u8],
    marker_size: usize,
    kind: HashKind,
) -> Result<Normalized, ParseError> {
    let size = if marker_size == 0 {
        rgit_diff::merge::DEFAULT_MARKER_SIZE
    } else {
        marker_size
    };
    let mut image = Vec::with_capacity(data.len());
    let mut hasher = Hasher::new(kind);
    let mut hunks = 0;
    let mut it = lines(data);
    while let Some(line) = it.next() {
        if marker(line, size) == Some(Marker::Begin) {
            let (one, two) = conflict(&mut it, size)?;
            hasher.update(&one);
            hasher.update(b"\0");
            hasher.update(&two);
            hasher.update(b"\0");
            write_conflict(&mut image, &one, &two, size);
            hunks += 1;
        } else {
            image.extend_from_slice(line);
        }
    }
    let id = if hunks > 0 {
        // A collision in a conflict id is harmless: it only names a
        // directory.
        Some(match hasher.finalize() {
            Ok(id) => id,
            Err(e) => e.id,
        })
    } else {
        None
    };
    Ok(Normalized { image, hunks, id })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(data: &[u8]) -> String {
        normalize(data, 7, HashKind::Sha1)
            .unwrap()
            .id
            .unwrap()
            .to_hex()
    }

    #[test]
    fn simple_conflicts_sorted_and_hashed() {
        let f = b"a\n<<<<<<< HEAD\nB2\n=======\nB1\n>>>>>>> x\nc\n<<<<<<< HEAD\nA0\n=======\nG1\n>>>>>>> x\nh\n";
        let n = normalize(f, 7, HashKind::Sha1).unwrap();
        assert_eq!(n.hunks, 2);
        assert_eq!(
            n.image,
            b"a\n<<<<<<<\nB1\n=======\nB2\n>>>>>>>\nc\n<<<<<<<\nA0\n=======\nG1\n>>>>>>>\nh\n"
        );
        // Checked against git.
        assert_eq!(hex(f), "595c678832d5f7b9c78632d1d1871fee1a9268ea");
    }

    #[test]
    fn nested_and_diff3() {
        let a = b"top\n<<<<<<< HEAD\n1\n=======\n<<<<<<< HEAD\n3\n=======\n2\n>>>>>>> b2\n>>>>>>> b3~\nbottom\n";
        let b =
            b"<<<<<<< a\n1\n=======\n<<<<<<< c\n3\n|||||||\n0\n=======\n2\n>>>>>>> d\n>>>>>>> b\n";
        assert_eq!(hex(a), "19807c4edbd36d0a514cbb9bc672ba05ff35e7bf");
        assert_eq!(hex(b), "19807c4edbd36d0a514cbb9bc672ba05ff35e7bf");
    }

    #[test]
    fn marker_rules() {
        let n = |d: &[u8]| normalize(d, 7, HashKind::Sha1);
        // Begin and end need a space; a longer marker is text.
        assert_eq!(n(b"<<<<<<<\n1\n=======\n2\n>>>>>>>\n").unwrap().hunks, 0);
        assert_eq!(
            n(b"<<<<<<<\tx\n1\n=======\n2\n>>>>>>> b\n").unwrap().hunks,
            0
        );
        assert_eq!(
            n(b"<<<<<<<< a\n1\n=======\n2\n>>>>>>> b\n").unwrap().hunks,
            0
        );
        assert!(n(b"<<<<<<< a\n1\n=======\n2\n>>>>>>>\n").is_err());
        assert!(n(b"<<<<<<< a\n").is_err());
        assert!(n(b"<<<<<<< a\n1\n=======\n2\n=======\n3\n>>>>>>> b\n").is_err());
        assert!(n(b"<<<<<<< a\n1\n=======\n2\n||||||| a\n3\n>>>>>>> b\n").is_err());
        // Middle markers take any white space; the last line needs no end.
        let ok = n(b"<<<<<<< a\r\n1\r\n=======\r\n2\r\n>>>>>>> b").unwrap();
        assert_eq!(ok.image, b"<<<<<<<\n1\r\n=======\n2\r\n>>>>>>>\n");
        assert_eq!(
            hex(b"<<<<<<< a\n1\n======= x\n2\n>>>>>>> b\n"),
            "926133105e31f617cc54391d3e20dbea186fdfea"
        );
        // Markers outside a conflict are text.
        let t = n(b"<<<<<<< a\n1\n=======\n2\n>>>>>>> b\n=======\n>>>>>>> b\n|||||||\n").unwrap();
        assert_eq!(
            t.image,
            b"<<<<<<<\n1\n=======\n2\n>>>>>>>\n=======\n>>>>>>> b\n|||||||\n"
        );
    }

    #[test]
    fn marker_size() {
        let f = b"1\n<<<<<<<<<< HEAD\nY\n|||||||||| base\n3\n==========\nX\n>>>>>>>>>> x\n";
        let n = normalize(f, 10, HashKind::Sha1).unwrap();
        assert_eq!(n.image, b"1\n<<<<<<<<<<\nX\n==========\nY\n>>>>>>>>>>\n");
    }
}
