// SPDX-License-Identifier: MIT
//! The bundle header (`Documentation/gitformat-bundle.adoc`):
//!
//! ```text
//! # v3 git bundle
//! @object-format=sha256        (v3 only, any number of capabilities)
//! -<obj-id> <comment>          (prerequisites)
//! <obj-id> <refname>           (references)
//!                              (empty line, then the pack)
//! ```
//!
//! [`read_header`] stops after the empty line (or at the end of the
//! input), so the reader is left at the start of the pack.

use std::io::{self, BufRead, Write};

use rgit_hash::{HashKind, ObjectId};

use crate::filter::FilterSpec;
use crate::messages;

const V2_SIGNATURE: &[u8] = b"# v2 git bundle\n";
const V3_SIGNATURE: &[u8] = b"# v3 git bundle\n";

/// A prerequisite: an object the reader must have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prerequisite {
    pub id: ObjectId,
    /// Free text after the id (git writes the commit's subject).
    pub comment: Vec<u8>,
}

/// A reference recorded in the bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub id: ObjectId,
    pub name: Vec<u8>,
}

/// A parsed bundle header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// 2 or 3.
    pub version: u32,
    /// `@object-format` (SHA-1 when not given).
    pub hash: HashKind,
    /// `@filter`.
    pub filter: Option<FilterSpec>,
    pub prerequisites: Vec<Prerequisite>,
    pub references: Vec<Reference>,
}

/// Why a header could not be read.
#[derive(Debug)]
pub enum HeaderError {
    /// The first line is not a bundle signature (see
    /// [`messages::not_a_bundle`]).
    NotABundle,
    /// A line that is reported with `error:`.
    Error(String),
    /// A problem reported with `fatal:` (a bad filter spec).
    Fatal(String),
    /// Reading failed.
    Io(io::Error),
}

impl From<io::Error> for HeaderError {
    fn from(e: io::Error) -> Self {
        HeaderError::Io(e)
    }
}

impl Header {
    /// An empty header of the given version and hash.
    pub fn new(version: u32, hash: HashKind) -> Header {
        Header {
            version,
            hash,
            filter: None,
            prerequisites: Vec::new(),
            references: Vec::new(),
        }
    }

    /// Write the header including the empty line that ends it.
    pub fn write(&self, out: &mut dyn Write) -> io::Result<()> {
        if self.version >= 3 {
            out.write_all(V3_SIGNATURE)?;
            writeln!(out, "@object-format={}", self.hash.name())?;
            if let Some(f) = &self.filter {
                writeln!(out, "@filter={}", f.as_str())?;
            }
        } else {
            out.write_all(V2_SIGNATURE)?;
        }
        for p in &self.prerequisites {
            write!(out, "-{} ", p.id.to_hex())?;
            out.write_all(&p.comment)?;
            out.write_all(b"\n")?;
        }
        for r in &self.references {
            write!(out, "{} ", r.id.to_hex())?;
            out.write_all(&r.name)?;
            out.write_all(b"\n")?;
        }
        out.write_all(b"\n")
    }
}

/// Whether `data` starts with a bundle signature.
pub fn has_signature(data: &[u8]) -> bool {
    data.starts_with(V2_SIGNATURE) || data.starts_with(V3_SIGNATURE)
}

/// Read one line including its `\n` (none at the end of the input).
fn read_line(r: &mut dyn BufRead, buf: &mut Vec<u8>) -> io::Result<bool> {
    buf.clear();
    let n = r.read_until(b'\n', buf)?;
    Ok(n > 0)
}

