// SPDX-License-Identifier: MIT
//! The `packfile` section: run `pack-objects` and relay its output.
//!
//! Like git's `upload-pack`, the pack is made by a child process:
//!
//! ```text
//! git [--shallow-file ""] pack-objects --revs --stdout [--thin]
//!     [--shallow] [--delta-base-offset] [--include-tag]
//!     (--progress | --quiet) [--filter=<spec>]
//! ```
//!
//! with `--shallow <oid>` lines, the wanted objects, `--not`, the common
//! objects and an empty line on its standard input (arguments and input
//! observed from git 2.56 through `uploadpack.packObjectsHook`). With
//! that hook configured the hook is run through the shell and gets this
//! command line, starting with `git`, as its arguments.
//!
//! The child's standard output goes to the client on sideband 1 and its
//! standard error on sideband 2. The last byte of the pack is held back
//! until the child has exited successfully, so a client never sees a
//! complete pack from a child that failed; then the client gets the
//! reason on sideband 3. While the child is silent, an empty sideband 1
//! packet goes out every `uploadpack.keepAlive` seconds.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use rgit_hash::ObjectId;
use rgit_protocol::sideband::{self, BAND_DATA, BAND_ERROR, BAND_PROGRESS, SidebandMode};

use super::serve::Session;
use super::{Error, Result, messages as msg};

/// What to pack and how.
pub(crate) struct PackRequest {
    /// The objects to send with everything they reach.
    pub tips: Vec<ObjectId>,
    /// Objects the client has.
    pub exclude: Vec<ObjectId>,
    /// Commits to treat as having no parents.
    pub shallow: Vec<ObjectId>,
    /// `thin-pack`.
    pub thin: bool,
    /// `ofs-delta`.
    pub ofs_delta: bool,
    /// `include-tag`.
    pub include_tag: bool,
    /// Progress on sideband 2 (not `no-progress`).
    pub progress: bool,
    /// `filter`, as an expanded spec.
    pub filter: Option<String>,
    /// How the pack travels: on sideband 1 with progress on sideband 2,
    /// or (protocol v0 without `side-band`) as it is, with the progress
    /// on our standard error.
    pub band: Option<SidebandMode>,
}

impl PackRequest {
    /// The arguments after the command that runs git.
    pub fn arguments(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = Vec::new();
        let shallow = !self.shallow.is_empty();
        if shallow {
            // The `--shallow` lines replace the repository's shallow file.
            args.push("--shallow-file".into());
            args.push("".into());
        }
        args.push("pack-objects".into());
        args.push("--revs".into());
        args.push("--stdout".into());
        if self.thin {
            args.push("--thin".into());
        }
        if shallow {
            args.push("--shallow".into());
        }
        if self.ofs_delta {
            args.push("--delta-base-offset".into());
        }
        if self.include_tag {
            args.push("--include-tag".into());
        }
        args.push(
            if self.progress {
                "--progress"
            } else {
                "--quiet"
            }
            .into(),
        );
        if let Some(spec) = &self.filter {
            args.push(format!("--filter={spec}").into());
        }
        args
    }

    /// What the child reads from its standard input.
    pub fn input(&self) -> Vec<u8> {
        let mut text = String::new();
        for id in &self.shallow {
            text.push_str(&format!("--shallow {id}\n"));
        }
        for id in &self.tips {
            text.push_str(&format!("{id}\n"));
        }
        text.push_str("--not\n");
        for id in &self.exclude {
            text.push_str(&format!("{id}\n"));
        }
        text.push('\n');
        text.into_bytes()
    }
}

/// What a reader thread saw.
enum Event {
    Pack(Vec<u8>),
    Progress(Vec<u8>),
    /// One of the two streams ended.
    Closed,
}

fn pump(mut from: impl Read, tx: mpsc::SyncSender<Event>, wrap: fn(Vec<u8>) -> Event) {
    // One sideband packet per read.
    let mut buf = vec![0u8; SidebandMode::Large.max_data()];
    loop {
        match from.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if tx.send(wrap(buf[..n].to_vec())).is_err() {
                    break;
                }
            }
        }
    }
    let _ = tx.send(Event::Closed);
}

