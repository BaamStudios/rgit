// SPDX-License-Identifier: MIT
//! A minimal blocking HTTP/1.1 client on `std::net` and rustls.
//!
//! It implements exactly what the smart protocol needs (RFC 9112):
//! request writing with a fixed header order, `Content-Length` and chunked
//! request bodies, status line and header parsing (interim `1xx` responses
//! are skipped), `Content-Length`, chunked and read-until-close response
//! bodies, `gzip`/`deflate` response decoding, HTTP proxies (absolute-form
//! requests for `http://`, `CONNECT` tunnels for `https://`), connection
//! reuse, and `http.lowSpeedLimit`/`http.lowSpeedTime`.
//!
//! The header order follows what git 2.56.0 sends through libcurl:
//! `Host`, `Proxy-Authorization`, `Authorization`, `User-Agent`, `Accept: */*`
//! (unless the caller sets `Accept`), `Accept-Encoding`, `Proxy-Connection`,
//! then the caller's headers in order, then `Content-Length`.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rustls::{ClientConfig, ClientConnection, StreamOwned};

use crate::error::Transport;
use crate::messages;
use crate::options::{HttpOptions, Proxy};
use crate::tls;
use crate::trace::Tracer;
use crate::url::{Scheme, Url};

/// What libcurl offers by default minus the codecs rgit does not decode.
pub(crate) const ACCEPT_ENCODING: &str = "deflate, gzip";

/// A request body.
pub(crate) enum ReqBody<'a> {
    /// No body.
    None,
    /// A body of known length (`Content-Length`).
    Bytes(&'a [u8]),
    /// `Transfer-Encoding: chunked`: `head` followed by the rest of `rest`.
    Chunked {
        head: &'a [u8],
        rest: &'a mut dyn Read,
    },
}

/// A request.
pub(crate) struct Request<'a> {
    pub method: &'static str,
    pub url: &'a Url,
    /// Caller headers, sent after the client's own headers, in order.
    pub headers: Vec<(String, String)>,
    /// Send `Accept-Encoding`.
    pub accept_encoding: bool,
    /// `Authorization` value.
    pub authorization: Option<String>,
    pub body: ReqBody<'a>,
}

/// A response with a streaming body.
pub(crate) struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Box<dyn Read + Send>,
}

impl Response {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Read the whole body.
    pub(crate) fn read_all(&mut self) -> Result<Vec<u8>, Transport> {
        let mut v = Vec::new();
        self.body
            .read_to_end(&mut v)
            .map_err(|e| io_to_transport(&e, 56, "Recv failure"))?;
        Ok(v)
    }
}

