// SPDX-License-Identifier: MIT
//! The client side of `git-receive-pack` in protocol v0
//! (`Documentation/gitprotocol-pack.adoc`, "Pushing Data To a Server",
//! and `gitprotocol-capabilities.adoc`).
//!
//! The exchange:
//!
//! 1. The server advertises its refs: `<oid> <name>`, the first line
//!    followed by NUL and the capability list. An empty repository sends
//!    `<zero-id> capabilities^{}` instead. `.have` lines name objects of
//!    alternate repositories, `shallow <oid>` lines the server's shallow
//!    boundary. A flush packet ends the list.
//! 2. The client sends one command per ref, `<old> <new> <name>`, the
//!    first one with NUL, a space and the requested capabilities, then a
//!    flush packet; with `push-options` one packet per option and another
//!    flush. A pack follows unless every command is a deletion. Without
//!    any command the client sends only the flush packet.
//! 3. With `report-status`/`report-status-v2` the server answers
//!    `unpack ok` (or `unpack <error>`), then `ok <ref>` or
//!    `ng <ref> <reason>` per command (v2 adds `option ...` lines) and a
//!    flush packet. With `side-band-64k` this report is itself wrapped in
//!    sideband channel 1 (see [`crate::sideband::SidebandReader`]).
//!
//! The capability string git 2.56.0 sends: ` report-status-v2
//! side-band-64k quiet atomic push-options object-format=<fmt>
//! agent=<agent>` (each only if the server advertised it and the push
//! needs it; `report-status` when `report-status-v2` is missing).

use std::io::{Read, Write};

use rgit_hash::{HashKind, ObjectId};

use crate::agent;
use crate::capabilities::{Capabilities, NO_REFS_NAME, split_ref_line};
use crate::error::{Error, Result};
use crate::pktline::{Packet, Reader, Writer};

/// A ref advertised by `receive-pack`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvertisedRef {
    /// Full ref name.
    pub name: String,
    /// Its value.
    pub oid: ObjectId,
}

/// The ref advertisement of `receive-pack`.
#[derive(Debug, Clone, Default)]
pub struct Advertisement {
    /// Refs in the order the server sent them.
    pub refs: Vec<AdvertisedRef>,
    /// Objects named by `.have` lines.
    pub haves: Vec<ObjectId>,
    /// `shallow` lines.
    pub shallow: Vec<ObjectId>,
    /// The capabilities of the first line.
    pub caps: Capabilities,
}

impl Advertisement {
    /// The value of `name`, if advertised.
    pub fn get(&self, name: &str) -> Option<ObjectId> {
        self.refs.iter().find(|r| r.name == name).map(|r| r.oid)
    }
}

/// Read the advertisement up to and including its flush packet. Object ids
/// are parsed as `hash` unless the server names another `object-format`,
/// which is an error.
pub fn read_advertisement<R: Read>(
    reader: &mut Reader<R>,
    hash: HashKind,
) -> Result<Advertisement> {
    let mut adv = Advertisement::default();
    let mut first = true;
    loop {
        let line = match reader.read()? {
            Packet::Flush => break,
            Packet::Data(d) => d.to_vec(),
            p => return Err(p.unexpected()),
        };
        // `version 1` in front of the refs (a server honouring v1).
        if first && line == b"version 1" {
            continue;
        }
        let (refpart, caps) = split_ref_line(&line);
        if let Some(caps) = caps {
            if first {
                if let Some(fmt) = caps.object_format() {
                    if fmt != hash.name() {
                        return Err(Error::MismatchedAlgorithms {
                            client: hash.name(),
                            server: HashKind::from_name(fmt).map_or("unknown", HashKind::name),
                        });
                    }
                }
                adv.caps = caps;
            }
        }
        first = false;
        if let Some(rest) = refpart.strip_prefix(b"shallow ") {
            let oid = ObjectId::from_hex(hash, rest).map_err(|_| unexpected(&line))?;
            adv.shallow.push(oid);
            continue;
        }
        let hex_len = hash.hex_len();
        if refpart.len() < hex_len + 2 || refpart[hex_len] != b' ' {
            return Err(unexpected(&line));
        }
        let oid = ObjectId::from_hex(hash, &refpart[..hex_len]).map_err(|_| unexpected(&line))?;
        let name = &refpart[hex_len + 1..];
        if name == b".have" {
            adv.haves.push(oid);
        } else if name == NO_REFS_NAME {
            // Only capabilities, no refs.
        } else {
            let name = String::from_utf8(name.to_vec()).map_err(|_| unexpected(&line))?;
            adv.refs.push(AdvertisedRef { name, oid });
        }
    }
    Ok(adv)
}

