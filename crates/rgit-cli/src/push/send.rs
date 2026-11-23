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
use rgit_object::ObjectKind;
use rgit_odb::ObjectDatabase;
use rgit_pack::write::{self as pw, ListedObject, Tip, WriteOptions};
use rgit_progress::Progress;
use rgit_protocol::push::{self, Advertisement, Command, Report, Request};
use rgit_protocol::{Error as PError, Progress as RemoteProgress, Reader, SidebandReader, Writer};
use rgit_transport::Connection;

use crate::messages::push as msg;

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
    rgit_protocol::session_id::note_server(&adv.caps);
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
    /// The shallow commits of the local repository: sent as `shallow`
    /// lines, and their parents are not walked.
    pub shallow: Vec<rgit_hash::ObjectId>,
    /// Objects the server has, as delta bases for a thin pack (from
    /// [`pack_objects`]).
    pub bases: Vec<ListedObject>,
    /// Send a thin pack (`--thin`, the default) unless the server says
    /// `no-thin`.
    pub thin: bool,
    /// Pack settings (`pack.*` configuration).
    pub pack: WriteOptions,
    /// Show the local progress meters (`--progress`, or stderr is a
    /// terminal) and the `Total` line.
    pub progress: bool,
}

/// The objects reachable from the new values, minus everything reachable
/// from what the server has (that we have too), in pack order; and for a
/// thin pack the trees and blobs at the edge of what the server has.
/// Computed before anything is sent, so that a failure cannot leave the
/// server waiting for a pack.
pub fn pack_objects(
    out: &Outgoing<'_>,
    adv: &Advertisement,
) -> Result<(Vec<ListedObject>, Vec<ListedObject>), String> {
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
        return Ok((Vec::new(), Vec::new()));
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
    let source = pw::ShallowSource {
        inner: out.odb,
        shallow: out.shallow.iter().copied().collect(),
    };
    let (listed, bases) = if out.thin && !adv.caps.contains("no-thin") {
        // A shallow repository builds its thin pack with the aggressive
        // edge (`rev-list --objects-edge-aggressive`).
        if out.shallow.is_empty() {
            pw::enumerate_with_edges(&source, out.hash, &tips, false)
        } else {
            pw::enumerate_with_edges_aggressive(&source, out.hash, &tips)
        }
        .map_err(|e| e.to_string())?
    } else {
        let listed = pw::enumerate(&source, out.hash, &tips).map_err(|e| e.to_string())?;
        (listed, Vec::new())
    };
    if out.progress {
        show_enumerated(&listed, &bases);
    }
    Ok((pw::pack_order(&listed), bases))
}

/// The `Enumerating objects` and `Counting objects` meters. Like git,
/// both count the objects to send plus the delta bases the other side has
/// under the path of a tree or blob that is sent.
fn show_enumerated(listed: &[ListedObject], bases: &[ListedObject]) {
    let paths: std::collections::HashSet<u32> = listed
        .iter()
        .filter(|o| matches!(o.kind, ObjectKind::Tree | ObjectKind::Blob))
        .map(|o| o.name_hash)
        .collect();
    let n = (listed.len()
        + bases
            .iter()
            .filter(|b| paths.contains(&b.name_hash))
            .count()) as u64;
    let mut meter = Progress::start(msg::PROGRESS_ENUMERATING, 0, true);
    meter.set(n);
    meter.stop();
    let mut meter = Progress::start(msg::PROGRESS_COUNTING, n, true);
    meter.set(n);
    meter.stop();
}

/// A sink that reports the bytes written to the `Writing objects` meter.
struct Metered<'a, W: Write> {
    inner: W,
    bytes: u64,
    meter: &'a mut Progress,
}

impl<W: Write> Write for Metered<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.bytes += n as u64;
        self.meter.throughput(self.bytes);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn write_pack(w: &mut dyn Write, out: &Outgoing<'_>, adv: &Advertisement) -> Result<(), String> {
    let packs = out.odb.packs().map(|p| p.set().packs()).unwrap_or_default();
    let opts = WriteOptions {
        ofs_delta: adv.caps.contains("ofs-delta"),
        ..out.pack.clone()
    };
    if !out.progress {
        let (_, mut bw) = pw::write_thin_pack(
            BufWriter::new(w),
            out.hash,
            &out.objects,
            &out.bases,
            out.odb,
            &packs,
            &opts,
        )
        .map_err(|e| e.to_string())?;
        return bw.flush().map_err(|e| e.to_string());
    }
    let mut meter = Progress::start(msg::PROGRESS_WRITING, out.objects.len() as u64, true);
    let sink = Metered {
        inner: w,
        bytes: 0,
        meter: &mut meter,
    };
    let (summary, mut bw) = pw::write_thin_pack(
        BufWriter::new(sink),
        out.hash,
        &out.objects,
        &out.bases,
        out.odb,
        &packs,
        &opts,
    )
    .map_err(|e| e.to_string())?;
    bw.flush().map_err(|e| e.to_string())?;
    drop(bw);
    meter.set(summary.entries.len() as u64);
    meter.stop();
    eprintln!(
        "{}",
        crate::messages::pack_objects::total(
            summary.entries.len(),
            summary.deltas,
            summary.reused,
            summary.reused_deltas,
        )
    );
    Ok(())
}

fn write_request(w: &mut dyn Write, out: &Outgoing<'_>, adv: &Advertisement) -> Result<(), String> {
    let caps = push::request_capabilities(&adv.caps, &out.request);
    let opts = caps
        .iter()
        .any(|c| c == "push-options")
        .then_some(out.request.push_options.as_slice());
    {
        let mut writer = Writer::new(&mut *w, "push");
        for id in &out.shallow {
            writer
                .write_data(format!("shallow {id}").as_bytes())
                .map_err(|e| e.to_string())?;
        }
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
    let mut sb = SidebandReader::new(Reader::new(r, "push"), RemoteProgress::stderr());
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
