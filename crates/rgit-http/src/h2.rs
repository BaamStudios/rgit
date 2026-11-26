// SPDX-License-Identifier: MIT
//! A blocking HTTP/2 client connection (RFC 9113).
//!
//! rgit sends one request at a time, so a connection carries at most one
//! open stream: the request goes out as `HEADERS` (+ `CONTINUATION`) and
//! `DATA` frames within the peer's flow-control windows, and the response
//! is read frame by frame while the caller consumes the body. Frames of
//! other streams are decoded (to keep the HPACK state in sync) and dropped.
//! `PING`s are answered, `SETTINGS` acknowledged, and `WINDOW_UPDATE`s
//! sent as data arrives. A `GOAWAY` or `RST_STREAM(REFUSED_STREAM)` that
//! covers the open stream before its response started means the request
//! was not processed and may be sent again on a new connection
//! ([`H2Error::Refused`]).
//!
//! Server push is disabled (`SETTINGS_ENABLE_PUSH = 0`).

use std::io::{self, BufReader, Read, Write};

use crate::hpack::{Decoder, Encoder};

/// The connection preface (RFC 9113, 3.4).
const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

const DATA: u8 = 0x0;
const HEADERS: u8 = 0x1;
const RST_STREAM: u8 = 0x3;
const SETTINGS: u8 = 0x4;
const PUSH_PROMISE: u8 = 0x5;
const PING: u8 = 0x6;
const GOAWAY: u8 = 0x7;
const WINDOW_UPDATE: u8 = 0x8;
const CONTINUATION: u8 = 0x9;

const END_STREAM: u8 = 0x1;
const ACK: u8 = 0x1;
const END_HEADERS: u8 = 0x4;
const PADDED: u8 = 0x8;
const PRIORITY: u8 = 0x20;

const SETTINGS_ENABLE_PUSH: u16 = 0x2;
const SETTINGS_MAX_CONCURRENT_STREAMS: u16 = 0x3;
const SETTINGS_INITIAL_WINDOW_SIZE: u16 = 0x4;
const SETTINGS_MAX_FRAME_SIZE: u16 = 0x5;

/// `REFUSED_STREAM`: the request was not processed.
const REFUSED_STREAM: u32 = 0x7;

/// The default window and frame size before any `SETTINGS`.
const DEFAULT_WINDOW: i64 = 65_535;
const DEFAULT_MAX_FRAME: usize = 16_384;
/// The largest frame payload accepted (our `SETTINGS_MAX_FRAME_SIZE`
/// stays at the default).
const OUR_MAX_FRAME: usize = DEFAULT_MAX_FRAME;
/// Our per-stream receive window and the connection window.
const OUR_STREAM_WINDOW: u32 = 1 << 20;
const OUR_CONN_WINDOW: u32 = 1 << 24;
/// Send `WINDOW_UPDATE` once this much has arrived.
const WINDOW_REFILL: u32 = 1 << 18;
/// The largest header block accepted.
const MAX_HEADER_BLOCK: usize = 1 << 20;

/// The payload of the `SETTINGS` this client sends (also the value of
/// `HTTP2-Settings` in an upgrade): 100 streams, a 1 MiB stream window,
/// no server push.
pub(crate) fn our_settings() -> Vec<u8> {
    let mut p = Vec::new();
    for (id, v) in [
        (SETTINGS_MAX_CONCURRENT_STREAMS, 100u32),
        (SETTINGS_INITIAL_WINDOW_SIZE, OUR_STREAM_WINDOW),
        (SETTINGS_ENABLE_PUSH, 0),
    ] {
        p.extend_from_slice(&id.to_be_bytes());
        p.extend_from_slice(&v.to_be_bytes());
    }
    p
}