enum Stream {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl Stream {
    fn tcp(&self) -> &TcpStream {
        match self {
            Stream::Plain(s) => s,
            Stream::Tls(s) => &s.sock,
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => match s.read(buf) {
                // A peer that closes without close_notify: treat as EOF.
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(0),
                r => r,
            },
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

/// Marker for low-speed aborts inside `io::Error`.
#[derive(Debug)]
struct TooSlow(String);

impl std::fmt::Display for TooSlow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TooSlow {}

/// `http.lowSpeedLimit` / `http.lowSpeedTime` enforcement.
struct Guarded {
    inner: Stream,
    limit: u64,
    time: Duration,
    window_start: Instant,
    window_bytes: u64,
}

impl Guarded {
    fn new(inner: Stream, limit: u64, secs: u64) -> io::Result<Guarded> {
        let time = Duration::from_secs(secs);
        if limit > 0 && secs > 0 {
            inner.tcp().set_read_timeout(Some(time))?;
            inner.tcp().set_write_timeout(Some(time))?;
        }
        Ok(Guarded {
            inner,
            limit,
            time,
            window_start: Instant::now(),
            window_bytes: 0,
        })
    }

    fn active(&self) -> bool {
        self.limit > 0 && !self.time.is_zero()
    }

    fn slow(&self) -> io::Error {
        io::Error::new(
            io::ErrorKind::TimedOut,
            TooSlow(messages::too_slow(self.limit, self.time.as_secs())),
        )
    }

    fn account(&mut self, n: usize) -> io::Result<()> {
        if !self.active() {
            return Ok(());
        }
        self.window_bytes += n as u64;
        let elapsed = self.window_start.elapsed();
        if elapsed >= self.time {
            let rate = self.window_bytes as f64 / elapsed.as_secs_f64();
            if rate < self.limit as f64 {
                return Err(self.slow());
            }
            self.window_start = Instant::now();
            self.window_bytes = 0;
        }
        Ok(())
    }

    fn map(&self, e: io::Error) -> io::Error {
        if self.active()
            && matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            )
        {
            self.slow()
        } else {
            e
        }
    }

    /// Restart the measurement (a new request on a reused connection).
    fn reset(&mut self) {
        self.window_start = Instant::now();
        self.window_bytes = 0;
    }
}

impl Read for Guarded {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf).map_err(|e| self.map(e))?;
        self.account(n)?;
        Ok(n)
    }
}

impl Write for Guarded {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf).map_err(|e| self.map(e))?;
        self.account(n)?;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush().map_err(|e| self.map(e))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PoolKey {
    scheme: Scheme,
    host_port: String,
    proxy: Option<(String, u16)>,
}

struct Conn {
    io: BufReader<Guarded>,
    key: PoolKey,
}

type Pool = Arc<Mutex<Vec<Conn>>>;

/// The HTTP client: options, TLS configuration, connection pool, tracing.
pub(crate) struct Client {
    pub opts: HttpOptions,
    tls: OnceLock<Arc<ClientConfig>>,
    pool: Pool,
    pub tracer: Tracer,
}

pub(crate) fn io_to_transport(e: &io::Error, code: u32, what: &str) -> Transport {
    if let Some(slow) = e.get_ref().and_then(|i| i.downcast_ref::<TooSlow>()) {
        return Transport::new(28, slow.0.clone());
    }
    Transport::new(code, format!("{what}: {e}"))
}

impl Client {
    pub(crate) fn new(opts: HttpOptions) -> Client {
        Client {
            opts,
            tls: OnceLock::new(),
            pool: Arc::new(Mutex::new(Vec::new())),
            tracer: Tracer::from_env(),
        }
    }

    fn tls_config(&self) -> Result<Arc<ClientConfig>, Transport> {
        if let Some(c) = self.tls.get() {
            return Ok(c.clone());
        }
        let c = tls::client_config(&self.opts)?;
        Ok(self.tls.get_or_init(|| c).clone())
    }

    /// Send one request and read the response head.
    pub(crate) fn send(&self, req: &mut Request<'_>) -> Result<Response, Transport> {
        let proxy = self
            .opts
            .proxy_for(req.url)
            .map_err(|e| Transport::new(5, e.to_string()))?;
        let key = PoolKey {
            scheme: req.url.scheme,
            host_port: req.url.host_port(),
            proxy: proxy.as_ref().map(|p| (p.host.clone(), p.port)),
        };
        let head = self.request_head(req, proxy.as_ref());
        let pooled = {
            let mut pool = self.pool.lock().unwrap_or_else(|p| p.into_inner());
            pool.iter()
                .position(|c| c.key == key)
                .map(|i| pool.swap_remove(i))
        };
        let replayable = !matches!(req.body, ReqBody::Chunked { .. });
        if let Some(mut conn) = pooled {
            self.tracer.info(&format!(
                "Reusing existing {}: connection with host {}",
                req.url.scheme.as_str(),
                req.url.host
            ));
            conn.io.get_mut().reset();
            match self.exchange(conn, &head, req) {
                Ok(r) => return Ok(r),
                // A stale keep-alive connection: try once more on a new one.
                Err((true, _)) if replayable => {}
                Err((_, e)) => return Err(e),
            }
        }
        let conn = self.connect(req.url, proxy.as_ref(), key)?;
        self.exchange(conn, &head, req).map_err(|(_, e)| e)
    }

