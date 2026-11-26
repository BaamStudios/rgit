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
                // A server that does not say uses SHA-1.
                if caps.object_format().unwrap_or(HashKind::Sha1.name()) != hash.name() {
                    return Err(Error::ReceiverHashNotSupported);
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
            let name = rgit_bstr::from_bytes(name).into_owned();
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
    caps.extend(crate::session_id::capability(server));
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
        let mut line =
            rgit_bstr::to_bytes(&format!("{} {} {}", c.old.to_hex(), c.new.to_hex(), c.name))
                .into_owned();
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

/// The unsigned text of a push certificate (Documentation/
/// gitprotocol-pack.adoc, "Push Certificate"): the header lines, the push
/// options (`push-option <o>`), an empty line and the commands. The
/// detached signature over it is appended before it is sent with
/// [`write_push_cert`].
pub fn push_cert_payload(
    pusher: &[u8],
    pushee: &str,
    nonce: &str,
    push_options: &[String],
    commands: &[Command],
) -> Vec<u8> {
    let mut out = b"certificate version 0.1\npusher ".to_vec();
    out.extend_from_slice(pusher);
    out.extend_from_slice(format!("\npushee {pushee}\nnonce {nonce}\n").as_bytes());
    for o in push_options {
        out.extend_from_slice(format!("push-option {o}\n").as_bytes());
    }
    out.push(b'\n');
    for c in commands {
        out.extend_from_slice(&rgit_bstr::to_bytes(&format!(
            "{} {} {}\n",
            c.old.to_hex(),
            c.new.to_hex(),
            c.name
        )));
    }
    out
}

/// Write a signed push certificate in place of the command list: the
/// `push-cert` line with `caps`, one packet per line of `cert` (payload
/// and signature), `push-cert-end` and a flush packet; then the push
/// options and their flush when `push_options` is `Some`.
pub fn write_push_cert<W: Write>(
    writer: &mut Writer<W>,
    cert: &[u8],
    caps: &[String],
    push_options: Option<&[String]>,
) -> Result<()> {
    let mut first = b"push-cert\0".to_vec();
    for cap in caps {
        first.push(b' ');
        first.extend_from_slice(cap.as_bytes());
    }
    writer.write_data(&first)?;
    for line in cert.split_inclusive(|&b| b == b'\n') {
        if line.ends_with(b"\n") {
            writer.write_data(line)?;
        } else {
            let mut l = line.to_vec();
            l.push(b'\n');
            writer.write_data(&l)?;
        }
    }
    writer.write_data(b"push-cert-end\n")?;
    writer.flush_pkt()?;
    if let Some(opts) = push_options {
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
    /// `report-status-v2` option lines after an `ok`: what the server
    /// really did when its `proc-receive` hook rewrote the command.
    pub options: RefOptions,
}

/// The `option` lines of one `ok` in a `report-status-v2` report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefOptions {
    /// `option refname`: the reference actually updated.
    pub refname: Option<String>,
    /// `option old-oid`.
    pub old_oid: Option<ObjectId>,
    /// `option new-oid`.
    pub new_oid: Option<ObjectId>,
    /// `option forced-update`.
    pub forced_update: bool,
}

impl RefOptions {
    /// True when the server sent any option line.
    pub fn any(&self) -> bool {
        self.refname.is_some()
            || self.old_oid.is_some()
            || self.new_oid.is_some()
            || self.forced_update
    }
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
                name: rgit_bstr::from_bytes(name).into_owned(),
                error: None,
                options: RefOptions::default(),
            });
        } else if let Some(rest) = line.strip_prefix(b"ng ") {
            let rest = rgit_bstr::from_bytes(rest);
            let (name, reason) = match rest.split_once(' ') {
                Some((n, r)) => (n.to_owned(), r.to_owned()),
                None => (rest.into_owned(), String::new()),
            };
            report.refs.push(RefReport {
                name,
                error: Some(reason),
                options: RefOptions::default(),
            });
        } else if let Some(option) = line.strip_prefix(b"option ") {
            // report-status-v2 details of the last `ok` (rewritten refs,
            // forced updates); unknown options and options without an
            // `ok` before them are ignored.
            let Some(last) = report.refs.last_mut().filter(|r| r.error.is_none()) else {
                continue;
            };
            let (key, value) = match option.iter().position(|&b| b == b' ') {
                Some(i) => (&option[..i], &option[i + 1..]),
                None => (option, &b""[..]),
            };
            let oid = || ObjectId::from_hex_any(value).ok();
            match key {
                b"refname" => {
                    last.options.refname = Some(String::from_utf8_lossy(value).into_owned())
                }
                b"old-oid" => last.options.old_oid = oid(),
                b"new-oid" => last.options.new_oid = oid(),
                b"forced-update" => last.options.forced_update = true,
                _ => {}
            }
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

    /// Advertised names that are not UTF-8 are kept (escaped), not
    /// rejected (#214).
    #[test]
    fn advertisement_with_raw_name() {
        let first = [
            format!("{A} refs/heads/caf").as_bytes(),
            b"\xe9\0report-status object-format=sha1",
        ]
        .concat();
        let data = pkts(&[&first, b""]);
        let mut r = Reader::new(&data[..], "push");
        let adv = read_advertisement(&mut r, HashKind::Sha1).unwrap();
        assert_eq!(adv.refs[0].name, "refs/heads/caf\u{10ffe9}");
    }

    /// The receiving end uses another hash algorithm: git's message, for a
    /// server that names its format and for one that does not (SHA-1).
    #[test]
    fn other_hash_algorithm() {
        let message = "the receiving end does not support this repository's hash algorithm";
        let zero64 = "0".repeat(64);
        let sha256 = format!("{zero64} capabilities^{{}}\0report-status object-format=sha256");
        let sha1 = format!("{A} refs/heads/main\0report-status object-format=sha1");
        let unnamed = format!("{A} refs/heads/main\0report-status");
        for (line, local) in [
            (&sha256, HashKind::Sha1),
            (&sha1, HashKind::Sha256),
            (&unnamed, HashKind::Sha256),
        ] {
            let data = pkts(&[line.as_bytes(), b""]);
            let mut r = Reader::new(&data[..], "push");
            let e = read_advertisement(&mut r, local).unwrap_err();
            assert_eq!(e.to_string(), message);
        }
        let data = pkts(&[sha256.as_bytes(), b""]);
        let mut r = Reader::new(&data[..], "push");
        let adv = read_advertisement(&mut r, HashKind::Sha256).unwrap();
        let caps = request_capabilities(&adv.caps, &Request::default());
        assert_eq!(caps[1], "object-format=sha256");
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
    fn push_certificate() {
        let hash = HashKind::Sha1;
        let cmds = [Command {
            old: ObjectId::from_hex(hash, ZERO.as_bytes()).unwrap(),
            new: ObjectId::from_hex(hash, A.as_bytes()).unwrap(),
            name: "refs/heads/x".into(),
        }];
        let opts = ["foo".to_string()];
        let payload = push_cert_payload(b"C <c@x> 1 +0000", "../dst", "n-1", &opts, &cmds);
        assert_eq!(
            payload,
            format!(
                "certificate version 0.1\npusher C <c@x> 1 +0000\npushee ../dst\nnonce n-1\n\
                 push-option foo\n\n{ZERO} {A} refs/heads/x\n"
            )
            .into_bytes()
        );
        let mut cert = payload.clone();
        cert.extend_from_slice(b"-----BEGIN-----\n\nabc\n-----END-----\n");
        let mut w = Writer::new(Vec::new(), "push");
        write_push_cert(&mut w, &cert, &["quiet".into()], Some(&opts)).unwrap();
        let line = format!("{ZERO} {A} refs/heads/x\n");
        let expect = pkts(&[
            b"push-cert\0 quiet",
            b"certificate version 0.1\n",
            b"pusher C <c@x> 1 +0000\n",
            b"pushee ../dst\n",
            b"nonce n-1\n",
            b"push-option foo\n",
            b"\n",
            line.as_bytes(),
            b"-----BEGIN-----\n",
            b"\n",
            b"abc\n",
            b"-----END-----\n",
            b"push-cert-end\n",
            b"",
            b"foo",
            b"",
        ]);
        assert_eq!(w.into_inner(), expect);
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
        assert!(rep.unpack_error.is_none() && rep.refs.len() == 2);
        let first = &rep.refs[0];
        assert!(first.error.is_none() && first.options.forced_update);
        assert!(first.options.refname.is_none());
        assert_eq!(
            rep.refs[1].error.as_deref(),
            Some("branch is currently checked out")
        );

        let old = format!("option old-oid {A}\n");
        let data = pkts(&[
            b"unpack ok\n",
            b"ok refs/for/main/topic\n",
            b"option refname refs/pull/1/head\n",
            old.as_bytes(),
            b"option fall-through\n",
            b"ok refs/for/main/topic\n",
            b"",
        ]);
        let rep = read_report(&mut Reader::new(&data[..], "push")).unwrap();
        assert_eq!(rep.refs.len(), 2);
        let o = &rep.refs[0].options;
        assert_eq!(o.refname.as_deref(), Some("refs/pull/1/head"));
        assert_eq!(o.old_oid.map(|id| id.to_hex()).as_deref(), Some(A));
        assert_eq!((o.new_oid, o.forced_update), (None, false));
        assert!(!rep.refs[1].options.any());

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