/// The name of an HTTP/2 error code (RFC 9113, 7).
pub(crate) fn error_name(code: u32) -> &'static str {
    match code {
        0x0 => "NO_ERROR",
        0x1 => "PROTOCOL_ERROR",
        0x2 => "INTERNAL_ERROR",
        0x3 => "FLOW_CONTROL_ERROR",
        0x4 => "SETTINGS_TIMEOUT",
        0x5 => "STREAM_CLOSED",
        0x6 => "FRAME_SIZE_ERROR",
        0x7 => "REFUSED_STREAM",
        0x8 => "CANCEL",
        0x9 => "COMPRESSION_ERROR",
        0xa => "CONNECT_ERROR",
        0xb => "ENHANCE_YOUR_CALM",
        0xc => "INADEQUATE_SECURITY",
        0xd => "HTTP_1_1_REQUIRED",
        _ => "unknown",
    }
}

/// Why an exchange failed.
#[derive(Debug)]
pub(crate) enum H2Error {
    /// The socket failed.
    Io(io::Error),
    /// Reading the request body failed.
    Body(io::Error),
    /// The server did not process the request (`GOAWAY` below our stream,
    /// `REFUSED_STREAM`, or the connection closed before any response):
    /// it may be sent again on a new connection.
    Refused,
    /// The server reset our stream.
    Reset { stream: u32, code: u32 },
    /// The peer broke the protocol.
    Protocol(String),
}

impl From<io::Error> for H2Error {
    fn from(e: io::Error) -> H2Error {
        H2Error::Io(e)
    }
}

/// Header fields (lower-case names).
pub(crate) type Fields = [(String, String)];

/// A response head.
pub(crate) struct Head {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    /// The stream ended with the headers (no body).
    pub end_stream: bool,
}

struct Frame {
    kind: u8,
    flags: u8,
    stream: u32,
    payload: Vec<u8>,
}

/// What a frame meant for the open stream.
enum Event {
    /// A complete header block of the open stream.
    Headers {
        fields: Vec<(String, String)>,
        end_stream: bool,
    },
    /// Body bytes of the open stream (possibly empty with `end_stream`).
    Data { data: Vec<u8>, end_stream: bool },
    /// Anything else (connection frames, other streams).
    Other,
}

/// One HTTP/2 connection.
pub(crate) struct H2Conn<T: Read + Write> {
    pub(crate) io: BufReader<T>,
    enc: Encoder,
    dec: Decoder,
    started: bool,
    next_stream: u32,
    /// The open stream (0: none yet).
    stream: u32,
    stream_done: bool,
    /// The open stream's response head arrived.
    got_head: bool,
    /// A frame read but not yet handled (the response started while the
    /// request body was still being sent).
    held: Option<Frame>,
    conn_send_window: i64,
    stream_send_window: i64,
    peer_initial_window: i64,
    peer_max_frame: usize,
    conn_recv_pending: u32,
    stream_recv_pending: u32,
    /// The last stream the server will process (`GOAWAY`).
    goaway: Option<u32>,
    /// The connection cannot carry more requests.
    broken: bool,
}