/// Run `pack-objects` for `request` and send its output to the client.
pub(crate) fn send<R: Read, W: Write>(
    s: &mut Session<'_, R, W>,
    request: &PackRequest,
) -> Result<()> {
    let args = request.arguments();
    let how = &s.opts.pack_objects;
    let mut command = match &s.settings.pack_objects_hook {
        Some(hook) => {
            // `sh -c '<hook> "$@"' <hook> git pack-objects ...`
            let mut c = Command::new("/bin/sh");
            c.arg("-c")
                .arg(format!("{hook} \"$@\""))
                .arg(hook)
                .arg("git");
            c
        }
        None => {
            let (program, rest) = how.git.split_first().ok_or_else(|| {
                Error::fatal(msg::cannot_run_pack_objects(&std::io::Error::from(
                    std::io::ErrorKind::NotFound,
                )))
            })?;
            let mut c = Command::new(program);
            c.args(rest);
            c
        }
    };
    command
        .args(&args)
        .envs(how.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| Error::fatal(msg::cannot_run_pack_objects(&e)))?;

    let mut stdin = child.stdin.take().expect("piped");
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let input = request.input();
    // A few packets at most wait here: a slow client slows the child
    // down instead of filling memory with the pack.
    let (tx, rx) = mpsc::sync_channel(4);
    let tx_err = tx.clone();
    let feeder = std::thread::spawn(move || {
        // A child that fails early closes its input: not our error.
        let _ = stdin.write_all(&input);
    });
    let out_reader = std::thread::spawn(move || pump(stdout, tx, Event::Pack));
    let err_reader = std::thread::spawn(move || pump(stderr, tx_err, Event::Progress));

    let keep_alive = match s.settings.keep_alive {
        0 => None,
        n => Some(Duration::from_secs(n)),
    };
    let mode = request.band;
    // Only the sideband has room for a packet without data.
    let keep_alive = keep_alive.filter(|_| mode.is_some());
    let mut held: Option<u8> = None;
    let mut open = 2;
    let mut write_error: Option<Error> = None;
    while open > 0 {
        let event = match keep_alive {
            Some(t) => match rx.recv_timeout(t) {
                Ok(e) => Some(e),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            },
            None => match rx.recv() {
                Ok(e) => Some(e),
                Err(_) => break,
            },
        };
        s.opts.active();
        if write_error.is_some() {
            // The client is gone: drain the child so it can finish.
            if let Some(Event::Closed) = event {
                open -= 1;
            }
            continue;
        }
        let w = &mut s.out.writer;
        let written = match event {
            Some(Event::Closed) => {
                open -= 1;
                Ok(())
            }
            Some(Event::Pack(mut data)) => {
                // Keep the last byte until the child's exit status is
                // known.
                let last = data.pop();
                let mut chunk = Vec::with_capacity(data.len() + 1);
                chunk.extend(held.take());
                chunk.extend_from_slice(&data);
                held = last;
                relay(w, mode, BAND_DATA, &chunk)
            }
            Some(Event::Progress(data)) => relay(w, mode, BAND_PROGRESS, &data),
            // Quiet for a while: an empty data packet.
            None => w.write_data(&[BAND_DATA]),
        }
        .and_then(|()| w.flush_io());
        if let Err(e) = written {
            write_error = Some(e.into());
            let _ = child.kill();
        }
    }
    let _ = feeder.join();
    let _ = out_reader.join();
    let _ = err_reader.join();
    let status = child.wait()?;
    if let Some(e) = write_error {
        return Err(e);
    }
    if !status.success() {
        eprintln!("error: {}", msg::PACK_OBJECTS_DIED);
        let w = &mut s.out.writer;
        let _ = relay(w, mode, BAND_ERROR, msg::ABORTING_PACKET.as_bytes());
        let _ = w.flush_io();
        return Err(Error::fatal(msg::ABORTING));
    }
    if let Some(last) = held {
        relay(&mut s.out.writer, mode, BAND_DATA, &[last])?;
    }
    Ok(())
}

/// Send `data` on `band`; without a sideband the pack goes out as it is
/// and everything else to our standard error.
fn relay<W: Write>(
    w: &mut rgit_protocol::Writer<W>,
    mode: Option<SidebandMode>,
    band: u8,
    data: &[u8],
) -> rgit_protocol::Result<()> {
    match mode {
        Some(mode) => sideband::send(w, mode, band, data),
        None if band == BAND_DATA => {
            // Raw bytes after the packets written so far.
            w.flush_io()?;
            w.get_mut().write_all(data)?;
            Ok(())
        }
        None => {
            let _ = std::io::stderr().write_all(data);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgit_hash::HashKind;

    fn id(n: u8) -> ObjectId {
        ObjectId::from_bytes(HashKind::Sha1, &[n; 20]).unwrap()
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    // Arguments and input as git 2.56 passes them to the hook.
    #[test]
    fn plain_clone() {
        let req = PackRequest {
            tips: vec![id(1), id(2)],
            exclude: vec![],
            shallow: vec![],
            thin: true,
            ofs_delta: true,
            include_tag: false,
            progress: false,
            filter: None,
            band: Some(SidebandMode::Large),
        };
        assert_eq!(
            strings(&req.arguments()),
            [
                "pack-objects",
                "--revs",
                "--stdout",
                "--thin",
                "--delta-base-offset",
                "--quiet"
            ]
        );
        assert_eq!(
            String::from_utf8(req.input()).unwrap(),
            format!("{}\n{}\n--not\n\n", id(1), id(2))
        );
    }

    #[test]
    fn shallow_filtered_fetch_with_progress() {
        let req = PackRequest {
            tips: vec![id(1)],
            exclude: vec![id(3)],
            shallow: vec![id(4)],
            thin: true,
            ofs_delta: true,
            include_tag: true,
            progress: true,
            filter: Some("blob:limit=1024".into()),
            band: Some(SidebandMode::Large),
        };
        assert_eq!(
            strings(&req.arguments()),
            [
                "--shallow-file",
                "",
                "pack-objects",
                "--revs",
                "--stdout",
                "--thin",
                "--shallow",
                "--delta-base-offset",
                "--include-tag",
                "--progress",
                "--filter=blob:limit=1024"
            ]
        );
        assert_eq!(
            String::from_utf8(req.input()).unwrap(),
            format!("--shallow {}\n{}\n--not\n{}\n\n", id(4), id(1), id(3))
        );
    }
}
