// SPDX-License-Identifier: MIT
//! Protocol v2: the capability advertisement and the request loop.
//!
//! A request is `command=<name>`, capability lines, then either a flush
//! packet or a delimiter packet followed by the command's arguments and a
//! flush packet. A stateful connection serves requests until the client
//! closes it or sends a lone flush packet; `--stateless-rpc` serves one
//! request and sends no advertisement.

use std::io::{Read, Write};

use rgit_protocol::sideband::BAND_DATA;
use rgit_protocol::{Packet, Reader, Writer};
use rgit_trace::trace2;

use super::{Error, Options, Repository, Result, Settings, Unborn, messages as msg};
use super::{bundle_uri, fetch, ls_refs, object_info, promisor};

/// The commands of protocol v2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    LsRefs,
    Fetch,
    ObjectInfo,
    BundleUri,
}

impl Command {
    fn name(self) -> &'static str {
        match self {
            Command::LsRefs => "ls-refs",
            Command::Fetch => "fetch",
            Command::ObjectInfo => "object-info",
            Command::BundleUri => "bundle-uri",
        }
    }
}

/// One connection.
pub(crate) struct Session<'a, R: Read, W: Write> {
    pub repo: &'a Repository<'a>,
    pub opts: &'a Options,
    pub settings: Settings,
    pub reader: Reader<R>,
    pub out: Output<W>,
    /// The client accepted promisor remotes we advertised: objects
    /// missing here that they promise may be left out of packs.
    pub promisor_accepted: bool,
}

/// The response stream. With `sideband-all` every line goes out on
/// sideband 1.
pub(crate) struct Output<W: Write> {
    pub writer: Writer<W>,
    pub sideband_all: bool,
}

impl<W: Write> Output<W> {
    /// A text line: `line` and LF.
    pub fn line(&mut self, line: &str) -> Result<()> {
        let mut data = Vec::with_capacity(line.len() + 2);
        // Reference names that are not UTF-8 go out as their bytes.
        data.extend_from_slice(&rgit_bstr::to_bytes(line));
        data.push(b'\n');
        self.data(&data)
    }

    /// A packet with `data` as it is.
    pub fn data(&mut self, data: &[u8]) -> Result<()> {
        if self.sideband_all {
            let mut framed = Vec::with_capacity(data.len() + 1);
            framed.push(BAND_DATA);
            framed.extend_from_slice(data);
            self.writer.write_data(&framed)?;
        } else {
            self.writer.write_data(data)?;
        }
        Ok(())
    }

    /// Tell the client about an error: `ERR <msg>`, without LF as git
    /// writes it.
    pub fn error(&mut self, msg: &str) {
        let _ = self.writer.write_data(format!("ERR {msg}").as_bytes());
        let _ = self.writer.flush_io();
    }

    pub fn flush(&mut self) -> Result<()> {
        self.writer.flush_pkt()?;
        Ok(())
    }

    pub fn delim(&mut self) -> Result<()> {
        self.writer.delim_pkt()?;
        Ok(())
    }
}

/// What a request was.
enum Served {
    /// A command ran; the next request may follow.
    Command,
    /// A lone flush packet or the end of input: the client is done.
    Done,
}

pub(crate) fn serve<R: Read, W: Write>(
    repo: &Repository<'_>,
    opts: &Options,
    input: R,
    output: W,
) -> Result<()> {
    let settings = Settings::load(repo, opts)?;
    trace2::data("transfer", "negotiated-version", "2", true);
    let mut reader = Reader::new(input, "upload-pack");
    reader.err_packets(false);
    let mut session = Session {
        repo,
        opts,
        settings,
        reader,
        out: Output {
            writer: Writer::new(output, "upload-pack"),
            sideband_all: false,
        },
        promisor_accepted: false,
    };
    if opts.advertise_refs {
        session.advertise()?;
        session.out.writer.flush_io()?;
        return Ok(());
    }
    if !opts.stateless_rpc {
        session.advertise()?;
        session.out.writer.flush_io()?;
    }
    loop {
        let served = session.request();
        // What was written before a failure still reaches the client.
        let flushed = session.out.writer.flush_io();
        match served? {
            Served::Done => break,
            Served::Command => flushed?,
        }
        if opts.stateless_rpc {
            break;
        }
    }
    Ok(())
}

