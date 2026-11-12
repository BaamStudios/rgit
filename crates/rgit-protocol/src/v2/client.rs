// SPDX-License-Identifier: MIT
//! The client side of protocol v2 (`Documentation/gitprotocol-v2.adoc`):
//! capability advertisement, `ls-refs` and `fetch`.
//!
//! [`Client`] works on a pair of byte streams. On a stateful connection
//! (`file://`, `ssh://`, `git://`) both streams belong to one long-running
//! `upload-pack`. On a stateless connection (smart HTTP) every request is
//! answered separately: the transport has to send what was written so far
//! as one request when the writer is flushed ([`Write::flush`]; the client
//! flushes after every request) and then let the reader return that
//! request's response. The client itself behaves the same in both modes,
//! because every v2 command is self-contained; the differences are:
//!
//! - stateless: a `# service=git-upload-pack` header before the
//!   advertisement is skipped, response-end packets (`0002`) in front of a
//!   response are skipped, and [`Client::close`] sends nothing;
//! - stateful: [`Client::close`] sends a flush packet to end the session.
//!
//! What is sent, as observed from git 2.56.0:
//!
//! - every request starts with `command=<name>`, then `agent=<ours>` if the
//!   server advertised `agent`, `object-format=<the server's>` if it
//!   advertised `object-format`, one `server-option=<opt>` per server
//!   option if it advertised `server-option`, and a delimiter packet;
//! - `ls-refs` arguments: `peel`, `symrefs`, `unborn` (only if the server
//!   advertised `ls-refs=unborn`), `ref-prefix <p>`...;
//! - `fetch` arguments: `thin-pack`, `no-progress`, `include-tag`,
//!   `ofs-delta`, `shallow <oid>`..., `deepen <n>`, `deepen-relative`,
//!   `deepen-since <t>`, `deepen-not <rev>`..., `filter <spec>` (only if
//!   the server advertised `fetch=... filter`), `want <oid>`...,
//!   `have <oid>`..., `done`;
//! - negotiation: the first request carries up to 16 new `have` lines, each
//!   further request twice as many up to 16384, then a tenth more each
//!   time. Every request repeats the wants and, first among the haves, all
//!   commits the server has ACKed so far. `done` is added when no new
//!   `have` is left, or when at least one ACK was received and 256 or more
//!   new haves went out since the last ACK of a commit not known to be
//!   common before;
//! - the response to a request without `done` starts with
//!   `acknowledgments` (`NAK`, `ACK <oid>`..., `ready`); after `ready` the
//!   `shallow-info`, `wanted-refs` and `packfile` sections follow in the
//!   same response. The response to `done` starts with those sections
//!   directly. The `packfile` section is a side-band-64k stream.

use std::io::{self, Read, Write};

use rgit_hash::{HashKind, ObjectId};

use crate::agent;
use crate::capabilities::{Capabilities, ProtocolVersion};
use crate::error::{Error, Result};
use crate::pktline::{Packet, Reader, Writer};
use crate::sideband::{Band, Progress, demux};

use super::negotiate::{INITIAL_HAVES, MAX_IN_VAIN, Negotiator, next_have_count};

/// The first line of a smart HTTP advertisement in front of the protocol
/// data.
const SERVICE_HEADER: &[u8] = b"# service=git-upload-pack";

/// Arguments of `ls-refs`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LsRefsArgs {
    /// `peel`: report the peeled object of annotated tags.
    pub peel: bool,
    /// `symrefs`: report the target of symbolic refs.
    pub symrefs: bool,
    /// `unborn`: report `HEAD` even if it points to an unborn branch. Only
    /// sent if the server advertises `ls-refs=unborn`.
    pub unborn: bool,
    /// `ref-prefix <prefix>` lines. The server may send other refs too.
    pub ref_prefixes: Vec<String>,
    /// `server-option=<option>` lines (sent only if the server advertises
    /// `server-option`).
    pub server_options: Vec<String>,
}

