// SPDX-License-Identifier: MIT
//! Protocol v0 and v1, client side of `upload-pack`
//! (`Documentation/gitprotocol-pack.adoc`, "Reference Discovery" and
//! "Packfile Negotiation"; `gitprotocol-capabilities.adoc`). v1 is v0 with
//! a `version 1` line in front of the advertisement.
//!
//! [`read_advertisement`] parses the ref advertisement; [`Client`] runs the
//! negotiation and receives the pack. What is sent, as observed from git
//! 2.56.0:
//!
//! - the first `want` line carries the requested capabilities, in this
//!   order and each only if the server advertised it: `multi_ack_detailed`
//!   (else `multi_ack`), `no-done` (stateless connections only),
//!   `side-band-64k` (else `side-band`), `thin-pack`, `no-progress`,
//!   `include-tag`, `ofs-delta`, `shallow` (when the client is or becomes
//!   shallow), `deepen-since`, `deepen-not`, `deepen-relative` (when
//!   asked), `filter` (when filtering), `agent=<ours>`; `object-format=`
//!   only for a server whose format is not SHA-1;
//! - then the remaining `want` lines (duplicates included), `shallow`
//!   lines, `deepen`/`deepen-since`/`deepen-not`, `filter`, and a flush;
//! - `have` lines in batches: a flush after 16 haves in total, then after
//!   twice as many in total up to 32, then every 32 more on a stateful
//!   connection; on a stateless one the total doubles each time (up to
//!   16384, then grows by a tenth). A stateful client keeps one batch in
//!   flight: it reads the response to a batch only after sending the next;
//! - a stateless request repeats the wants and sends the commits ACKed so
//!   far before the new haves;
//! - `done` when no have is left, when the server said `ready`, or (after
//!   the first ACK) when 256 haves in a row brought no new common commit;
//! - each batch is answered by `ACK <oid> continue|common|ready` lines and
//!   `NAK`; `done` by `ACK <oid>` or `NAK`, after which the pack follows
//!   (multiplexed with side-band, else raw up to the end of the stream).
//!   With `no-done`, a stateless response that said `ready` continues with
//!   the final `ACK` and the pack.

use std::io::{self, Read, Write};

use rgit_hash::{HashKind, ObjectId};

use crate::agent;
use crate::capabilities::{Capabilities, NO_REFS_NAME, ProtocolVersion, split_ref_line};
use crate::error::{Error, Result};
use crate::pktline::{Packet, Reader, Writer};
use crate::sideband::Progress;
use crate::v2::client::{Duplex, FetchArgs, FetchOutcome, RemoteRef, ShallowInfo, read_sideband};
use crate::v2::negotiate::{INITIAL_HAVES, MAX_IN_VAIN, Negotiator, next_have_count};

/// A parsed v0/v1 ref advertisement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advertisement {
    /// The protocol version the server answered with (V0 or V1).
    pub version: ProtocolVersion,
    /// The refs in the order sent; a `<name>^{}` line becomes the
    /// `peeled` value of the ref before it, `symref=` capabilities the
    /// `symref_target`s.
    pub refs: Vec<RemoteRef>,
    /// The capabilities of the first line.
    pub caps: Capabilities,
    /// `shallow <oid>` lines (the server is a shallow repository).
    pub shallow: Vec<ObjectId>,
    /// The server's hash algorithm (`object-format=`, SHA-1 if absent).
    pub format: HashKind,
}

impl Advertisement {
    /// `HEAD`'s target from the `symref=HEAD:<ref>` capability (also sent
    /// by an empty repository).
    pub fn head_target(&self) -> Option<&str> {
        self.caps
            .symrefs()
            .find(|(from, _)| *from == "HEAD")
            .map(|(_, to)| to)
    }
}