impl<R: Read, W: Write> Session<'_, R, W> {
    /// `version 2` and the capabilities, in git's order.
    fn advertise(&mut self) -> Result<()> {
        let s = &self.settings;
        let w = &mut self.out.writer;
        w.write_line("version 2")?;
        w.write_line(&format!("agent={}", rgit_protocol::agent::agent()))?;
        w.write_line(match s.unborn {
            Unborn::Advertise => "ls-refs=unborn",
            _ => "ls-refs",
        })?;
        let mut fetch = String::from("fetch=shallow wait-for-done");
        for (on, feature) in [
            (s.allow_filter, " filter"),
            (s.allow_ref_in_want, " ref-in-want"),
            (s.allow_sideband_all, " sideband-all"),
        ] {
            if on {
                fetch.push_str(feature);
            }
        }
        w.write_line(&fetch)?;
        w.write_line("server-option")?;
        w.write_line(&format!("object-format={}", self.repo.hash.name()))?;
        if s.advertise_sid {
            w.write_line(&format!("session-id={}", trace2::session_id()))?;
        }
        if s.advertise_object_info {
            w.write_line("object-info=size type")?;
        }
        if s.advertise_bundle_uris {
            w.write_line("bundle-uri")?;
        }
        if let Some(value) = &s.promisor_remotes {
            w.write_line(&format!("promisor-remote={value}"))?;
        }
        w.flush_pkt()?;
        Ok(())
    }

    fn command(&self, name: &str) -> Option<Command> {
        match name {
            "ls-refs" => Some(Command::LsRefs),
            "fetch" => Some(Command::Fetch),
            "object-info" if self.settings.advertise_object_info => Some(Command::ObjectInfo),
            "bundle-uri" if self.settings.advertise_bundle_uris => Some(Command::BundleUri),
            _ => None,
        }
    }

    /// Check a capability line of a request. Returns the client's object
    /// format if this line names it.
    fn capability(&mut self, line: &str) -> Result<Option<String>> {
        let (key, value) = match line.split_once('=') {
            Some((k, v)) => (k, Some(v)),
            None => (line, None),
        };
        match key {
            "agent" | "server-option" => Ok(None),
            "object-format" => match value {
                Some(v) => Ok(Some(v.to_owned())),
                None => Err(Error::fatal(msg::capability_requires_argument(key))),
            },
            "session-id" if self.settings.advertise_sid => {
                if let Some(sid) = value {
                    trace2::data("transfer", "client-sid", sid, true);
                }
                Ok(None)
            }
            // git knows this capability even when it does not advertise
            // it. The client names the advertised remotes it accepts.
            "promisor-remote" => match value {
                Some(names) => {
                    if self.settings.promisor_remotes.is_some() {
                        let ours = promisor::promisor_remotes(self.repo.config);
                        self.promisor_accepted |= names
                            .split(';')
                            .any(|n| ours.iter().any(|o| promisor::encode(o) == n));
                    }
                    Ok(None)
                }
                None => Err(Error::fatal(msg::capability_requires_argument(key))),
            },
            _ => Err(Error::fatal(msg::unknown_capability(line))),
        }
    }

    /// Read and answer one request.
    fn request(&mut self) -> Result<Served> {
        let mut command: Option<Command> = None;
        let mut client_format: Option<String> = None;
        let mut seen_any = false;
        let has_args = loop {
            self.opts.active();
            let line = match self.reader.read_or_eof()? {
                None if seen_any => return Err(Error::fatal(msg::HUNG_UP)),
                None => return Ok(Served::Done),
                Some(Packet::Flush) if !seen_any => return Ok(Served::Done),
                Some(Packet::Flush) => break false,
                Some(Packet::Delim) => break true,
                Some(Packet::ResponseEnd) => {
                    return Err(Error::fatal(msg::unexpected_line("0002")));
                }
                Some(Packet::Data(d)) => String::from_utf8_lossy(d).into_owned(),
            };
            seen_any = true;
            if let Some(name) = line.strip_prefix("command=") {
                if let Some(old) = command {
                    return Err(Error::fatal(msg::command_after_command(name, old.name())));
                }
                command = Some(
                    self.command(name)
                        .ok_or_else(|| Error::fatal(msg::invalid_command(name)))?,
                );
            } else if let Some(format) = self.capability(&line)? {
                client_format = Some(format);
            }
        };
        let Some(command) = command else {
            return Err(Error::fatal(msg::NO_COMMAND));
        };
        // A client that names no format means SHA-1.
        let client_format = client_format.as_deref().unwrap_or("sha1");
        if client_format != self.repo.hash.name() {
            return Err(Error::fatal(msg::mismatched_object_format(
                self.repo.hash.name(),
                client_format,
            )));
        }
        let args = self.arguments(has_args)?;
        match command {
            Command::LsRefs => ls_refs::run(self, &args)?,
            Command::Fetch => fetch::run(self, &args)?,
            Command::ObjectInfo => object_info::run(self, &args)?,
            Command::BundleUri => bundle_uri::run(self, &args)?,
        }
        Ok(Served::Command)
    }

    /// The argument lines of a command, up to the flush packet.
    fn arguments(&mut self, has_args: bool) -> Result<Vec<String>> {
        let mut args = Vec::new();
        if !has_args {
            return Ok(args);
        }
        loop {
            self.opts.active();
            match self.reader.read_or_eof()? {
                Some(Packet::Flush) => return Ok(args),
                Some(Packet::Data(d)) => args.push(String::from_utf8_lossy(d).into_owned()),
                // The lines so far are checked first, as git reads and
                // checks line by line.
                Some(_) => {
                    args.push(END_WITHOUT_FLUSH.to_owned());
                    return Ok(args);
                }
                None => return Err(Error::fatal(msg::HUNG_UP)),
            }
        }
    }
}

/// A marker at the end of the argument list: the arguments ended with a
/// packet other than a flush packet. Commands report it with
/// [`msg::expected_flush_after`] when they reach it.
pub(crate) const END_WITHOUT_FLUSH: &str = "\0end-without-flush";