impl LsRefsArgs {
    /// `peel`, `symrefs` and `unborn`: what `git ls-remote`, `fetch` and
    /// `clone` ask for.
    pub fn standard() -> LsRefsArgs {
        LsRefsArgs {
            peel: true,
            symrefs: true,
            unborn: true,
            ..LsRefsArgs::default()
        }
    }
}

/// One ref of an `ls-refs` response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRef {
    /// The full ref name (`HEAD`, `refs/heads/main`).
    pub name: Vec<u8>,
    /// The object the ref points to; `None` for an unborn `HEAD`.
    pub oid: Option<ObjectId>,
    /// `symref-target:<ref>`.
    pub symref_target: Option<Vec<u8>>,
    /// `peeled:<oid>`.
    pub peeled: Option<ObjectId>,
}

/// Arguments of `fetch`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchArgs {
    /// `want <oid>` lines, sent in this order (duplicates included).
    pub wants: Vec<ObjectId>,
    /// `thin-pack`.
    pub thin_pack: bool,
    /// `no-progress`.
    pub no_progress: bool,
    /// `include-tag`.
    pub include_tag: bool,
    /// `ofs-delta`.
    pub ofs_delta: bool,
    /// `shallow <oid>`: the client's current shallow commits.
    pub shallow: Vec<ObjectId>,
    /// `deepen <depth>`.
    pub deepen: Option<u32>,
    /// `deepen-relative`.
    pub deepen_relative: bool,
    /// `deepen-since <timestamp>`.
    pub deepen_since: Option<i64>,
    /// `deepen-not <rev>` lines.
    pub deepen_not: Vec<String>,
    /// `filter <filter-spec>`; dropped if the server does not advertise the
    /// `filter` feature (see [`Client::supports_filter`]).
    pub filter: Option<String>,
    /// `server-option=<option>` lines (sent only if the server advertises
    /// `server-option`).
    pub server_options: Vec<String>,
    /// The local repository's hash algorithm; if set, it must match the
    /// server's.
    pub local_format: Option<HashKind>,
}

impl FetchArgs {
    fn wants_shallow(&self) -> bool {
        !self.shallow.is_empty()
            || self.deepen.is_some()
            || self.deepen_relative
            || self.deepen_since.is_some()
            || !self.deepen_not.is_empty()
    }
}

/// The `shallow-info` section.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShallowInfo {
    /// `shallow <oid>`: commits whose parents are not sent.
    pub shallow: Vec<ObjectId>,
    /// `unshallow <oid>`: formerly shallow commits whose parents are sent.
    pub unshallow: Vec<ObjectId>,
}

/// What a `fetch` returned besides the pack.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchOutcome {
    /// Commits the server ACKed, in the order of their first ACK.
    pub common: Vec<ObjectId>,
    /// The `shallow-info` section, if there was one.
    pub shallow_info: Option<ShallowInfo>,
    /// The `wanted-refs` section: `(oid, refname)`.
    pub wanted_refs: Vec<(ObjectId, Vec<u8>)>,
    /// Number of requests sent.
    pub rounds: usize,
    /// Whether a `packfile` section was received.
    pub packfile: bool,
}

/// An owned packet, so that the reader can be used again while one is being
/// looked at.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pkt {
    Flush,
    Delim,
    End,
    Data(Vec<u8>),
}

impl Pkt {
    fn from_packet(p: Packet<'_>) -> Pkt {
        match p {
            Packet::Flush => Pkt::Flush,
            Packet::Delim => Pkt::Delim,
            Packet::ResponseEnd => Pkt::End,
            Packet::Data(d) => Pkt::Data(d.to_vec()),
        }
    }

    fn describe(&self) -> String {
        match self {
            Pkt::Flush => "0000".into(),
            Pkt::Delim => "0001".into(),
            Pkt::End => "0002".into(),
            Pkt::Data(d) => String::from_utf8_lossy(d).into_owned(),
        }
    }
}

/// A protocol v2 client.
#[derive(Debug)]
pub struct Client<R, W> {
    reader: Reader<R>,
    writer: Writer<W>,
    stateless: bool,
    caps: Capabilities,
    format: HashKind,
    agent: String,
}