fn unexpected(line: &[u8]) -> Error {
    Error::Unexpected(String::from_utf8_lossy(line).into_owned())
}

/// One ref update sent to the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// The value the client expects on the server (zero: create).
    pub old: ObjectId,
    /// The new value (zero: delete).
    pub new: ObjectId,
    /// Full ref name.
    pub name: String,
}

/// What the client asks of the server.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Request {
    /// Ask for no progress output (`quiet`).
    pub quiet: bool,
    /// All or nothing (`atomic`).
    pub atomic: bool,
    /// `push-options` to send (the capability is requested when non-empty
    /// or [`Request::push_options_requested`] is set).
    pub push_options: Vec<String>,
    /// Request `push-options` even without options.
    pub push_options_requested: bool,
}

/// The capabilities the client requests, given the server's.
pub fn request_capabilities(server: &Capabilities, req: &Request) -> Vec<String> {
    let mut caps: Vec<String> = Vec::new();
    if server.contains("report-status-v2") {
        caps.push("report-status-v2".into());
    } else if server.contains("report-status") {
        caps.push("report-status".into());
    }
    if server.contains("side-band-64k") {
        caps.push("side-band-64k".into());
    }
    if req.quiet && server.contains("quiet") {
        caps.push("quiet".into());
    }
    if req.atomic && server.contains("atomic") {
        caps.push("atomic".into());
    }
    if (req.push_options_requested || !req.push_options.is_empty())
        && server.contains("push-options")
    {
        caps.push("push-options".into());
    }
    if let Some(fmt) = server.object_format() {
        caps.push(format!("object-format={fmt}"));
    }
    if server.contains("agent") {
        caps.push(format!("agent={}", agent::agent()));
    }
    caps
}

/// Write the command list (with `caps` on the first command) and its flush
/// packet, then the push options and their flush when `push_options` is
/// `Some`. Nothing but a flush packet is written when `commands` is empty.
pub fn write_commands<W: Write>(
    writer: &mut Writer<W>,
    commands: &[Command],
    caps: &[String],
    push_options: Option<&[String]>,
) -> Result<()> {
    for (i, c) in commands.iter().enumerate() {
        let mut line = format!("{} {} {}", c.old.to_hex(), c.new.to_hex(), c.name).into_bytes();
        if i == 0 {
            line.push(0);
            for cap in caps {
                line.push(b' ');
                line.extend_from_slice(cap.as_bytes());
            }
        }
        writer.write_data(&line)?;
    }
    writer.flush_pkt()?;
    if let (Some(opts), false) = (push_options, commands.is_empty()) {
        for o in opts {
            writer.write_data(o.as_bytes())?;
        }
        writer.flush_pkt()?;
    }
    Ok(())
}

/// The result for one ref in the server's report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefReport {
    /// Full ref name.
    pub name: String,
    /// `None` for `ok`, the reason for `ng`.
    pub error: Option<String>,
}

/// The server's status report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// `None` for `unpack ok`, else the error text.
    pub unpack_error: Option<String>,
    /// Per-ref results in the order received.
    pub refs: Vec<RefReport>,
}