/// Read a v0/v1 advertisement up to its flush packet (or the end of the
/// stream). `version` is what [`ProtocolVersion::read`] returned; its
/// `version 1` line must be consumed already.
pub fn read_advertisement<R: Read>(
    reader: &mut Reader<R>,
    version: ProtocolVersion,
) -> Result<Advertisement> {
    let mut adv = Advertisement {
        version,
        refs: Vec::new(),
        caps: Capabilities::new(),
        shallow: Vec::new(),
        format: HashKind::Sha1,
    };
    let mut first = true;
    loop {
        let line = match reader.read_or_eof()? {
            None | Some(Packet::Flush) => break,
            Some(Packet::Data(d)) => d.to_vec(),
            Some(other) => return Err(other.unexpected()),
        };
        let bad = || Error::Unexpected(String::from_utf8_lossy(&line).into_owned());
        let (refpart, caps) = split_ref_line(&line);
        if let Some(caps) = caps {
            if first {
                if let Some(name) = caps.object_format() {
                    adv.format = HashKind::from_name(name)
                        .ok_or_else(|| Error::UnknownObjectFormat(name.to_owned()))?;
                }
                adv.caps = caps;
            }
        }
        first = false;
        if let Some(hex) = refpart.strip_prefix(b"shallow ") {
            adv.shallow
                .push(ObjectId::from_hex(adv.format, hex).map_err(|_| bad())?);
            continue;
        }
        let sp = refpart.iter().position(|&b| b == b' ').ok_or_else(bad)?;
        let (hex, name) = (&refpart[..sp], &refpart[sp + 1..]);
        let oid = ObjectId::from_hex(adv.format, hex).map_err(|_| bad())?;
        if name == NO_REFS_NAME {
            continue;
        }
        if name.is_empty() {
            return Err(bad());
        }
        if let Some(base) = name.strip_suffix(b"^{}") {
            if let Some(last) = adv.refs.last_mut().filter(|r| r.name == base) {
                last.peeled = Some(oid);
                continue;
            }
        }
        adv.refs.push(RemoteRef {
            name: name.to_vec(),
            oid: Some(oid),
            symref_target: None,
            peeled: None,
        });
    }
    let symrefs: Vec<(String, String)> = adv
        .caps
        .symrefs()
        .map(|(a, b)| (a.to_owned(), b.to_owned()))
        .collect();
    for r in &mut adv.refs {
        r.symref_target = symrefs
            .iter()
            .find(|(from, _)| from.as_bytes() == r.name)
            .map(|(_, to)| to.as_bytes().to_vec());
    }
    Ok(adv)
}

/// How the server acknowledges `have` lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AckMode {
    /// Neither `multi_ack` nor `multi_ack_detailed`.
    Single,
    /// `multi_ack`.
    Multi,
    /// `multi_ack_detailed`.
    Detailed,
}

/// The next total of `have` lines after which a stateful client flushes.
fn next_stateful_flush(count: usize) -> usize {
    if count < 32 { count * 2 } else { count + 32 }
}

/// One line of a negotiation response.
enum Ack {
    /// `ACK <oid> continue|common|ready` (`ready` = true for `ready`).
    Continue(ObjectId, bool),
    /// `ACK <oid>` without status.
    Final(ObjectId),
    /// `NAK`.
    Nak,
}

/// A protocol v0/v1 fetch client over a connection whose advertisement
/// was read with [`read_advertisement`].
#[derive(Debug)]
pub struct Client<C> {
    reader: Reader<C>,
    caps: Capabilities,
    format: HashKind,
    stateless: bool,
    agent: String,
    /// Whether a request was sent (a session without one ends with a
    /// flush packet).
    requested: bool,
}

/// Per-fetch negotiation state.
#[derive(Default)]
struct State {
    in_vain: usize,
    got_continue: bool,
    got_ready: bool,
    single_acked: bool,
}

impl<C: Duplex> Client<C> {
    /// A client continuing on `reader` after `adv`.
    pub fn new(reader: Reader<C>, adv: &Advertisement) -> Client<C> {
        let stateless = reader.get_ref().is_stateless();
        Client {
            reader,
            caps: adv.caps.clone(),
            format: adv.format,
            stateless,
            agent: agent::agent().to_owned(),
            requested: false,
        }
    }

    /// The server's capabilities.
    pub fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    /// The server's hash algorithm.
    pub fn object_format(&self) -> HashKind {
        self.format
    }