impl<R: Read, W: Write> Client<R, W> {
    /// Read the capability advertisement from `read`. `name` is the program
    /// name used in packet traces (`fetch`, `clone`, `ls-remote`). A server
    /// that answers with protocol v0 or v1 is an [`Error::NotV2`].
    pub fn connect(read: R, write: W, stateless: bool, name: &'static str) -> Result<Self> {
        let mut reader = Reader::new(read, name);
        let writer = Writer::new(write, name);
        if stateless && matches!(reader.peek()?, Some(Packet::Data(d)) if d == SERVICE_HEADER) {
            reader.read()?;
            match reader.read()? {
                Packet::Flush => {}
                other => return Err(other.unexpected()),
            }
        }
        let caps = match ProtocolVersion::read(&mut reader)? {
            ProtocolVersion::V2 => Capabilities::read_v2(&mut reader)?,
            ProtocolVersion::V1 => return Err(Error::NotV2(1)),
            ProtocolVersion::V0 => return Err(Error::NotV2(0)),
        };
        Client::with_capabilities(reader, writer, caps, stateless)
    }

    /// A client for a connection whose advertisement was already read.
    pub fn with_capabilities(
        reader: Reader<R>,
        writer: Writer<W>,
        caps: Capabilities,
        stateless: bool,
    ) -> Result<Self> {
        let format = match caps.object_format() {
            None => HashKind::Sha1,
            Some(name) => HashKind::from_name(name)
                .ok_or_else(|| Error::UnknownObjectFormat(name.to_owned()))?,
        };
        Ok(Client {
            reader,
            writer,
            stateless,
            caps,
            format,
            agent: agent::agent().to_owned(),
        })
    }

    /// The server's capabilities.
    pub fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    /// The server's hash algorithm (SHA-1 if it advertises none).
    pub fn object_format(&self) -> HashKind {
        self.format
    }

    /// Whether the connection is stateless.
    pub fn is_stateless(&self) -> bool {
        self.stateless
    }

    /// The agent string sent to servers that advertise `agent` (default:
    /// [`agent::agent`]).
    pub fn set_agent(&mut self, agent: impl Into<String>) {
        self.agent = agent.into();
    }

    /// Whether the server accepts `filter` in `fetch`.
    pub fn supports_filter(&self) -> bool {
        self.caps.has_feature("fetch", "filter")
    }

    /// Whether the server accepts shallow and deepen arguments in `fetch`.
    pub fn supports_shallow(&self) -> bool {
        self.caps.has_feature("fetch", "shallow")
    }

    /// The packet reader.
    pub fn reader_mut(&mut self) -> &mut Reader<R> {
        &mut self.reader
    }

    /// The packet writer.
    pub fn writer_mut(&mut self) -> &mut Writer<W> {
        &mut self.writer
    }

    /// The packet reader and writer.
    pub fn into_parts(self) -> (Reader<R>, Writer<W>) {
        (self.reader, self.writer)
    }

    /// End the session: a flush packet on a stateful connection, nothing on
    /// a stateless one. git sends it after `ls-refs` when it does not fetch;
    /// after a `fetch` that received a pack it just closes the connection.
    pub fn close(&mut self) -> Result<()> {
        if !self.stateless {
            self.writer.flush_pkt()?;
            self.writer.flush_io()?;
        }
        Ok(())
    }

    fn line(&mut self, line: &str) -> Result<()> {
        if line.contains('\n') {
            return Err(Error::BadRequestLine(line.to_owned()));
        }
        self.writer.write_line(line)
    }

    /// `command=`, the capability list and the delimiter.
    fn begin(&mut self, command: &'static str, server_options: &[String]) -> Result<()> {
        if !self.caps.contains(command) {
            return Err(Error::CommandNotSupported(command));
        }
        self.line(&format!("command={command}"))?;
        if self.caps.contains("agent") {
            let agent = format!("agent={}", self.agent);
            self.line(&agent)?;
        }
        if self.caps.contains("object-format") {
            let format = format!("object-format={}", self.format.name());
            self.line(&format)?;
        }
        if self.caps.contains("server-option") {
            for o in server_options {
                self.line(&format!("server-option={o}"))?;
            }
        }
        self.writer.delim_pkt()
    }

