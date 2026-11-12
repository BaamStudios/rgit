// SPDX-License-Identifier: MIT
//! The `packed-refs` file.
//!
//! Format (as written by the reference `git`):
//!
//! ```text
//! # pack-refs with: peeled fully-peeled sorted
//! <oid> SP <refname> LF
//! ^<peeled-oid> LF          (optional, after a reference to a tag)
//! ```
//!
//! The header line is optional and lists *traits*:
//!
//! * `peeled`: every reference below `refs/tags/` that points to a tag has
//!   a `^` line, so a tag reference without one does not peel.
//! * `fully-peeled`: the same for every reference, not only tags.
//! * `sorted`: the entries are sorted by name. Without it the reader sorts.

use rgit_hash::{HashKind, ObjectId};

use crate::target::Peeled;

/// Header git writes when it rewrites `packed-refs`.
pub const HEADER: &str = crate::messages::PACKED_REFS_HEADER;

/// One entry of `packed-refs`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackedRef {
    /// Full reference name.
    pub name: String,
    /// The object the reference points to.
    pub oid: ObjectId,
    /// Peeled value from a `^` line, if any.
    pub peeled: Option<ObjectId>,
}

/// Traits named in the header line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Traits {
    /// `peeled`
    pub peeled: bool,
    /// `fully-peeled`
    pub fully_peeled: bool,
    /// `sorted`
    pub sorted: bool,
}

/// Error while parsing `packed-refs`. The store adds the file name when it
/// reports it ([`crate::messages::packed_error`]).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PackedError {
    /// A line that is neither a reference nor a peeled line.
    #[error("unexpected line: {0}")]
    UnexpectedLine(String),
    /// The last line has no terminating newline.
    #[error("unterminated line: {0}")]
    UnterminatedLine(String),
}

/// A parsed `packed-refs` file, entries sorted by name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackedRefs {
    traits: Traits,
    refs: Vec<PackedRef>,
}

impl PackedRefs {
    /// Parses the bytes of a `packed-refs` file.
    pub fn parse(data: &[u8], kind: HashKind) -> Result<Self, PackedError> {
        let mut traits = Traits::default();
        let mut refs: Vec<PackedRef> = Vec::new();
        let mut rest = data;
        let mut first = true;
        while !rest.is_empty() {
            let Some(nl) = rest.iter().position(|&b| b == b'\n') else {
                return Err(PackedError::UnterminatedLine(lossy(rest)));
            };
            let line = &rest[..nl];
            rest = &rest[nl + 1..];
            if first {
                first = false;
                if let Some(t) = line.strip_prefix(b"# pack-refs with:") {
                    for word in t.split(|&b| b == b' ').filter(|w| !w.is_empty()) {
                        match word {
                            b"peeled" => traits.peeled = true,
                            b"fully-peeled" => traits.fully_peeled = true,
                            b"sorted" => traits.sorted = true,
                            _ => {}
                        }
                    }
                    continue;
                }
            }
            if let Some(hex) = line.strip_prefix(b"^") {
                let peeled = ObjectId::from_hex(kind, hex)
                    .map_err(|_| PackedError::UnexpectedLine(lossy(line)))?;
                match refs.last_mut() {
                    Some(last) if last.peeled.is_none() => last.peeled = Some(peeled),
                    _ => return Err(PackedError::UnexpectedLine(lossy(line))),
                }
                continue;
            }
            let hex_len = kind.hex_len();
            let ok = line.len() > hex_len + 1 && line[hex_len] == b' ';
            let parsed = ok
                .then(|| {
                    let oid = ObjectId::from_hex(kind, &line[..hex_len]).ok()?;
                    let name = std::str::from_utf8(&line[hex_len + 1..]).ok()?;
                    Some((oid, name.to_owned()))
                })
                .flatten();
            let Some((oid, name)) = parsed else {
                return Err(PackedError::UnexpectedLine(lossy(line)));
            };
            refs.push(PackedRef {
                name,
                oid,
                peeled: None,
            });
        }
        let is_sorted = traits.sorted || refs.windows(2).all(|w| w[0].name <= w[1].name);
        if !is_sorted {
            refs.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        }
        // Duplicate names are kept: the reference git lists every one.
        Ok(Self { traits, refs })
    }

    /// Builds a file from entries (sorted here).
    pub fn from_refs(mut refs: Vec<PackedRef>) -> Self {
        refs.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        refs.dedup_by(|later, earlier| later.name == earlier.name);
        Self {
            traits: Traits {
                peeled: true,
                fully_peeled: true,
                sorted: true,
            },
            refs,
        }
    }