/// Strip the padding of a `PADDED` frame.
fn unpad(flags: u8, payload: &[u8]) -> Result<&[u8], H2Error> {
    if flags & PADDED == 0 {
        return Ok(payload);
    }
    let (&pad, rest) = payload
        .split_first()
        .ok_or_else(|| H2Error::Protocol("bad padding".into()))?;
    let pad = pad as usize;
    if pad > rest.len() {
        return Err(H2Error::Protocol("bad padding".into()));
    }
    Ok(&rest[..rest.len() - pad])
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

impl<T: Read + Write> H2Conn<T> {
    /// A connection on which nothing has been sent yet (ALPN `h2`).
    pub(crate) fn new(io: BufReader<T>) -> H2Conn<T> {
        H2Conn {
            io,
            enc: Encoder,
            dec: Decoder::new(4096),
            started: false,
            next_stream: 1,
            stream: 0,
            stream_done: true,
            got_head: false,
            held: None,
            conn_send_window: DEFAULT_WINDOW,
            stream_send_window: DEFAULT_WINDOW,
            peer_initial_window: DEFAULT_WINDOW,
            peer_max_frame: DEFAULT_MAX_FRAME,
            conn_recv_pending: 0,
            stream_recv_pending: 0,
            goaway: None,
            broken: false,
        }
    }

    /// A connection after `101 Switching Protocols` (RFC 9113, 3.1 and
    /// RFC 7540, 3.2): the request was stream 1, which is now half-closed
    /// (local). Sends the preface right away.
    pub(crate) fn upgraded(io: BufReader<T>) -> Result<H2Conn<T>, H2Error> {
        let mut c = H2Conn::new(io);
        c.stream = 1;
        c.stream_done = false;
        c.next_stream = 3;
        c.start()?;
        Ok(c)
    }

    /// Whether another request may be sent on this connection.
    pub(crate) fn usable(&self) -> bool {
        !self.broken && self.goaway.is_none() && self.stream_done && self.next_stream < (1 << 30)
    }

    /// Bytes already received and not yet processed.
    pub(crate) fn buffered(&self) -> usize {
        self.io.buffer().len()
    }

    /// Handle frames that are already buffered (a `GOAWAY` or `PING` that
    /// arrived while the connection was idle in the pool).
    pub(crate) fn process_idle(&mut self) -> Result<(), H2Error> {
        while self.buffered() >= 9 {
            let f = self.read_frame()?;
            self.handle(f)?;
        }
        Ok(())
    }

    fn frame(&mut self, kind: u8, flags: u8, stream: u32, payload: &[u8]) -> io::Result<()> {
        let mut f = Vec::with_capacity(9 + payload.len());
        f.extend_from_slice(&(payload.len() as u32).to_be_bytes()[1..]);
        f.push(kind);
        f.push(flags);
        f.extend_from_slice(&(stream & 0x7fff_ffff).to_be_bytes());
        f.extend_from_slice(payload);
        self.io.get_mut().write_all(&f)
    }

    /// The preface, our `SETTINGS` and the larger connection window.
    fn start(&mut self) -> Result<(), H2Error> {
        if self.started {
            return Ok(());
        }
        self.started = true;
        self.io.get_mut().write_all(PREFACE)?;
        self.frame(SETTINGS, 0, 0, &our_settings())?;
        let inc = OUR_CONN_WINDOW - DEFAULT_WINDOW as u32;
        self.frame(WINDOW_UPDATE, 0, 0, &inc.to_be_bytes())?;
        self.io.get_mut().flush()?;
        Ok(())
    }

    /// Send a request on a new stream. `fields` are the pseudo-headers and
    /// headers in order (lower-case names). With `has_body`, `body` is
    /// called to append more request body to its argument and returns true
    /// at the end; `on_data` sees every DATA payload sent.
    pub(crate) fn send_request(
        &mut self,
        fields: &[(String, String)],
        has_body: bool,
        body: &mut dyn FnMut(&mut Vec<u8>) -> io::Result<bool>,
        on_data: &mut dyn FnMut(&[u8]),
    ) -> Result<u32, H2Error> {
        self.start()?;
        let id = self.next_stream;
        self.next_stream += 2;
        self.stream = id;
        self.stream_done = false;
        self.got_head = false;
        self.stream_send_window = self.peer_initial_window;
        self.stream_recv_pending = 0;
        let block = self.enc.encode(fields);
        let max = self.peer_max_frame;
        let mut chunks = block.chunks(max).peekable();
        let first = chunks.next().unwrap_or(&[]);
        let mut flags = if has_body { 0 } else { END_STREAM };
        if chunks.peek().is_none() {
            flags |= END_HEADERS;
        }
        self.frame(HEADERS, flags, id, first)?;
        while let Some(c) = chunks.next() {
            let f = if chunks.peek().is_none() {
                END_HEADERS
            } else {
                0
            };
            self.frame(CONTINUATION, f, id, c)?;
        }
        if has_body {
            let mut pending: Vec<u8> = Vec::new();
            let mut eof = false;
            loop {
                while pending.is_empty() && !eof {
                    eof = body(&mut pending).map_err(H2Error::Body)?;
                }
                if pending.is_empty() {
                    self.frame(DATA, END_STREAM, id, &[])?;
                    break;
                }
                let window = self.conn_send_window.min(self.stream_send_window);
                if window <= 0 {
                    self.io.get_mut().flush()?;
                    if self.wait_for_window(id)? {
                        // The response started: the rest of the upload is
                        // not wanted.
                        return Ok(id);
                    }
                    continue;
                }
                let n = pending.len().min(window as usize).min(max);
                let last = eof && n == pending.len();
                let chunk: Vec<u8> = pending.drain(..n).collect();
                self.frame(DATA, if last { END_STREAM } else { 0 }, id, &chunk)?;
                on_data(&chunk);
                self.conn_send_window -= n as i64;
                self.stream_send_window -= n as i64;
                if last {
                    break;
                }
            }
        }
        self.io.get_mut().flush()?;
        Ok(id)
    }

    /// Read frames until a send window opens. True if the response of
    /// `id` started instead.
    fn wait_for_window(&mut self, id: u32) -> Result<bool, H2Error> {
        while self.conn_send_window <= 0 || self.stream_send_window <= 0 {
            let f = self.read_frame()?;
            if f.stream == id && f.kind == HEADERS {
                self.held = Some(f);
                // The stream is half-open for good: do not reuse.
                self.broken = true;
                return Ok(true);
            }
            if let Event::Data { .. } = self.handle(f)? {
                return Err(H2Error::Protocol("DATA before HEADERS".into()));
            }
        }
        Ok(false)
    }

    fn eof(&self) -> H2Error {
        if !self.stream_done && !self.got_head {
            H2Error::Refused
        } else {
            H2Error::Protocol("connection closed".into())
        }
    }

    fn read_frame(&mut self) -> Result<Frame, H2Error> {
        if let Some(f) = self.held.take() {
            return Ok(f);
        }
        let mut h = [0u8; 9];
        if let Err(e) = self.io.read_exact(&mut h) {
            self.broken = true;
            return Err(if e.kind() == io::ErrorKind::UnexpectedEof {
                self.eof()
            } else {
                H2Error::Io(e)
            });
        }
        let len = u32::from_be_bytes([0, h[0], h[1], h[2]]) as usize;
        if len > OUR_MAX_FRAME {
            self.broken = true;
            return Err(H2Error::Protocol(format!("frame of {len} bytes too large")));
        }
        let mut payload = vec![0u8; len];
        if let Err(e) = self.io.read_exact(&mut payload) {
            self.broken = true;
            return Err(if e.kind() == io::ErrorKind::UnexpectedEof {
                self.eof()
            } else {
                H2Error::Io(e)
            });
        }
        Ok(Frame {
            kind: h[3],
            flags: h[4],
            stream: be32(&h[5..9]) & 0x7fff_ffff,
            payload,
        })
    }

    /// The rest of a header block: `CONTINUATION` frames up to
    /// `END_HEADERS`.
    fn header_block(&mut self, stream: u32, flags: u8, first: &[u8]) -> Result<Vec<u8>, H2Error> {
        let mut block = first.to_vec();
        let mut end = flags & END_HEADERS != 0;
        while !end {
            let f = self.read_frame()?;
            if f.kind != CONTINUATION || f.stream != stream {
                self.broken = true;
                return Err(H2Error::Protocol("expected CONTINUATION".into()));
            }
            block.extend_from_slice(&f.payload);
            if block.len() > MAX_HEADER_BLOCK {
                self.broken = true;
                return Err(H2Error::Protocol("header block too large".into()));
            }
            end = f.flags & END_HEADERS != 0;
        }
        Ok(block)
    }

    fn decode(&mut self, block: &[u8]) -> Result<Vec<(String, String)>, H2Error> {
        match self.dec.decode(block) {
            Ok(fields) => Ok(fields
                .into_iter()
                .map(|(n, v)| {
                    (
                        String::from_utf8_lossy(&n).into_owned(),
                        String::from_utf8_lossy(&v).into_owned(),
                    )
                })
                .collect()),
            Err(e) => {
                self.broken = true;
                Err(H2Error::Protocol(format!("HPACK: {}", e.0)))
            }
        }
    }

    fn refill(&mut self, n: u32, ours: bool, end_stream: bool) -> Result<(), H2Error> {
        self.conn_recv_pending += n;
        let mut wrote = false;
        if self.conn_recv_pending >= WINDOW_REFILL {
            let inc = std::mem::take(&mut self.conn_recv_pending);
            self.frame(WINDOW_UPDATE, 0, 0, &inc.to_be_bytes())?;
            wrote = true;
        }
        if ours && !end_stream {
            self.stream_recv_pending += n;
            if self.stream_recv_pending >= WINDOW_REFILL {
                let inc = std::mem::take(&mut self.stream_recv_pending);
                let id = self.stream;
                self.frame(WINDOW_UPDATE, 0, id, &inc.to_be_bytes())?;
                wrote = true;
            }
        }
        if wrote {
            self.io.get_mut().flush()?;
        }
        Ok(())
    }

    /// Process one frame.
    fn handle(&mut self, f: Frame) -> Result<Event, H2Error> {
        let ours = f.stream == self.stream && f.stream != 0 && !self.stream_done;
        match f.kind {
            DATA => {
                let end_stream = f.flags & END_STREAM != 0;
                self.refill(f.payload.len() as u32, ours, end_stream)?;
                if !ours {
                    return Ok(Event::Other);
                }
                let data = unpad(f.flags, &f.payload)?.to_vec();
                Ok(Event::Data { data, end_stream })
            }
            HEADERS => {
                let mut p = unpad(f.flags, &f.payload)?;
                if f.flags & PRIORITY != 0 {
                    if p.len() < 5 {
                        return Err(H2Error::Protocol("short HEADERS".into()));
                    }
                    p = &p[5..];
                }
                let block = self.header_block(f.stream, f.flags, p)?;
                let fields = self.decode(&block)?;
                if !ours {
                    return Ok(Event::Other);
                }
                Ok(Event::Headers {
                    fields,
                    end_stream: f.flags & END_STREAM != 0,
                })
            }
            PUSH_PROMISE => {
                // Push is disabled; keep the HPACK state in sync anyway.
                let p = unpad(f.flags, &f.payload)?;
                let p = p.get(4..).unwrap_or(&[]);
                let block = self.header_block(f.stream, f.flags, p)?;
                self.decode(&block)?;
                Ok(Event::Other)
            }
            RST_STREAM => {
                if ours && f.payload.len() >= 4 {
                    let code = be32(&f.payload);
                    self.stream_done = true;
                    if code == REFUSED_STREAM && !self.got_head {
                        return Err(H2Error::Refused);
                    }
                    return Err(H2Error::Reset {
                        stream: f.stream,
                        code,
                    });
                }
                Ok(Event::Other)
            }
            SETTINGS => {
                if f.flags & ACK != 0 || f.stream != 0 {
                    return Ok(Event::Other);
                }
                for s in f.payload.as_chunks::<6>().0 {
                    let id = u16::from_be_bytes([s[0], s[1]]);
                    let v = be32(&s[2..6]);
                    match id {
                        SETTINGS_INITIAL_WINDOW_SIZE => {
                            let v = i64::from(v);
                            self.stream_send_window += v - self.peer_initial_window;
                            self.peer_initial_window = v;
                        }
                        SETTINGS_MAX_FRAME_SIZE => {
                            self.peer_max_frame = (v as usize).clamp(DEFAULT_MAX_FRAME, 1 << 24);
                        }
                        _ => {}
                    }
                }
                self.frame(SETTINGS, ACK, 0, &[])?;
                self.io.get_mut().flush()?;
                Ok(Event::Other)
            }
            PING => {
                if f.flags & ACK == 0 && f.payload.len() == 8 {
                    self.frame(PING, ACK, 0, &f.payload)?;
                    self.io.get_mut().flush()?;
                }
                Ok(Event::Other)
            }
            GOAWAY => {
                let last = f.payload.get(..4).map_or(0, |b| be32(b) & 0x7fff_ffff);
                self.goaway = Some(last);
                if !self.stream_done && self.stream > last && !self.got_head {
                    self.stream_done = true;
                    return Err(H2Error::Refused);
                }
                Ok(Event::Other)
            }
            WINDOW_UPDATE => {
                if f.payload.len() >= 4 {
                    let inc = i64::from(be32(&f.payload) & 0x7fff_ffff);
                    if f.stream == 0 {
                        self.conn_send_window += inc;
                    } else if f.stream == self.stream {
                        self.stream_send_window += inc;
                    }
                }
                Ok(Event::Other)
            }
            CONTINUATION => {
                self.broken = true;
                Err(H2Error::Protocol("unexpected CONTINUATION".into()))
            }
            // PRIORITY and unknown frame types are ignored.
            _ => Ok(Event::Other),
        }
    }

    /// Read the response head of the open stream (interim `1xx` responses
    /// are passed to `on_interim` and skipped).
    pub(crate) fn read_head(
        &mut self,
        on_interim: &mut dyn FnMut(u16, &Fields),
    ) -> Result<Head, H2Error> {
        loop {
            let f = self.read_frame()?;
            match self.handle(f)? {
                Event::Other => {}
                Event::Data { .. } => {
                    self.broken = true;
                    return Err(H2Error::Protocol("DATA before HEADERS".into()));
                }
                Event::Headers { fields, end_stream } => {
                    let status = fields
                        .iter()
                        .find(|(k, _)| k == ":status")
                        .and_then(|(_, v)| v.parse::<u16>().ok())
                        .ok_or_else(|| H2Error::Protocol("missing :status".into()))?;
                    let headers: Vec<(String, String)> = fields
                        .into_iter()
                        .filter(|(k, _)| !k.starts_with(':'))
                        .collect();
                    if (100..200).contains(&status) && !end_stream {
                        on_interim(status, &headers);
                        continue;
                    }
                    self.got_head = true;
                    if end_stream {
                        self.stream_done = true;
                    }
                    return Ok(Head {
                        status,
                        headers,
                        end_stream,
                    });
                }
            }
        }
    }

    /// The next piece of the response body; `None` at its end.
    pub(crate) fn read_data(&mut self) -> Result<Option<Vec<u8>>, H2Error> {
        loop {
            if self.stream_done {
                return Ok(None);
            }
            let f = self.read_frame()?;
            match self.handle(f)? {
                Event::Other => {}
                Event::Data { data, end_stream } => {
                    if end_stream {
                        self.stream_done = true;
                    }
                    if !data.is_empty() {
                        return Ok(Some(data));
                    }
                }
                // Trailers.
                Event::Headers { end_stream, .. } => {
                    if end_stream {
                        self.stream_done = true;
                    }
                }
            }
        }
    }

    /// Abandon the open stream (the caller stopped reading the body).
    pub(crate) fn abandon(&mut self) {
        if !self.stream_done && self.stream != 0 {
            let id = self.stream;
            const CANCEL: u32 = 0x8;
            if self.frame(RST_STREAM, 0, id, &CANCEL.to_be_bytes()).is_ok() {
                let _ = self.io.get_mut().flush();
            }
            self.stream_done = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake socket: reads from `input`, records what is written.
    struct Fake {
        input: io::Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Read for Fake {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.input.read(buf)
        }
    }

    impl Write for Fake {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn frame(kind: u8, flags: u8, stream: u32, payload: &[u8]) -> Vec<u8> {
        let mut f = (payload.len() as u32).to_be_bytes()[1..].to_vec();
        f.push(kind);
        f.push(flags);
        f.extend_from_slice(&stream.to_be_bytes());
        f.extend_from_slice(payload);
        f
    }

    fn conn(input: Vec<u8>) -> H2Conn<Fake> {
        H2Conn::new(BufReader::new(Fake {
            input: io::Cursor::new(input),
            output: Vec::new(),
        }))
    }

    /// Split written bytes (after the preface) into frames.
    fn written(c: &H2Conn<Fake>) -> Vec<(u8, u8, u32, Vec<u8>)> {
        let mut out = c.io.get_ref().output.as_slice();
        out = out.strip_prefix(PREFACE).unwrap();
        let mut v = Vec::new();
        while out.len() >= 9 {
            let len = u32::from_be_bytes([0, out[0], out[1], out[2]]) as usize;
            v.push((out[3], out[4], be32(&out[5..9]), out[9..9 + len].to_vec()));
            out = &out[9 + len..];
        }
        v
    }

    #[test]
    fn get_with_settings_ping_and_body() {
        let mut input = frame(SETTINGS, 0, 0, &[0, 5, 0, 0, 0x40, 0]);
        input.extend(frame(PING, 0, 0, b"12345678"));
        // :status 200 (indexed), content-type: x (literal, indexed name 31).
        input.extend(frame(
            HEADERS,
            END_HEADERS,
            1,
            &[0x88, 0x0f, 0x10, 0x01, b'x'],
        ));
        input.extend(frame(DATA, PADDED, 1, &[2, b'h', b'i', 0, 0]));
        input.extend(frame(DATA, END_STREAM, 1, b"!"));
        let mut c = conn(input);
        let fields = vec![(":method".to_string(), "GET".to_string())];
        let id = c
            .send_request(&fields, false, &mut |_| Ok(true), &mut |_| {})
            .unwrap();
        assert_eq!(id, 1);
        let head = c.read_head(&mut |_, _| {}).unwrap();
        assert_eq!(head.status, 200);
        assert_eq!(head.headers, vec![("content-type".into(), "x".into())]);
        assert_eq!(c.read_data().unwrap().unwrap(), b"hi");
        assert_eq!(c.read_data().unwrap().unwrap(), b"!");
        assert_eq!(c.read_data().unwrap(), None);
        assert!(c.usable());
        let w = written(&c);
        assert_eq!(w[0].0, SETTINGS);
        assert_eq!(w[1].0, WINDOW_UPDATE);
        assert_eq!((w[2].0, w[2].1), (HEADERS, END_HEADERS | END_STREAM));
        assert!(w.iter().any(|f| f.0 == SETTINGS && f.1 == ACK));
        assert!(
            w.iter()
                .any(|f| f.0 == PING && f.1 == ACK && f.3 == b"12345678")
        );
        assert_eq!(c.peer_max_frame, 0x4000);
    }

    #[test]
    fn body_waits_for_window() {
        // The peer allows 10 bytes per stream, then opens the window.
        let mut input = frame(SETTINGS, 0, 0, &[0, 4, 0, 0, 0, 10]);
        input.extend(frame(WINDOW_UPDATE, 0, 1, &100u32.to_be_bytes()));
        input.extend(frame(HEADERS, END_HEADERS | END_STREAM, 1, &[0x89]));
        let mut c = conn(input);
        // Read the SETTINGS before sending, as after a round trip.
        c.start().unwrap();
        let f = c.read_frame().unwrap();
        c.handle(f).unwrap();
        let mut sent = Vec::new();
        let mut body = Some(vec![7u8; 25]);
        c.send_request(
            &[(":method".into(), "POST".into())],
            true,
            &mut |out| {
                out.extend(body.take().unwrap_or_default());
                Ok(true)
            },
            &mut |d| sent.push(d.len()),
        )
        .unwrap();
        assert_eq!(sent, vec![10, 15]);
        let head = c.read_head(&mut |_, _| {}).unwrap();
        assert_eq!(head.status, 204);
        assert!(head.end_stream);
        let data: Vec<_> = written(&c).into_iter().filter(|f| f.0 == DATA).collect();
        assert_eq!(data.last().unwrap().1, END_STREAM);
    }

    #[test]
    fn goaway_refuses_open_stream() {
        let mut p = 0u32.to_be_bytes().to_vec();
        p.extend_from_slice(&0u32.to_be_bytes());
        let mut c = conn(frame(GOAWAY, 0, 0, &p));
        c.send_request(&[], false, &mut |_| Ok(true), &mut |_| {})
            .unwrap();
        assert!(matches!(c.read_head(&mut |_, _| {}), Err(H2Error::Refused)));
        assert!(!c.usable());
    }

    #[test]
    fn reset_and_eof() {
        let mut c = conn(frame(RST_STREAM, 0, 1, &2u32.to_be_bytes()));
        c.send_request(&[], false, &mut |_| Ok(true), &mut |_| {})
            .unwrap();
        assert!(matches!(
            c.read_head(&mut |_, _| {}),
            Err(H2Error::Reset { stream: 1, code: 2 })
        ));
        let mut c = conn(Vec::new());
        c.send_request(&[], false, &mut |_| Ok(true), &mut |_| {})
            .unwrap();
        assert!(matches!(c.read_head(&mut |_, _| {}), Err(H2Error::Refused)));
    }
}