    fn request_head(&self, req: &Request<'_>, proxy: Option<&Proxy>) -> Vec<u8> {
        let url = req.url;
        let via_http_proxy = proxy.is_some() && url.scheme == Scheme::Http;
        let target = if via_http_proxy {
            url.without_credentials().to_string()
        } else {
            url.path.clone()
        };
        let mut h = format!("{} {target} HTTP/1.1\r\n", req.method);
        h.push_str(&format!("Host: {}\r\n", url.authority()));
        if via_http_proxy {
            if let Some(auth) = proxy.and_then(proxy_authorization) {
                h.push_str(&format!("Proxy-Authorization: {auth}\r\n"));
            }
        }
        if let Some(a) = &req.authorization {
            h.push_str(&format!("Authorization: {a}\r\n"));
        }
        h.push_str(&format!("User-Agent: {}\r\n", self.opts.user_agent));
        if !req
            .headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("accept"))
        {
            h.push_str("Accept: */*\r\n");
        }
        if req.accept_encoding {
            h.push_str(&format!("Accept-Encoding: {ACCEPT_ENCODING}\r\n"));
        }
        if via_http_proxy {
            h.push_str("Proxy-Connection: Keep-Alive\r\n");
        }
        for (k, v) in &req.headers {
            h.push_str(&format!("{k}: {v}\r\n"));
        }
        if let ReqBody::Bytes(b) = req.body {
            h.push_str(&format!("Content-Length: {}\r\n", b.len()));
        } else if req.method == "POST" && matches!(req.body, ReqBody::None) {
            h.push_str("Content-Length: 0\r\n");
        }
        h.push_str("\r\n");
        h.into_bytes()
    }