    /// Header traits as read.
    pub fn traits(&self) -> Traits {
        self.traits
    }

    /// All entries, sorted by name.
    pub fn refs(&self) -> &[PackedRef] {
        &self.refs
    }

    /// Whether there are no entries.
    pub fn is_empty(&self) -> bool {
        self.refs.is_empty()
    }

    fn position(&self, name: &str) -> Result<usize, usize> {
        self.refs
            .binary_search_by(|r| r.name.as_bytes().cmp(name.as_bytes()))
    }

    /// Looks up one reference.
    pub fn find(&self, name: &str) -> Option<&PackedRef> {
        self.position(name).ok().map(|i| &self.refs[i])
    }

    /// Entries whose name starts with `prefix`.
    pub fn with_prefix<'a>(&'a self, prefix: &'a str) -> impl Iterator<Item = &'a PackedRef> + 'a {
        let start = self.position(prefix).unwrap_or_else(|i| i);
        self.refs[start..]
            .iter()
            .take_while(move |r| r.name.starts_with(prefix))
    }

    /// What is known about the peeled value of `entry` given the traits.
    pub fn peeled(&self, entry: &PackedRef) -> Peeled {
        match entry.peeled {
            Some(p) => Peeled::Tag(p),
            None if self.traits.fully_peeled => Peeled::NotTag,
            None if self.traits.peeled && entry.name.starts_with("refs/tags/") => Peeled::NotTag,
            None => Peeled::Unknown,
        }
    }

    /// Removes `name`; returns whether it was present.
    pub fn remove(&mut self, name: &str) -> bool {
        match self.position(name) {
            Ok(i) => {
                self.refs.remove(i);
                true
            }
            Err(_) => false,
        }
    }

    /// Inserts or replaces an entry.
    pub fn upsert(&mut self, entry: PackedRef) {
        match self.position(&entry.name) {
            Ok(i) => self.refs[i] = entry,
            Err(i) => self.refs.insert(i, entry),
        }
    }

    /// Serializes as git writes the file: the full header, then every entry
    /// with its peeled line if one is known.
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER.len() + self.refs.len() * 64);
        out.extend_from_slice(HEADER.as_bytes());
        for r in &self.refs {
            out.extend_from_slice(r.oid.to_hex().as_bytes());
            out.push(b' ');
            out.extend_from_slice(r.name.as_bytes());
            out.push(b'\n');
            if let Some(p) = r.peeled {
                out.push(b'^');
                out.extend_from_slice(p.to_hex().as_bytes());
                out.push(b'\n');
            }
        }
        out
    }
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "b2e6e911d0750ef5d57039fc461069c4a5277897";
    const T: &str = "8b21c372ce20790e9d010c31775b2617e31c4bd1";

    #[test]
    fn parse_and_write_back() {
        let text = format!(
            "{HEADER}{A} refs/heads/master\n{A} refs/tags/lw\n{T} refs/tags/v1\n^{A}\n{A} refs/x/y\n"
        );
        let p = PackedRefs::parse(text.as_bytes(), HashKind::Sha1).unwrap();
        assert!(p.traits().fully_peeled && p.traits().sorted && p.traits().peeled);
        assert_eq!(p.refs().len(), 4);
        let v1 = p.find("refs/tags/v1").unwrap();
        assert_eq!(v1.peeled.unwrap().to_hex(), A);
        assert_eq!(p.peeled(p.find("refs/tags/lw").unwrap()), Peeled::NotTag);
        assert_eq!(p.serialize(), text.as_bytes());
        assert_eq!(p.with_prefix("refs/tags/").count(), 2);
    }

    #[test]
    fn unsorted_without_header() {
        let text = format!("{A} refs/heads/b\n{A} refs/heads/a\n");
        let p = PackedRefs::parse(text.as_bytes(), HashKind::Sha1).unwrap();
        assert_eq!(p.refs()[0].name, "refs/heads/a");
        assert_eq!(p.peeled(&p.refs()[0]), Peeled::Unknown);
    }

    #[test]
    fn errors() {
        assert!(PackedRefs::parse(b"garbage\n", HashKind::Sha1).is_err());
        assert!(PackedRefs::parse(format!("{A} refs/x").as_bytes(), HashKind::Sha1).is_err());
        assert!(PackedRefs::parse(format!("^{A}\n").as_bytes(), HashKind::Sha1).is_err());
    }
}