/// Read a status report (`report-status` or `report-status-v2`) up to its
/// flush packet.
pub fn read_report<R: Read>(reader: &mut Reader<R>) -> Result<Report> {
    let mut report = Report::default();
    let first = match reader.read()? {
        Packet::Data(d) => d.to_vec(),
        p => return Err(p.unexpected()),
    };
    match first.strip_prefix(b"unpack ") {
        Some(b"ok") => {}
        Some(err) => report.unpack_error = Some(String::from_utf8_lossy(err).into_owned()),
        None => return Err(unexpected(&first)),
    }
    loop {
        let line = match reader.read()? {
            Packet::Flush => break,
            Packet::Data(d) => d.to_vec(),
            p => return Err(p.unexpected()),
        };
        if let Some(name) = line.strip_prefix(b"ok ") {
            report.refs.push(RefReport {
                name: String::from_utf8_lossy(name).into_owned(),
                error: None,
            });
        } else if let Some(rest) = line.strip_prefix(b"ng ") {
            let rest = String::from_utf8_lossy(rest);
            let (name, reason) = match rest.split_once(' ') {
                Some((n, r)) => (n.to_owned(), r.to_owned()),
                None => (rest.into_owned(), String::new()),
            };
            report.refs.push(RefReport {
                name,
                error: Some(reason),
            });
        } else if line.starts_with(b"option ") {
            // report-status-v2 details (rewritten refs, forced updates):
            // not used yet.
        } else {
            return Err(unexpected(&line));
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pktline::encode;

    const ZERO: &str = "0000000000000000000000000000000000000000";
    const A: &str = "8a9dddb74223866816bd2d1f14e99fc512bfcd62";
    const B: &str = "58c8d7b823a2e23553d359dac99540d8cec72a1e";

    fn pkts(lines: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for l in lines {
            if l.is_empty() {
                out.extend_from_slice(b"0000");
            } else {
                out.extend(encode(l).unwrap());
            }
        }
        out
    }

    #[test]
    fn advertisement_with_refs() {
        let first = format!(
            "{A} refs/heads/main\0report-status report-status-v2 delete-refs side-band-64k quiet atomic ofs-delta object-format=sha1 agent=git/2.56.0"
        );
        let second = format!("{B} refs/tags/v1\n");
        let have = format!("{B} .have");
        let data = pkts(&[first.as_bytes(), second.as_bytes(), have.as_bytes(), b""]);
        let mut r = Reader::new(&data[..], "push");
        let adv = read_advertisement(&mut r, HashKind::Sha1).unwrap();
        assert_eq!(adv.refs.len(), 2);
        assert_eq!(adv.refs[1].name, "refs/tags/v1");
        assert_eq!(adv.get("refs/heads/main").unwrap().to_hex(), A);
        assert_eq!(adv.haves.len(), 1);
        assert!(adv.caps.contains("delete-refs"));
        let req = Request {
            quiet: true,
            ..Request::default()
        };
        let caps = request_capabilities(&adv.caps, &req);
        assert_eq!(
            caps[..3],
            ["report-status-v2", "side-band-64k", "quiet"].map(String::from)
        );
        assert_eq!(caps[3], "object-format=sha1");
        assert!(caps[4].starts_with("agent="));
    }

    #[test]
    fn empty_repository() {
        let first = format!("{ZERO} capabilities^{{}}\0report-status delete-refs");
        let data = pkts(&[first.as_bytes(), b""]);
        let mut r = Reader::new(&data[..], "push");
        let adv = read_advertisement(&mut r, HashKind::Sha1).unwrap();
        assert!(adv.refs.is_empty());
        let caps = request_capabilities(&adv.caps, &Request::default());
        assert_eq!(caps, ["report-status"]);
    }

    #[test]
    fn commands_and_options() {
        let hash = HashKind::Sha1;
        let cmds = [
            Command {
                old: ObjectId::from_hex(hash, ZERO.as_bytes()).unwrap(),
                new: ObjectId::from_hex(hash, A.as_bytes()).unwrap(),
                name: "refs/heads/main".into(),
            },
            Command {
                old: ObjectId::from_hex(hash, B.as_bytes()).unwrap(),
                new: ObjectId::from_hex(hash, ZERO.as_bytes()).unwrap(),
                name: "refs/heads/x".into(),
            },
        ];
        let mut w = Writer::new(Vec::new(), "push");
        let opts = ["a".to_string()];
        write_commands(&mut w, &cmds, &["quiet".into()], Some(&opts)).unwrap();
        let out = w.into_inner();
        let expect = pkts(&[
            format!("{ZERO} {A} refs/heads/main\0 quiet").as_bytes(),
            format!("{B} {ZERO} refs/heads/x").as_bytes(),
            b"",
            b"a",
            b"",
        ]);
        assert_eq!(out, expect);

        let mut w = Writer::new(Vec::new(), "push");
        write_commands(&mut w, &[], &[], Some(&opts)).unwrap();
        assert_eq!(w.into_inner(), b"0000");
    }

    #[test]
    fn report() {
        let data = pkts(&[
            b"unpack ok\n",
            b"ok refs/heads/main\n",
            b"option forced-update\n",
            b"ng refs/heads/x branch is currently checked out\n",
            b"",
        ]);
        let mut r = Reader::new(&data[..], "push");
        let rep = read_report(&mut r).unwrap();
        assert_eq!(rep.unpack_error, None);
        assert_eq!(rep.refs.len(), 2);
        assert_eq!(rep.refs[0].error, None);
        assert_eq!(
            rep.refs[1].error.as_deref(),
            Some("branch is currently checked out")
        );

        let data = pkts(&[
            b"unpack index-pack abnormal exit\n",
            b"ng refs/heads/a unpacker error\n",
            b"",
        ]);
        let rep = read_report(&mut Reader::new(&data[..], "push")).unwrap();
        assert_eq!(
            rep.unpack_error.as_deref(),
            Some("index-pack abnormal exit")
        );
    }
}
