// SPDX-License-Identifier: MIT
//! Capability lists (`Documentation/gitprotocol-capabilities.adoc`,
//! `gitprotocol-v2.adoc`).
//!
//! - Protocol v0/v1: the first ref line of the advertisement carries the
//!   capabilities after a NUL byte, separated by spaces, each `name` or
//!   `name=value` (`symref=HEAD:refs/heads/main` may repeat). An empty
//!   repository advertises `<zero-id> capabilities^{}\0<caps>`. v1 starts
//!   with a `version 1` line.
//! - Protocol v2: `version 2`, then one capability per pkt-line as
//!   `key[=value]` (the value may contain spaces, e.g.
//!   `fetch=shallow wait-for-done`), then a flush packet.

use std::io::Read;

use crate::error::{Error, Result};
use crate::pktline::{Packet, Reader, Writer};

/// Wire protocol version announced by the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProtocolVersion {
    /// No version line.
    V0,
    /// `version 1`.
    V1,
    /// `version 2`.
    V2,
}

impl ProtocolVersion {
    /// Detect the version from the first packet of a server response and
    /// consume the `version N` line if there is one. The first packet of a
    /// v0 response is left unread.
    pub fn read<R: Read>(reader: &mut Reader<R>) -> Result<ProtocolVersion> {
        let version = match reader.peek()? {
            Some(Packet::Data(b"version 1")) => ProtocolVersion::V1,
            Some(Packet::Data(b"version 2")) => ProtocolVersion::V2,
            _ => return Ok(ProtocolVersion::V0),
        };
        reader.read()?;
        Ok(version)
    }
}

/// One capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability {
    /// The name (v2: key).
    pub name: String,
    /// The value after `=`, if any.
    pub value: Option<String>,
}

impl Capability {
    /// Parse `name` or `name=value`.
    pub fn parse(s: &str) -> Capability {
        match s.split_once('=') {
            Some((n, v)) => Capability {
                name: n.to_owned(),
                value: Some(v.to_owned()),
            },
            None => Capability {
                name: s.to_owned(),
                value: None,
            },
        }
    }

    /// `name` or `name=value`.
    pub fn to_wire(&self) -> String {
        match &self.value {
            Some(v) => format!("{}={v}", self.name),
            None => self.name.clone(),
        }
    }
}

/// An ordered capability list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capabilities {
    list: Vec<Capability>,
}

impl Capabilities {
    /// An empty list.
    pub fn new() -> Capabilities {
        Capabilities::default()
    }

    /// Parse a v0/v1 capability string (the part after the NUL).
    pub fn parse_v0(caps: &[u8]) -> Capabilities {
        let text = String::from_utf8_lossy(caps);
        Capabilities {
            list: text
                .split(' ')
                .filter(|s| !s.is_empty())
                .map(Capability::parse)
                .collect(),
        }
    }

    /// Read a v2 capability advertisement up to and including its flush
    /// packet. The `version 2` line must already be consumed (see
    /// [`ProtocolVersion::read`]).
    pub fn read_v2<R: Read>(reader: &mut Reader<R>) -> Result<Capabilities> {
        let mut caps = Capabilities::new();
        loop {
            match reader.read()? {
                Packet::Flush => return Ok(caps),
                Packet::Data(line) => caps
                    .list
                    .push(Capability::parse(&String::from_utf8_lossy(line))),
                other => return Err(other.unexpected()),
            }
        }
    }

    /// Read `version 2` and the capability advertisement.
    pub fn read_v2_advertisement<R: Read>(reader: &mut Reader<R>) -> Result<Capabilities> {
        match ProtocolVersion::read(reader)? {
            ProtocolVersion::V2 => Capabilities::read_v2(reader),
            _ => Err(Error::ExpectedVersion2),
        }
    }

    /// Write a v2 capability advertisement: `version 2`, the list, flush.
    pub fn write_v2<W: std::io::Write>(&self, writer: &mut Writer<W>) -> Result<()> {
        writer.write_line("version 2")?;
        for c in &self.list {
            writer.write_line(&c.to_wire())?;
        }
        writer.flush_pkt()
    }

    /// Append a capability.
    pub fn push(&mut self, name: impl Into<String>, value: Option<String>) {
        self.list.push(Capability {
            name: name.into(),
            value,
        });
    }

    /// All capabilities in order.
    pub fn iter(&self) -> impl Iterator<Item = &Capability> {
        self.list.iter()
    }

    /// Number of capabilities.
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// Whether the list is empty.
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Whether `name` is present (with or without value).
    pub fn contains(&self, name: &str) -> bool {
        self.list.iter().any(|c| c.name == name)
    }

    /// The value of the first `name`; `None` if absent or without value.
    pub fn value(&self, name: &str) -> Option<&str> {
        self.list
            .iter()
            .find(|c| c.name == name)
            .and_then(|c| c.value.as_deref())
    }