    fn end_request(&mut self) -> Result<()> {
        self.writer.flush_pkt()?;
        self.writer.flush_io()
    }

    fn next(&mut self) -> Result<Pkt> {
        Ok(Pkt::from_packet(self.reader.read()?))
    }

    /// The first packet of a response; response-end packets left over from
    /// an earlier response are skipped.
    fn first(&mut self) -> Result<Pkt> {
        loop {
            match self.next()? {
                Pkt::End => continue,
                p => return Ok(p),
            }
        }
    }

    fn oid(&self, hex: &[u8]) -> Option<ObjectId> {
        ObjectId::from_hex(self.format, hex).ok()
    }

    /// Run `ls-refs`.
    pub fn ls_refs(&mut self, args: &LsRefsArgs) -> Result<Vec<RemoteRef>> {
        self.begin("ls-refs", &args.server_options)?;
        if args.peel {
            self.line("peel")?;
        }
        if args.symrefs {
            self.line("symrefs")?;
        }
        if args.unborn && self.caps.has_feature("ls-refs", "unborn") {
            self.line("unborn")?;
        }
        for p in &args.ref_prefixes {
            self.line(&format!("ref-prefix {p}"))?;
        }
        self.end_request()?;

        let mut refs = Vec::new();
        let mut pkt = self.first()?;
        loop {
            match pkt {
                Pkt::Flush => return Ok(refs),
                Pkt::Data(line) => refs.push(self.parse_ref(&line)?),
                other => return Err(Error::Unexpected(other.describe())),
            }
            pkt = self.next()?;
        }
    }

    fn parse_ref(&self, line: &[u8]) -> Result<RemoteRef> {
        let bad = || Error::InvalidLsRefs(String::from_utf8_lossy(line).into_owned());
        let mut fields = line.split(|&b| b == b' ');
        let first = fields.next().ok_or_else(bad)?;
        let oid = if first == b"unborn" {
            None
        } else {
            Some(self.oid(first).ok_or_else(bad)?)
        };
        let name = fields.next().filter(|n| !n.is_empty()).ok_or_else(bad)?;
        let mut r = RemoteRef {
            name: name.to_vec(),
            oid,
            symref_target: None,
            peeled: None,
        };
        for attr in fields {
            if let Some(target) = attr.strip_prefix(b"symref-target:") {
                r.symref_target = Some(target.to_vec());
            } else if let Some(hex) = attr.strip_prefix(b"peeled:") {
                r.peeled = Some(self.oid(hex).ok_or_else(bad)?);
            }
            // Unknown attributes are ignored.
        }
        Ok(r)
    }