    /// Write the request and read the response head. The flag in the error
    /// says whether nothing was received (a stale pooled connection).
    fn exchange(
        &self,
        mut conn: Conn,
        head: &[u8],
        req: &mut Request<'_>,
    ) -> Result<Response, (bool, Transport)> {
        let send_err = |e: io::Error| (true, io_to_transport(&e, 55, "Send failure"));
        self.tracer.send_header(head);
        let w = conn.io.get_mut();
        w.write_all(head).map_err(send_err)?;
        match &mut req.body {
            ReqBody::None => {}
            ReqBody::Bytes(b) => {
                w.write_all(b).map_err(send_err)?;
                self.tracer.send_data(b);
            }
            ReqBody::Chunked { head: first, rest } => {
                let mut total = 0usize;
                let write_chunk = |w: &mut Guarded, data: &[u8]| -> io::Result<()> {
                    if data.is_empty() {
                        return Ok(());
                    }
                    let mut framed = format!("{:x}\r\n", data.len()).into_bytes();
                    framed.extend_from_slice(data);
                    framed.extend_from_slice(b"\r\n");
                    w.write_all(&framed)?;
                    self.tracer.send_data(&framed);
                    Ok(())
                };
                write_chunk(w, first)
                    .map_err(|e| (false, io_to_transport(&e, 55, "Send failure")))?;
                total += first.len();
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    let n = match rest.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(e) => {
                            return Err((
                                false,
                                Transport::new(26, format!("Read callback failed: {e}")),
                            ));
                        }
                    };
                    write_chunk(w, &buf[..n])
                        .map_err(|e| (false, io_to_transport(&e, 55, "Send failure")))?;
                    total += n;
                }
                w.write_all(b"0\r\n\r\n")
                    .map_err(|e| (false, io_to_transport(&e, 55, "Send failure")))?;
                self.tracer.send_data(b"0\r\n\r\n");
                self.tracer
                    .info(&format!("upload completely sent off: {total} bytes"));
            }
        }
        conn.io
            .get_mut()
            .flush()
            .map_err(|e| (true, io_to_transport(&e, 55, "Send failure")))?;
        let (status, headers, http10) = loop {
            let status_line = match read_head_line(&mut conn.io) {
                Ok(Some(l)) => l,
                Ok(None) => return Err((true, Transport::new(52, messages::EMPTY_REPLY))),
                Err(e) => return Err((true, io_to_transport(&e, 56, "Recv failure"))),
            };
            self.tracer.recv_header(&status_line);
            let line = String::from_utf8_lossy(&status_line);
            let line = line.trim_end();
            let mut parts = line.splitn(3, ' ');
            let version = parts.next().unwrap_or("");
            let status: u16 = match (
                version.starts_with("HTTP/1."),
                parts.next().and_then(|s| s.parse().ok()),
            ) {
                (true, Some(s)) => s,
                _ => return Err((false, Transport::new(8, messages::bad_response(line)))),
            };
            let _reason = parts.next();
            let mut headers = Vec::new();
            loop {
                let raw = match read_head_line(&mut conn.io) {
                    Ok(Some(l)) => l,
                    Ok(None) => {
                        return Err((
                            false,
                            Transport::new(56, messages::bad_response("truncated header")),
                        ));
                    }
                    Err(e) => return Err((false, io_to_transport(&e, 56, "Recv failure"))),
                };
                self.tracer.recv_header(&raw);
                let l = String::from_utf8_lossy(&raw);
                let l = l.trim_end_matches(['\r', '\n']);
                if l.is_empty() {
                    break;
                }
                if let Some((k, v)) = l.split_once(':') {
                    headers.push((k.trim().to_string(), v.trim().to_string()));
                }
            }
            if (100..200).contains(&status) && status != 101 {
                continue;
            }
            break (status, headers, version == "HTTP/1.0");
        };
        let get = |n: &str| {
            headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(n))
                .map(|(_, v)| v.to_ascii_lowercase())
        };
        let conn_hdr = get("connection").unwrap_or_default();
        let mut keep_alive = if http10 {
            conn_hdr.contains("keep-alive")
        } else {
            !conn_hdr.contains("close")
        };
        let framing = if req.method == "HEAD" || status == 204 || status == 304 {
            Framing::Length(0)
        } else if get("transfer-encoding").is_some_and(|v| v.contains("chunked")) {
            Framing::Chunked {
                remaining: 0,
                done: false,
                first: true,
            }
        } else if let Some(n) = get("content-length").and_then(|v| v.trim().parse::<u64>().ok()) {
            Framing::Length(n)
        } else {
            keep_alive = false;
            Framing::Close
        };
        let raw = BodyReader {
            conn: Some(conn),
            framing,
            keep_alive,
            pool: self.pool.clone(),
            tracer: self.tracer,
        };
        let body: Box<dyn Read + Send> = match get("content-encoding").as_deref().map(str::trim) {
            Some("gzip") | Some("x-gzip") => Box::new(flate2::read::MultiGzDecoder::new(raw)),
            Some("deflate") => Box::new(flate2::read::ZlibDecoder::new(raw)),
            _ => Box::new(raw),
        };
        Ok(Response {
            status,
            headers,
            body,
        })
    }

    fn connect(&self, url: &Url, proxy: Option<&Proxy>, key: PoolKey) -> Result<Conn, Transport> {
        let tcp = match proxy {
            Some(p) => tcp_connect(&p.host, p.port, true, &self.tracer)?,
            None => tcp_connect(&url.host, url.port_or_default(), false, &self.tracer)?,
        };
        let _ = tcp.set_nodelay(true);
        if let (Some(p), Scheme::Https) = (proxy, url.scheme) {
            self.tunnel(&tcp, url, p)?;
        }
        let stream = match url.scheme {
            Scheme::Http => Stream::Plain(tcp),
            Scheme::Https => {
                let cfg = self.tls_config()?;
                let name = tls::server_name(&url.host)?;
                let mut c = ClientConnection::new(cfg, name)
                    .map_err(|e| Transport::new(35, messages::ssl_connect_error(&e.to_string())))?;
                let mut sock = tcp;
                while c.is_handshaking() {
                    c.complete_io(&mut sock)
                        .map_err(|e| tls::map_error(&e, &url.host))?;
                }
                Stream::Tls(Box::new(StreamOwned::new(c, sock)))
            }
        };
        let guarded = Guarded::new(stream, self.opts.low_speed_limit, self.opts.low_speed_time)
            .map_err(|e| io_to_transport(&e, 7, "Failed to set socket options"))?;
        Ok(Conn {
            io: BufReader::with_capacity(64 * 1024, guarded),
            key,
        })
    }

    fn tunnel(&self, tcp: &TcpStream, url: &Url, p: &Proxy) -> Result<(), Transport> {
        let target = url.host_port();
        let mut h = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
        if let Some(a) = proxy_authorization(p) {
            h.push_str(&format!("Proxy-Authorization: {a}\r\n"));
        }
        h.push_str(&format!(
            "User-Agent: {}\r\nProxy-Connection: Keep-Alive\r\n\r\n",
            self.opts.user_agent
        ));
        self.tracer.send_header(h.as_bytes());
        let mut w = tcp;
        w.write_all(h.as_bytes())
            .map_err(|e| io_to_transport(&e, 55, "Send failure"))?;
        // Read byte by byte so no TLS bytes are swallowed by a buffer.
        let mut r = ByteReader(tcp);
        let mut status = 0;
        let mut first = true;
        loop {
            let line = read_head_line(&mut r)
                .map_err(|e| io_to_transport(&e, 56, "Recv failure"))?
                .ok_or_else(|| Transport::new(56, messages::EMPTY_REPLY))?;
            self.tracer.recv_header(&line);
            let text = String::from_utf8_lossy(&line);
            if first {
                status = text
                    .split(' ')
                    .nth(1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                first = false;
            } else if text.trim().is_empty() {
                break;
            }
        }
        if !(200..300).contains(&status) {
            return Err(Transport::new(56, messages::proxy_connect_failed(status)));
        }
        Ok(())
    }
}