/// Read a bundle header from `r`, leaving it at the start of the pack.
pub fn read_header(r: &mut dyn BufRead) -> Result<Header, HeaderError> {
    let mut line = Vec::new();
    if !read_line(r, &mut line)? {
        return Err(HeaderError::NotABundle);
    }
    let version = if line == V2_SIGNATURE {
        2
    } else if line == V3_SIGNATURE {
        3
    } else {
        return Err(HeaderError::NotABundle);
    };
    let mut header = Header::new(version, HashKind::Sha1);
    // Lines are kept until the capabilities (which may name the hash)
    // have been seen.
    let mut pending: Vec<Vec<u8>> = Vec::new();
    while read_line(r, &mut line)? {
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.is_empty() {
            break;
        }
        if version >= 3 && line[0] == b'@' {
            let cap = String::from_utf8_lossy(&line[1..]).into_owned();
            let (key, value) = match cap.split_once('=') {
                Some((k, v)) => (k, Some(v)),
                None => (cap.as_str(), None),
            };
            match (key, value) {
                ("object-format", Some(v)) => {
                    header.hash = HashKind::from_name(v)
                        .ok_or_else(|| HeaderError::Error(messages::unrecognized_hash(v)))?;
                }
                ("filter", Some(v)) => {
                    header.filter = Some(FilterSpec::parse(v).map_err(HeaderError::Fatal)?);
                }
                _ => return Err(HeaderError::Error(messages::unknown_capability(&cap))),
            }
            continue;
        }
        pending.push(line.clone());
    }
    for line in pending {
        let bad = || {
            HeaderError::Error(messages::unrecognized_header(&String::from_utf8_lossy(
                &line,
            )))
        };
        let (prereq, rest) = match line.strip_prefix(b"-") {
            Some(rest) => (true, rest),
            None => (false, &line[..]),
        };
        let hex_len = header.hash.hex_len();
        if rest.len() < hex_len {
            return Err(bad());
        }
        let id = ObjectId::from_hex(header.hash, &rest[..hex_len]).map_err(|_| bad())?;
        let tail = &rest[hex_len..];
        if prereq {
            let comment = match tail.first() {
                None => Vec::new(),
                Some(b' ') => tail[1..].to_vec(),
                Some(_) => return Err(bad()),
            };
            header.prerequisites.push(Prerequisite { id, comment });
        } else {
            match tail.first() {
                Some(b' ') if tail.len() > 1 => header.references.push(Reference {
                    id,
                    name: tail[1..].to_vec(),
                }),
                _ => return Err(bad()),
            }
        }
    }
    Ok(header)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &[u8]) -> Result<Header, HeaderError> {
        read_header(&mut &s[..])
    }

    #[test]
    fn v2_round_trip() {
        let a = "13ab4201b4597f4f003fc172c87a4fe8495248f3";
        let b = "f1b06e978718cac1125ecfa36f0f71468b44b91f";
        let text = format!("# v2 git bundle\n-{a} c2\n{b} refs/heads/master\n\nPACK");
        let mut r = text.as_bytes();
        let h = read_header(&mut r).unwrap();
        assert_eq!(r, b"PACK");
        assert_eq!(h.version, 2);
        assert_eq!(h.prerequisites[0].comment, b"c2");
        assert_eq!(h.references[0].name, b"refs/heads/master");
        let mut out = Vec::new();
        h.write(&mut out).unwrap();
        assert_eq!(out, text.as_bytes()[..text.len() - 4]);
    }

    #[test]
    fn v3_capabilities() {
        let h = parse(b"# v3 git bundle\n@object-format=sha256\n@filter=blob:none\n\n").unwrap();
        assert_eq!(h.hash, HashKind::Sha256);
        assert_eq!(h.filter.unwrap().as_str(), "blob:none");
        match parse(b"# v3 git bundle\n@unknown=silly\n\n") {
            Err(HeaderError::Error(m)) => assert_eq!(m, "unknown capability 'unknown=silly'"),
            other => panic!("{other:?}"),
        }
        match parse(b"# v2 git bundle\n@object-format=sha1\n\n") {
            Err(HeaderError::Error(m)) => {
                assert_eq!(m, "unrecognized header: @object-format=sha1 (19)")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn not_a_bundle() {
        assert!(matches!(parse(b""), Err(HeaderError::NotABundle)));
        assert!(matches!(
            parse(b"# v2 git bundle\r\n\n"),
            Err(HeaderError::NotABundle)
        ));
    }
}