    /// Run `fetch`: negotiate with `negotiator` and hand the pack data to
    /// `on_pack` in the order received. Remote progress and errors go to
    /// `progress`.
    ///
    /// With no wants nothing is sent. Sideband errors
    /// ([`Error::SidebandDisconnect`] and friends) are prefixed with the
    /// program name by git (`fetch-pack: `); that is up to the caller.
    pub fn fetch<P, F>(
        &mut self,
        args: &FetchArgs,
        negotiator: &mut dyn Negotiator,
        progress: &mut Progress<P>,
        mut on_pack: F,
    ) -> Result<FetchOutcome>
    where
        P: Write,
        F: FnMut(&[u8]) -> io::Result<()>,
    {
        let mut outcome = FetchOutcome::default();
        if let Some(local) = args.local_format {
            if local != self.format {
                return Err(Error::MismatchedAlgorithms {
                    client: local.name(),
                    server: self.format.name(),
                });
            }
        }
        if args.wants.is_empty() {
            return Ok(outcome);
        }
        if args.wants_shallow() && !self.supports_shallow() {
            return Err(Error::ShallowNotSupported);
        }

        let mut count = INITIAL_HAVES;
        let mut in_vain = 0usize;
        let mut seen_ack = false;
        loop {
            self.write_fetch_args(args)?;
            for id in &outcome.common {
                self.writer.write_line(&format!("have {id}"))?;
            }
            let mut added = 0;
            while added < count {
                let Some(id) = negotiator.next_have() else {
                    break;
                };
                self.writer.write_line(&format!("have {id}"))?;
                added += 1;
            }
            in_vain += added;
            let done = added == 0 || (seen_ack && in_vain >= MAX_IN_VAIN);
            if done {
                self.writer.write_line("done")?;
            }
            self.end_request()?;
            outcome.rounds += 1;

            if done {
                let first = self.first()?;
                self.read_sections(first, &mut outcome, progress, &mut on_pack)?;
                return Ok(outcome);
            }

            match self.first()? {
                Pkt::Data(d) if d == b"acknowledgments" => {}
                other => {
                    return Err(Error::ExpectedSection("acknowledgments", other.describe()));
                }
            }
            let mut ready = false;
            loop {
                match self.next()? {
                    Pkt::Flush if !ready => break,
                    Pkt::Delim if !ready => return Err(Error::NoSectionsWithoutReady),
                    Pkt::Delim => {
                        let first = self.next()?;
                        self.read_sections(first, &mut outcome, progress, &mut on_pack)?;
                        return Ok(outcome);
                    }
                    Pkt::Data(d) if !ready && d == b"NAK" => {}
                    Pkt::Data(d) if !ready && d == b"ready" => ready = true,
                    Pkt::Data(d) if !ready && d.starts_with(b"ACK ") => {
                        let Some(id) = self.oid(&d[4..]) else {
                            return Err(Error::UnexpectedAckLine(
                                String::from_utf8_lossy(&d).into_owned(),
                            ));
                        };
                        seen_ack = true;
                        if negotiator.ack(&id) {
                            in_vain = 0;
                        }
                        if !outcome.common.contains(&id) {
                            outcome.common.push(id);
                        }
                    }
                    Pkt::Data(d) => {
                        return Err(Error::UnexpectedAckLine(
                            String::from_utf8_lossy(&d).into_owned(),
                        ));
                    }
                    _ if ready => return Err(Error::PackAfterReady),
                    other => return Err(Error::Unexpected(other.describe())),
                }
            }
            count = next_have_count(count);
        }
    }

    fn write_fetch_args(&mut self, args: &FetchArgs) -> Result<()> {
        self.begin("fetch", &args.server_options)?;
        for (on, name) in [
            (args.thin_pack, "thin-pack"),
            (args.no_progress, "no-progress"),
            (args.include_tag, "include-tag"),
            (args.ofs_delta, "ofs-delta"),
        ] {
            if on {
                self.line(name)?;
            }
        }
        for id in &args.shallow {
            self.line(&format!("shallow {id}"))?;
        }
        if let Some(depth) = args.deepen {
            self.line(&format!("deepen {depth}"))?;
        }
        if args.deepen_relative {
            self.line("deepen-relative")?;
        }
        if let Some(since) = args.deepen_since {
            self.line(&format!("deepen-since {since}"))?;
        }
        for rev in &args.deepen_not {
            self.line(&format!("deepen-not {rev}"))?;
        }
        if let Some(filter) = &args.filter {
            if self.supports_filter() {
                self.line(&format!("filter {filter}"))?;
            }
        }
        for id in &args.wants {
            self.line(&format!("want {id}"))?;
        }
        Ok(())
    }