struct ByteReader<'a>(&'a TcpStream);

impl BufRead for ByteReader<'_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        unreachable!("read_head_line uses read_until on BufReader only")
    }
    fn consume(&mut self, _amt: usize) {}
    fn read_until(&mut self, byte: u8, buf: &mut Vec<u8>) -> io::Result<usize> {
        let mut n = 0;
        let mut one = [0u8; 1];
        loop {
            let mut s = self.0;
            match s.read(&mut one)? {
                0 => return Ok(n),
                _ => {
                    buf.push(one[0]);
                    n += 1;
                    if one[0] == byte {
                        return Ok(n);
                    }
                }
            }
        }
    }
}

impl Read for ByteReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut s = self.0;
        s.read(buf)
    }
}

fn proxy_authorization(p: &Proxy) -> Option<String> {
    let user = p.user.as_ref()?;
    let pass = p.password.as_deref().unwrap_or("");
    Some(format!(
        "Basic {}",
        crate::auth::base64(format!("{user}:{pass}").as_bytes())
    ))
}

fn tcp_connect(
    host: &str,
    port: u16,
    is_proxy: bool,
    tracer: &Tracer,
) -> Result<TcpStream, Transport> {
    let resolve_err = || {
        if is_proxy {
            Transport::new(5, messages::could_not_resolve_proxy(host))
        } else {
            Transport::new(6, messages::could_not_resolve_host(host))
        }
    };
    let addrs: Vec<_> = (host, port)
        .to_socket_addrs()
        .map_err(|_| resolve_err())?
        .collect();
    if addrs.is_empty() {
        return Err(resolve_err());
    }
    let start = Instant::now();
    for a in &addrs {
        tracer.info(&format!("  Trying {a}..."));
        if let Ok(s) = TcpStream::connect(a) {
            if let Ok(local) = s.local_addr() {
                tracer.info(&format!(
                    "Established connection to {host} ({} port {port}) from {} port {} ",
                    a.ip(),
                    local.ip(),
                    local.port()
                ));
            }
            return Ok(s);
        }
    }
    let shown = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    Err(Transport::new(
        7,
        messages::failed_to_connect(&shown, port, start.elapsed().as_millis()),
    ))
}

