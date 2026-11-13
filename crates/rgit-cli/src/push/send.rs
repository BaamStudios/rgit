// SPDX-License-Identifier: MIT
//! The `receive-pack` exchange over an `rgit-transport` connection: read
//! the advertisement, send the commands and the pack, read the status
//! report (through sideband channel 1 when `side-band-64k` is in use, with
//! channel 2 shown as `remote: ...`).
//!
//! On a stateful connection (local, SSH) the pack is written while another
//! thread reads the server's output, so progress messages cannot block
//! the server. On a stateless one (smart HTTP) the whole request is
//! buffered and sent as one POST.

use std::io::{BufWriter, Read, Write};

use rgit_hash::HashKind;
use rgit_odb::ObjectDatabase;
use rgit_pack::write::{self as pw, ListedObject, Tip, WriteOptions};
use rgit_protocol::push::{self, Advertisement, Command, Report, Request};
use rgit_protocol::{Error as PError, Progress, Reader, SidebandReader, Writer};
use rgit_transport::Connection;

/// An open connection with its advertisement.
pub struct Session {
    pub conn: Box<dyn Connection>,
    pub adv: Advertisement,
}

/// Read the advertisement from a fresh connection.
pub fn start(mut conn: Box<dyn Connection>, hash: HashKind) -> Result<Session, PError> {
    let adv = {
        let mut reader = Reader::new(&mut conn, "push");
        push::read_advertisement(&mut reader, hash)?
    };
    Ok(Session { conn, adv })
}

/// What to send.
pub struct Outgoing<'a> {
    pub commands: Vec<Command>,
    pub request: Request,
    pub odb: &'a ObjectDatabase,
    pub hash: HashKind,
    /// From [`pack_objects`].
    pub objects: Vec<ListedObject>,
}

/// The objects reachable from the new values, minus everything reachable
/// from what the server has (that we have too), in pack order. Computed
/// before anything is sent, so that a failure cannot leave the server
/// waiting for a pack.
pub fn pack_objects(out: &Outgoing<'_>, adv: &Advertisement) -> Result<Vec<ListedObject>, String> {
    let mut tips: Vec<Tip> = out
        .commands
        .iter()
        .filter(|c| !c.new.is_null())
        .map(|c| Tip {
            id: c.new,
            exclude: false,
        })
        .collect();
    if tips.is_empty() {
        return Ok(Vec::new());
    }
    let mut seen = std::collections::HashSet::new();
    for id in adv
        .refs
        .iter()
        .map(|r| r.oid)
        .chain(adv.haves.iter().copied())
    {
        if seen.insert(id) && out.odb.contains(&id) {
            tips.push(Tip { id, exclude: true });
        }
    }
    let listed = pw::enumerate(out.odb, out.hash, &tips).map_err(|e| e.to_string())?;
    Ok(pw::pack_order(&listed))
}

fn write_pack(w: &mut dyn Write, out: &Outgoing<'_>, adv: &Advertisement) -> Result<(), String> {
    let packs = out.odb.packs().map(|p| p.set().packs()).unwrap_or_default();
    let opts = WriteOptions {
        ofs_delta: adv.caps.contains("ofs-delta"),
        ..WriteOptions::default()
    };
    let (_, mut bw) = pw::write_pack(
        BufWriter::new(w),
        out.hash,
        &out.objects,
        out.odb,
        &packs,
        &opts,
    )
    .map_err(|e| e.to_string())?;
    bw.flush().map_err(|e| e.to_string())
}

fn write_request(w: &mut dyn Write, out: &Outgoing<'_>, adv: &Advertisement) -> Result<(), String> {
    let caps = push::request_capabilities(&adv.caps, &out.request);
    let opts = caps
        .iter()
        .any(|c| c == "push-options")
        .then_some(out.request.push_options.as_slice());
    {
        let mut writer = Writer::new(&mut *w, "push");
        push::write_commands(&mut writer, &out.commands, &caps, opts).map_err(|e| e.to_string())?;
    }
    if out.commands.iter().any(|c| !c.new.is_null()) {
        write_pack(w, out, adv)?;
    }
    w.flush().map_err(|e| e.to_string())
}

/// Read the report (if one was asked for) and the rest of the server's
/// output.
fn read_response(r: &mut dyn Read, sideband: bool, report: bool) -> Result<Option<Report>, PError> {
    if !sideband {
        if !report {
            return Ok(None);
        }
        let mut reader = Reader::new(r, "push");
        return push::read_report(&mut reader).map(Some);
    }
    let mut sb = SidebandReader::new(Reader::new(r, "push"), Progress::stderr());
    let result = if report {
        let mut inner = Reader::new(&mut sb, "push");
        let rep = push::read_report(&mut inner);
        drop(inner);
        rep.map(Some)
    } else {
        Ok(None)
    };
    if let Err(e) = &result {
        if let Some(inner) = sb.take_error() {
            return Err(inner);
        }
        if !matches!(e, PError::Io(_)) {
            return result;
        }
    }
    // Remaining progress (post-receive hooks) until the end of the stream.
    loop {
        match sb.next_data() {
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(e) => {
                if result.is_ok() {
                    return Err(e);
                }
                break;
            }
        }
    }
    result
}

/// Send `out` and return the server's report (`None` without
/// `report-status`). Errors are git's messages.
pub fn exchange(session: &mut Session, out: &Outgoing<'_>) -> Result<Option<Report>, String> {
    let adv = &session.adv;
    let sideband = adv.caps.contains("side-band-64k");
    let report = adv.caps.contains("report-status-v2") || adv.caps.contains("report-status");
    let conn = &mut session.conn;
    if conn.is_stateless() {
        write_request(conn, out, adv)?;
        conn.end_request().map_err(|e| e.to_string())?;
        return read_response(conn, sideband, report).map_err(|e| e.to_string());
    }
    let (r, w) = conn.split();
    std::thread::scope(|s| {
        let reader = s.spawn(move || read_response(r, sideband, report));
        let written = write_request(w, out, adv);
        let read = reader
            .join()
            .unwrap_or_else(|_| Err(PError::Unexpected("reader thread".into())));
        written?;
        read.map_err(|e| e.to_string())
    })
}

/// End a session without updates: a flush packet, then close.
pub fn finish_empty(mut session: Session) {
    let _ = session.conn.write_all(b"0000");
    let _ = session.conn.end_request();
    let _ = session.conn.close();
}

/// Turn a protocol error while reading the advertisement into git's
/// message.
pub fn advertisement_error(e: &PError) -> String {
    match e {
        PError::HungUp => crate::messages::push::COULD_NOT_READ.to_owned(),
        other => other.to_string(),
    }
}