    /// Whether the connection is stateless (smart HTTP).
    pub fn is_stateless(&self) -> bool {
        self.stateless
    }

    /// The connection.
    pub fn into_inner(self) -> C {
        self.reader.into_inner()
    }

    /// End a session that fetched nothing: a flush packet on a stateful
    /// connection (instead of wants).
    pub fn close(&mut self) -> Result<()> {
        if !self.stateless && !self.requested {
            let mut w = self.writer();
            w.flush_pkt()?;
            // A server that has gone away already does not need it.
            let _ = self.send(&w.into_inner());
        }
        Ok(())
    }

    fn mode(&self) -> AckMode {
        if self.caps.contains("multi_ack_detailed") {
            AckMode::Detailed
        } else if self.caps.contains("multi_ack") {
            AckMode::Multi
        } else {
            AckMode::Single
        }
    }

    fn no_done(&self) -> bool {
        self.stateless && self.mode() == AckMode::Detailed && self.caps.contains("no-done")
    }

    /// The capability list of the first `want` line.
    fn request_caps(&self, args: &FetchArgs) -> String {
        let has = |c: &str| self.caps.contains(c);
        let mut out: Vec<String> = Vec::new();
        match self.mode() {
            AckMode::Detailed => out.push("multi_ack_detailed".into()),
            AckMode::Multi => out.push("multi_ack".into()),
            AckMode::Single => {}
        }
        if self.no_done() {
            out.push("no-done".into());
        }
        if has("side-band-64k") {
            out.push("side-band-64k".into());
        } else if has("side-band") {
            out.push("side-band".into());
        }
        for (on, name) in [
            (args.thin_pack, "thin-pack"),
            (args.no_progress, "no-progress"),
            (args.include_tag, "include-tag"),
            (args.ofs_delta, "ofs-delta"),
            (args.wants_shallow(), "shallow"),
            (true, "deepen-since"),
            (true, "deepen-not"),
            (args.deepen_relative, "deepen-relative"),
            (args.filter.is_some(), "filter"),
        ] {
            if on && has(name) {
                out.push(name.into());
            }
        }
        if has("agent") {
            out.push(format!("agent={}", self.agent));
        }
        out.extend(crate::session_id::capability(&self.caps));
        if self.format != HashKind::Sha1 {
            out.push(format!("object-format={}", self.format.name()));
        }
        out.join(" ")
    }

    /// Wants, shallow and deepen lines and the flush packet.
    fn write_head(&self, w: &mut Writer<Vec<u8>>, args: &FetchArgs) -> Result<()> {
        let caps = self.request_caps(args);
        for (i, id) in args.wants.iter().enumerate() {
            if i == 0 {
                w.write_line(&format!("want {id} {caps}"))?;
            } else {
                w.write_line(&format!("want {id}"))?;
            }
        }
        for id in &args.shallow {
            w.write_line(&format!("shallow {id}"))?;
        }
        if let Some(depth) = args.deepen {
            w.write_line(&format!("deepen {depth}"))?;
        }
        if let Some(since) = args.deepen_since {
            w.write_line(&format!("deepen-since {since}"))?;
        }
        for rev in &args.deepen_not {
            w.write_line(&format!("deepen-not {rev}"))?;
        }
        if let Some(f) = &args.filter {
            if self.caps.contains("filter") {
                w.write_line(&format!("filter {f}"))?;
            }
        }
        w.flush_pkt()
    }

    fn send(&mut self, bytes: &[u8]) -> Result<()> {
        let conn = self.reader.get_mut();
        conn.write_all(bytes)?;
        conn.end_request()?;
        self.requested = true;
        Ok(())
    }

    fn writer(&self) -> Writer<Vec<u8>> {
        Writer::new(Vec::new(), self.reader.tracer().name().to_owned())
    }