    /// The values of all `name=value` entries.
    pub fn values<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.list
            .iter()
            .filter(move |c| c.name == name)
            .filter_map(|c| c.value.as_deref())
    }

    /// v2: whether the value of `name` lists `feature` (space separated),
    /// e.g. `fetch=shallow wait-for-done`.
    pub fn has_feature(&self, name: &str, feature: &str) -> bool {
        self.value(name)
            .is_some_and(|v| v.split(' ').any(|f| f == feature))
    }

    /// `symref=<from>:<to>` pairs.
    pub fn symrefs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.values("symref").filter_map(|v| v.split_once(':'))
    }

    /// `agent=`.
    pub fn agent(&self) -> Option<&str> {
        self.value("agent")
    }

    /// `object-format=` (the first one: the format of the advertisement).
    pub fn object_format(&self) -> Option<&str> {
        self.value("object-format")
    }

    /// `session-id=`.
    pub fn session_id(&self) -> Option<&str> {
        self.value("session-id")
    }

    /// The v0 wire form: entries joined by spaces.
    pub fn to_v0_string(&self) -> String {
        self.list
            .iter()
            .map(Capability::to_wire)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Split a v0/v1 ref advertisement line at the NUL: the `<oid> <ref>` part
/// and the capability list, if present.
pub fn split_ref_line(line: &[u8]) -> (&[u8], Option<Capabilities>) {
    match line.iter().position(|&b| b == 0) {
        Some(i) => (&line[..i], Some(Capabilities::parse_v0(&line[i + 1..]))),
        None => (line, None),
    }
}

/// The ref name used when a repository without refs advertises its
/// capabilities.
pub const NO_REFS_NAME: &[u8] = b"capabilities^{}";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pktline::encode;

    const FIRST: &[u8] = b"8a9dddb74223866816bd2d1f14e99fc512bfcd62 HEAD\0multi_ack thin-pack \
side-band side-band-64k ofs-delta shallow deepen-since deepen-not deepen-relative no-progress \
include-tag multi_ack_detailed symref=HEAD:refs/heads/main object-format=sha1 agent=git/2.56.0-Linux";

    #[test]
    fn v0_first_line() {
        let (refpart, caps) = split_ref_line(FIRST);
        assert_eq!(refpart, b"8a9dddb74223866816bd2d1f14e99fc512bfcd62 HEAD");
        let caps = caps.unwrap();
        assert!(caps.contains("side-band-64k"));
        assert!(caps.contains("multi_ack_detailed"));
        assert!(!caps.contains("multi"));
        assert_eq!(caps.value("side-band"), None);
        assert_eq!(caps.agent(), Some("git/2.56.0-Linux"));
        assert_eq!(caps.object_format(), Some("sha1"));
        assert_eq!(caps.session_id(), None);
        assert_eq!(
            caps.symrefs().collect::<Vec<_>>(),
            vec![("HEAD", "refs/heads/main")]
        );
        assert_eq!(caps.to_v0_string().as_bytes(), &FIRST[46..]);
        let (_, none) = split_ref_line(b"8a9d refs/heads/main");
        assert!(none.is_none());
    }

    #[test]
    fn v0_repeated_and_session() {
        let caps = Capabilities::parse_v0(
            b"symref=HEAD:refs/heads/a symref=refs/remotes/o/HEAD:refs/remotes/o/a  session-id=abc object-format=sha256 object-format=sha1",
        );
        assert_eq!(caps.symrefs().count(), 2);
        assert_eq!(caps.session_id(), Some("abc"));
        assert_eq!(caps.object_format(), Some("sha256"));
        assert_eq!(caps.values("object-format").count(), 2);
        assert_eq!(caps.len(), 5);
    }

    #[test]
    fn no_refs_line() {
        let line =
            b"0000000000000000000000000000000000000000 capabilities^{}\0report-status delete-refs";
        let (refpart, caps) = split_ref_line(line);
        assert!(refpart.ends_with(NO_REFS_NAME));
        assert!(caps.unwrap().contains("delete-refs"));
    }

    fn stream(lines: &[&str]) -> Vec<u8> {
        let mut s = Vec::new();
        for l in lines {
            if l.is_empty() {
                s.extend_from_slice(b"0000");
            } else {
                s.extend(encode(format!("{l}\n").as_bytes()).unwrap());
            }
        }
        s
    }

    #[test]
    fn v2_advertisement() {
        let input = stream(&[
            "version 2",
            "agent=git/2.56.0-Linux",
            "ls-refs=unborn",
            "fetch=shallow wait-for-done",
            "server-option",
            "object-format=sha1",
            "",
        ]);
        let mut r = Reader::new(&input[..], "test");
        let caps = Capabilities::read_v2_advertisement(&mut r).unwrap();
        assert_eq!(caps.len(), 5);
        assert!(caps.has_feature("fetch", "wait-for-done"));
        assert!(caps.has_feature("ls-refs", "unborn"));
        assert!(!caps.has_feature("server-option", "x"));
        assert!(caps.contains("server-option"));
        assert_eq!(caps.agent(), Some("git/2.56.0-Linux"));
        assert_eq!(r.read_or_eof().unwrap(), None);

        let mut w = Writer::new(Vec::new(), "test");
        caps.write_v2(&mut w).unwrap();
        assert_eq!(w.into_inner(), input);
    }

    #[test]
    fn version_detection() {
        let input = stream(&["version 1", "x HEAD"]);
        let mut r = Reader::new(&input[..], "test");
        assert_eq!(ProtocolVersion::read(&mut r).unwrap(), ProtocolVersion::V1);
        assert_eq!(r.read().unwrap(), Packet::Data(b"x HEAD"));

        let input = stream(&["x HEAD"]);
        let mut r = Reader::new(&input[..], "test");
        assert_eq!(ProtocolVersion::read(&mut r).unwrap(), ProtocolVersion::V0);
        assert_eq!(r.read().unwrap(), Packet::Data(b"x HEAD"));

        let mut r = Reader::new(&input[..], "test");
        assert!(matches!(
            Capabilities::read_v2_advertisement(&mut r),
            Err(Error::ExpectedVersion2)
        ));
    }

    #[test]
    fn v2_unexpected_delim() {
        let mut input = stream(&["version 2", "agent=x"]);
        input.extend_from_slice(b"0001");
        let mut r = Reader::new(&input[..], "test");
        let e = Capabilities::read_v2_advertisement(&mut r).unwrap_err();
        assert_eq!(e.to_string(), "protocol error: unexpected '0001'");
    }
}