    /// The sections after `acknowledgments` (or of a response to `done`),
    /// starting with the header packet `first`.
    fn read_sections<P: Write, F: FnMut(&[u8]) -> io::Result<()>>(
        &mut self,
        first: Pkt,
        outcome: &mut FetchOutcome,
        progress: &mut Progress<P>,
        on_pack: &mut F,
    ) -> Result<()> {
        // Sections must come in this order; each may appear once.
        const ORDER: [&[u8]; 3] = [b"shallow-info", b"wanted-refs", b"packfile"];
        let mut stage = 0;
        let mut header = first;
        loop {
            let pos = match &header {
                Pkt::Data(d) => ORDER.iter().position(|s| s == d),
                _ => None,
            };
            let pos = match pos {
                Some(p) if p >= stage => p,
                _ => return Err(Error::ExpectedSection("packfile", header.describe())),
            };
            stage = pos + 1;
            let end = match pos {
                0 => {
                    let mut info = ShallowInfo::default();
                    let end = self.read_lines(|this, line| {
                        let (list, hex) = if let Some(h) = line.strip_prefix(b"shallow ") {
                            (&mut info.shallow, h)
                        } else if let Some(h) = line.strip_prefix(b"unshallow ") {
                            (&mut info.unshallow, h)
                        } else {
                            return Err(Error::BadShallowLine(
                                String::from_utf8_lossy(line).into_owned(),
                            ));
                        };
                        let id = this.oid(hex).ok_or_else(|| {
                            Error::BadShallowLine(String::from_utf8_lossy(line).into_owned())
                        })?;
                        list.push(id);
                        Ok(())
                    })?;
                    outcome.shallow_info = Some(info);
                    end
                }
                1 => {
                    let mut refs = Vec::new();
                    let end = self.read_lines(|this, line| {
                        let bad =
                            || Error::BadWantedRef(String::from_utf8_lossy(line).into_owned());
                        let sp = line.iter().position(|&b| b == b' ').ok_or_else(bad)?;
                        let id = this.oid(&line[..sp]).ok_or_else(bad)?;
                        refs.push((id, line[sp + 1..].to_vec()));
                        Ok(())
                    })?;
                    outcome.wanted_refs = refs;
                    end
                }
                _ => {
                    self.read_packfile(progress, on_pack)?;
                    outcome.packfile = true;
                    return Ok(());
                }
            };
            match end {
                Pkt::Delim => header = self.next()?,
                other => return Err(Error::ExpectedSection("packfile", other.describe())),
            }
        }
    }

    /// Data lines up to the next special packet, which is returned.
    fn read_lines(&mut self, mut each: impl FnMut(&Self, &[u8]) -> Result<()>) -> Result<Pkt> {
        loop {
            match self.next()? {
                Pkt::Data(line) => each(self, &line)?,
                other => return Ok(other),
            }
        }
    }

    /// The side-band-64k stream of the `packfile` section, up to its flush
    /// packet.
    fn read_packfile<P: Write, F: FnMut(&[u8]) -> io::Result<()>>(
        &mut self,
        progress: &mut Progress<P>,
        on_pack: &mut F,
    ) -> Result<()> {
        let name = self.reader.tracer_mut().name().to_owned();
        self.reader.tracer_mut().set_name("sideband");
        self.reader.chomp_newline(false).err_packets(false);
        let result = read_sideband(&mut self.reader, progress, on_pack);
        self.reader.tracer_mut().set_name(name);
        self.reader.chomp_newline(true).err_packets(true);
        result
    }
}

fn read_sideband<R: Read, P: Write, F: FnMut(&[u8]) -> io::Result<()>>(
    reader: &mut Reader<R>,
    progress: &mut Progress<P>,
    on_pack: &mut F,
) -> Result<()> {
    loop {
        let packet = match reader.read_or_eof() {
            Ok(Some(p)) => p,
            Ok(None) | Err(Error::HungUp) => {
                progress.finish()?;
                return Err(Error::SidebandDisconnect);
            }
            Err(e) => return Err(e),
        };
        let Packet::Data(payload) = packet else {
            progress.finish()?;
            return Ok(());
        };
        match demux(payload) {
            Ok(Band::Data(d)) => {
                if !d.is_empty() {
                    on_pack(d)?;
                }
            }
            Ok(Band::Progress(p)) => progress.feed(p)?,
            Ok(Band::Error(m)) => {
                let msg = m.to_vec();
                progress.remote_error(&msg)?;
                let text = String::from_utf8_lossy(&msg);
                let text = text.strip_suffix('\n').unwrap_or(&text);
                return Err(Error::SidebandFatal(text.to_owned()));
            }
            Err(e) => {
                progress.finish()?;
                return Err(e);
            }
        }
    }
}