    /// Run the negotiation for `args.wants` and hand the pack data to
    /// `on_pack`. Progress and remote errors go to `progress`.
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
                return Err(Error::ObjectFormatNotSupported);
            }
        }
        if args.wants.is_empty() {
            return Ok(outcome);
        }
        if args.wants_shallow() && !self.caps.contains("shallow") {
            return Err(Error::ShallowNotSupported);
        }
        let deepen = args.deepen.is_some_and(|d| d > 0)
            || args.deepen_since.is_some()
            || !args.deepen_not.is_empty();
        let mut head = self.writer();
        self.write_head(&mut head, args)?;
        let head = head.into_inner();

        let mut st = State::default();
        let ended_with_pack = if self.stateless {
            self.negotiate_stateless(&head, deepen, negotiator, &mut st, &mut outcome)?
        } else {
            self.negotiate_stateful(&head, deepen, negotiator, &mut st, &mut outcome)?
        };
        if !ended_with_pack {
            return Ok(outcome);
        }
        if self.caps.contains("side-band-64k") || self.caps.contains("side-band") {
            let name = self.reader.tracer().name().to_owned();
            self.reader.tracer_mut().set_name("sideband");
            self.reader.chomp_newline(false).err_packets(false);
            let result = read_sideband(&mut self.reader, progress, &mut on_pack);
            self.reader.tracer_mut().set_name(name);
            self.reader.chomp_newline(true).err_packets(true);
            result?;
        } else {
            let conn = self.reader.get_mut();
            let mut buf = vec![0u8; 65536];
            loop {
                let n = match conn.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e.into()),
                };
                on_pack(&buf[..n])?;
            }
        }
        outcome.packfile = true;
        Ok(outcome)
    }

    /// Stateful negotiation; returns whether a pack follows.
    fn negotiate_stateful(
        &mut self,
        head: &[u8],
        deepen: bool,
        negotiator: &mut dyn Negotiator,
        st: &mut State,
        outcome: &mut FetchOutcome,
    ) -> Result<bool> {
        self.send(head)?;
        outcome.rounds += 1;
        if deepen {
            outcome.shallow_info = Some(self.read_shallow_update()?);
        }
        let mut w = self.writer();
        let mut flush_at = INITIAL_HAVES;
        let mut total = 0usize;
        let mut in_flight = 0usize;
        while let Some(id) = negotiator.next_have() {
            w.write_line(&format!("have {id}"))?;
            total += 1;
            st.in_vain += 1;
            if total < flush_at {
                continue;
            }
            w.flush_pkt()?;
            let bytes = std::mem::take(w.get_mut());
            self.send(&bytes)?;
            outcome.rounds += 1;
            in_flight += 1;
            flush_at = next_stateful_flush(flush_at);
            if in_flight > 1 {
                self.read_round(negotiator, st, outcome)?;
                in_flight -= 1;
            }
            if st.single_acked || st.got_ready || (st.got_continue && st.in_vain >= MAX_IN_VAIN) {
                break;
            }
        }
        w.write_line("done")?;
        let bytes = std::mem::take(w.get_mut());
        self.send(&bytes)?;
        if st.single_acked {
            // The server stays silent after its single ACK.
            return Ok(true);
        }
        for _ in 0..in_flight {
            self.read_round(negotiator, st, outcome)?;
            if st.single_acked {
                return Ok(true);
            }
        }
        self.read_final(negotiator, st, outcome)?;
        Ok(true)
    }

    /// Stateless negotiation (one request per round); returns whether a
    /// pack follows.
    fn negotiate_stateless(
        &mut self,
        head: &[u8],
        deepen: bool,
        negotiator: &mut dyn Negotiator,
        st: &mut State,
        outcome: &mut FetchOutcome,
    ) -> Result<bool> {
        let mut flush_at = INITIAL_HAVES;
        let mut total = 0usize;
        loop {
            let mut w = self.writer();
            for id in &outcome.common {
                w.write_line(&format!("have {id}"))?;
            }
            let mut done = st.got_ready || (st.got_continue && st.in_vain >= MAX_IN_VAIN);
            while !done {
                let Some(id) = negotiator.next_have() else {
                    done = true;
                    break;
                };
                w.write_line(&format!("have {id}"))?;
                total += 1;
                st.in_vain += 1;
                if total >= flush_at {
                    break;
                }
            }
            if done {
                w.write_line("done")?;
            } else {
                w.flush_pkt()?;
            }
            let mut req = head.to_vec();
            req.extend_from_slice(w.get_mut());
            self.send(&req)?;
            outcome.rounds += 1;
            if deepen {
                outcome.shallow_info = Some(self.read_shallow_update()?);
            }
            if done {
                self.read_final(negotiator, st, outcome)?;
                return Ok(true);
            }
            self.read_round(negotiator, st, outcome)?;
            if st.single_acked {
                return Ok(true);
            }
            if st.got_ready && self.no_done() {
                self.read_final(negotiator, st, outcome)?;
                return Ok(true);
            }
            flush_at = next_have_count(flush_at);
        }
    }

    /// `shallow`/`unshallow` lines up to a flush packet.
    fn read_shallow_update(&mut self) -> Result<ShallowInfo> {
        let mut info = ShallowInfo::default();
        loop {
            let line = match self.reader.read()? {
                Packet::Flush => return Ok(info),
                Packet::Data(d) => d.to_vec(),
                other => return Err(Error::ExpectedShallowList(other.describe())),
            };
            let bad = || Error::ExpectedShallowList(String::from_utf8_lossy(&line).into_owned());
            if let Some(hex) = line.strip_prefix(b"shallow ") {
                info.shallow
                    .push(ObjectId::from_hex(self.format, hex).map_err(|_| bad())?);
            } else if let Some(hex) = line.strip_prefix(b"unshallow ") {
                info.unshallow
                    .push(ObjectId::from_hex(self.format, hex).map_err(|_| bad())?);
            } else {
                return Err(bad());
            }
        }
    }

    fn read_ack(&mut self) -> Result<Ack> {
        let line = match self.reader.read()? {
            Packet::Data(d) => d.to_vec(),
            other => return Err(Error::ExpectedAckNak(other.describe())),
        };
        let bad = || Error::ExpectedAckNak(String::from_utf8_lossy(&line).into_owned());
        if line == b"NAK" {
            return Ok(Ack::Nak);
        }
        let rest = line.strip_prefix(b"ACK ").ok_or_else(bad)?;
        let hex_len = self.format.hex_len();
        if rest.len() < hex_len {
            return Err(bad());
        }
        let id = ObjectId::from_hex(self.format, &rest[..hex_len]).map_err(|_| bad())?;
        Ok(match &rest[hex_len..] {
            b" continue" | b" common" => Ack::Continue(id, false),
            b" ready" => Ack::Continue(id, true),
            _ => Ack::Final(id),
        })
    }

    fn note_common(
        negotiator: &mut dyn Negotiator,
        st: &mut State,
        outcome: &mut FetchOutcome,
        id: ObjectId,
    ) {
        if negotiator.ack(&id) {
            st.in_vain = 0;
        }
        if !outcome.common.contains(&id) {
            outcome.common.push(id);
        }
    }

    /// The response to one batch of haves: ACKs up to `NAK` (or a single
    /// `ACK <oid>` without `multi_ack`).
    fn read_round(
        &mut self,
        negotiator: &mut dyn Negotiator,
        st: &mut State,
        outcome: &mut FetchOutcome,
    ) -> Result<()> {
        loop {
            match self.read_ack()? {
                Ack::Nak => return Ok(()),
                Ack::Continue(id, ready) => {
                    st.got_continue = true;
                    st.got_ready |= ready;
                    Self::note_common(negotiator, st, outcome, id);
                }
                Ack::Final(id) => {
                    st.single_acked = true;
                    Self::note_common(negotiator, st, outcome, id);
                    return Ok(());
                }
            }
        }
    }

    /// The response to `done`: ACKs of the last haves, then `ACK <oid>` or
    /// `NAK`.
    fn read_final(
        &mut self,
        negotiator: &mut dyn Negotiator,
        st: &mut State,
        outcome: &mut FetchOutcome,
    ) -> Result<()> {
        loop {
            match self.read_ack()? {
                Ack::Nak => return Ok(()),
                Ack::Continue(id, ready) => {
                    st.got_ready |= ready;
                    Self::note_common(negotiator, st, outcome, id);
                }
                Ack::Final(id) => {
                    Self::note_common(negotiator, st, outcome, id);
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