/// Read one header line including its line ending; `None` at EOF.
fn read_head_line(r: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut buf = Vec::new();
    let n = r.read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    if buf.len() > 64 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "header line too long",
        ));
    }
    Ok(Some(buf))
}

enum Framing {
    Length(u64),
    Chunked {
        remaining: u64,
        done: bool,
        first: bool,
    },
    Close,
}

/// The raw (still content-encoded) response body. Returns the connection
/// to the pool once the body has been read completely.
struct BodyReader {
    conn: Option<Conn>,
    framing: Framing,
    keep_alive: bool,
    pool: Pool,
    tracer: Tracer,
}

impl BodyReader {
    fn finish(&mut self) {
        if let Some(conn) = self.conn.take() {
            if self.keep_alive {
                let mut pool = self.pool.lock().unwrap_or_else(|p| p.into_inner());
                pool.push(conn);
            }
        }
    }

    fn read_inner(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Some(conn) = self.conn.as_mut() else {
            return Ok(0);
        };
        match &mut self.framing {
            Framing::Length(left) => {
                if *left == 0 {
                    self.finish();
                    return Ok(0);
                }
                let max = (*left).min(buf.len() as u64) as usize;
                let n = conn.io.read(&mut buf[..max])?;
                if n == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "transfer closed with outstanding read data remaining",
                    ));
                }
                *left -= n as u64;
                if *left == 0 {
                    self.tracer.recv_data(&buf[..n]);
                    self.finish();
                    return Ok(n);
                }
                self.tracer.recv_data(&buf[..n]);
                Ok(n)
            }
            Framing::Close => {
                let n = conn.io.read(buf)?;
                if n == 0 {
                    self.conn = None;
                } else {
                    self.tracer.recv_data(&buf[..n]);
                }
                Ok(n)
            }
            Framing::Chunked {
                remaining,
                done,
                first,
            } => {
                if *done {
                    return Ok(0);
                }
                if *remaining == 0 {
                    if !*first {
                        // CRLF after the previous chunk.
                        let mut crlf = Vec::new();
                        conn.io.read_until(b'\n', &mut crlf)?;
                    }
                    *first = false;
                    let mut line = Vec::new();
                    conn.io.read_until(b'\n', &mut line)?;
                    if line.is_empty() {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "unexpected EOF in chunked body",
                        ));
                    }
                    let text = String::from_utf8_lossy(&line);
                    let size_text = text
                        .trim()
                        .split(';')
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    let size = u64::from_str_radix(&size_text, 16).map_err(|_| {
                        io::Error::new(io::ErrorKind::InvalidData, "invalid chunk size")
                    })?;
                    if size == 0 {
                        // Trailers up to the empty line.
                        loop {
                            let mut t = Vec::new();
                            let n = conn.io.read_until(b'\n', &mut t)?;
                            if n == 0 || t == b"\r\n" || t == b"\n" {
                                break;
                            }
                        }
                        *done = true;
                        self.finish();
                        return Ok(0);
                    }
                    *remaining = size;
                }
                let max = (*remaining).min(buf.len() as u64) as usize;
                let n = conn.io.read(&mut buf[..max])?;
                if n == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "unexpected EOF in chunked body",
                    ));
                }
                *remaining -= n as u64;
                self.tracer.recv_data(&buf[..n]);
                Ok(n)
            }
        }
    }
}

impl Read for BodyReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        match self.read_inner(buf) {
            Ok(n) => Ok(n),
            Err(e) => {
                self.conn = None;
                Err(e)
            }
        }
    }
}
