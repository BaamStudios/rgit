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
//! HTTP/2 (RFC 9113, [`crate::h2`]) is used like libcurl does for git:
//! offered via ALPN on HTTPS unless `http.version=HTTP/1.1`, and with
//! `http.version=HTTP/2` also requested from plain HTTP servers with
//! `Upgrade: h2c` on requests without a streamed body. The request is
//! traced in HTTP/1 form with `HTTP/2` as version, the response as
//! `HTTP/2 <status>` and the lower-case header lines, like libcurl.
//!
//! The header order follows what git 2.56.0 sends through libcurl:
//! `Host`, `Proxy-Authorization`, `Authorization`, `User-Agent`, `Accept: */*`
//! (unless the caller sets `Accept`), `Accept-Encoding`, `Proxy-Connection`,
//! then the caller's headers in order, then `Content-Length`.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rustls::{ClientConfig, ClientConnection, StreamOwned};

use crate::cookies::CookieJar;
use crate::error::Transport;
use crate::h2::{H2Conn, H2Error};
use crate::messages;
use crate::options::{HttpOptions, HttpVersion, Proxy, ProxyKind};
use crate::socks;
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
    /// A non-blocking liveness check is running: report `WouldBlock`.
    polling: bool,
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
            polling: false,
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
        if self.polling {
            return self.inner.read(buf);
        }
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

/// An HTTP/2 connection and its pool key.
struct H2Pooled {
    conn: H2Conn<Guarded>,
    key: PoolKey,
}

/// A connection in the pool.
enum Pooled {
    H1(Conn),
    H2(Box<H2Pooled>),
}

impl Pooled {
    fn key(&self) -> &PoolKey {
        match self {
            Pooled::H1(c) => &c.key,
            Pooled::H2(h) => &h.key,
        }
    }
}

type Pool = Arc<Mutex<Vec<Pooled>>>;

/// Marker for HTTP/2 stream failures inside `io::Error` (curl code and
/// message).
#[derive(Debug)]
struct H2Failure(u32, String);

impl std::fmt::Display for H2Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.1)
    }
}

impl std::error::Error for H2Failure {}

/// The transport error for an HTTP/2 failure.
fn h2_transport(tracer: &Tracer, e: H2Error, sending: bool) -> Transport {
    match e {
        H2Error::Io(e) if sending => io_to_transport(&e, 55, "Send failure"),
        H2Error::Io(e) => io_to_transport(&e, 56, "Recv failure"),
        H2Error::Body(e) => Transport::new(26, format!("Read callback failed: {e}")),
        H2Error::Refused => Transport::new(92, messages::H2_REFUSED.to_string()),
        H2Error::Reset { stream, code } => Transport::new(
            92,
            messages::h2_stream_error(stream, crate::h2::error_name(code), code),
        ),
        H2Error::Protocol(why) => {
            tracer.info(&format!("HTTP/2 protocol error: {why}"));
            Transport::new(16, messages::H2_FRAMING.to_string())
        }
    }
}

/// The HTTP client: options, TLS configuration, connection pool, tracing.
pub(crate) struct Client {
    pub opts: HttpOptions,
    tls: OnceLock<Arc<ClientConfig>>,
    pool: Pool,
    pub tracer: Tracer,
    /// `http.cookieFile` / `http.saveCookies`.
    cookies: Option<Mutex<CookieJar>>,
    /// The proxy password obtained from the credential helpers when the
    /// proxy URL names a user but no password.
    pub proxy_password: Option<String>,
}

pub(crate) fn io_to_transport(e: &io::Error, code: u32, what: &str) -> Transport {
    if let Some(slow) = e.get_ref().and_then(|i| i.downcast_ref::<TooSlow>()) {
        return Transport::new(28, slow.0.clone());
    }
    if let Some(f) = e.get_ref().and_then(|i| i.downcast_ref::<H2Failure>()) {
        return Transport::new(f.0, f.1.clone());
    }
    Transport::new(code, format!("{what}: {e}"))
}

impl Client {
    pub(crate) fn new(opts: HttpOptions) -> Client {
        let cookies = opts
            .cookie_file
            .as_ref()
            .map(|f| Mutex::new(CookieJar::load(f, opts.save_cookies)));
        Client {
            opts,
            tls: OnceLock::new(),
            pool: Arc::new(Mutex::new(Vec::new())),
            tracer: Tracer::from_env(),
            cookies,
            proxy_password: None,
        }
    }

    /// The proxy for `url` with the password from the credential helpers
    /// filled in.
    fn proxy(&self, url: &Url) -> Result<Option<Proxy>, Transport> {
        let mut proxy = self
            .opts
            .proxy_for(url)
            .map_err(|e| Transport::new(5, e.to_string()))?;
        if let Some(p) = proxy.as_mut() {
            if p.user.is_some() && p.password.is_none() {
                p.password = self.proxy_password.clone();
            }
            if p.kind == ProxyKind::Https {
                return Err(Transport::new(4, messages::HTTPS_PROXY_UNSUPPORTED));
            }
            if p.socket.is_some() {
                return Err(Transport::new(4, messages::UNIX_SOCKET_PROXY_UNSUPPORTED));
            }
        }
        Ok(proxy)
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
        let proxy = self.proxy(req.url)?;
        let key = PoolKey {
            scheme: req.url.scheme,
            host_port: req.url.host_port(),
            proxy: proxy.as_ref().map(|p| (p.host.clone(), p.port)),
        };
        // `http.version=HTTP/2` on plain HTTP: ask for an upgrade (not
        // through proxies, and not with a streamed body).
        let upgrade = self.opts.version == HttpVersion::Http2
            && req.url.scheme == Scheme::Http
            && proxy.is_none()
            && !matches!(req.body, ReqBody::Chunked { .. });
        let fields = self.header_fields(req, proxy.as_ref(), upgrade);
        let replayable = !matches!(req.body, ReqBody::Chunked { .. });
        loop {
            let pooled = {
                let mut pool = self.pool.lock().unwrap_or_else(|p| p.into_inner());
                pool.iter()
                    .position(|c| *c.key() == key)
                    .map(|i| pool.swap_remove(i))
            };
            let Some(pooled) = pooled else { break };
            let reused = format!(
                "Reusing existing {}: connection with host {}",
                req.url.scheme.as_str(),
                req.url.host
            );
            let res = match pooled {
                Pooled::H1(mut conn) => {
                    self.tracer.info(&reused);
                    conn.io.get_mut().reset();
                    self.exchange(conn, &fields, upgrade, req)
                }
                Pooled::H2(mut h) => {
                    if !h2_alive(&mut h) {
                        // Closed or going away while idle: try the next.
                        continue;
                    }
                    self.tracer.info(&reused);
                    h.conn.io.get_mut().reset();
                    self.exchange_h2(h, &fields, req)
                }
            };
            match res {
                Ok(r) => return Ok(r),
                // A stale keep-alive connection, or a request the server
                // refused: try once more on a new one.
                Err((true, _)) if replayable => break,
                Err((_, e)) => return Err(e),
            }
        }
        let res = match self.connect(req.url, proxy.as_ref(), key)? {
            Pooled::H1(conn) => self.exchange(conn, &fields, upgrade, req),
            Pooled::H2(h) => self.exchange_h2(h, &fields, req),
        };
        res.map_err(|(_, e)| e)
    }

    /// The request line target and the header fields in the order libcurl
    /// sends them for git.
    fn header_fields(
        &self,
        req: &Request<'_>,
        proxy: Option<&Proxy>,
        upgrade: bool,
    ) -> (String, Vec<(String, String)>) {
        let url = req.url;
        let via_http_proxy =
            proxy.is_some_and(|p| p.kind == ProxyKind::Http) && url.scheme == Scheme::Http;
        let target = if via_http_proxy {
            url.without_credentials().to_string()
        } else {
            url.path.clone()
        };
        let mut h: Vec<(String, String)> = Vec::new();
        let mut add = |k: &str, v: String| h.push((k.to_string(), v));
        add("Host", url.authority());
        if via_http_proxy {
            if let Some(auth) = proxy.and_then(proxy_authorization) {
                add("Proxy-Authorization", auth);
            }
        }
        if let Some(a) = &req.authorization {
            add("Authorization", a.clone());
        }
        add("User-Agent", self.opts.user_agent.clone());
        if !req
            .headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("accept"))
        {
            add("Accept", "*/*".into());
        }
        if req.accept_encoding {
            add("Accept-Encoding", ACCEPT_ENCODING.into());
        }
        if upgrade {
            add("Upgrade", "h2c".into());
            add(
                "HTTP2-Settings",
                crate::auth::base64url(&crate::h2::our_settings()),
            );
        }
        if via_http_proxy {
            add("Proxy-Connection", "Keep-Alive".into());
        }
        if let Some(jar) = &self.cookies {
            let jar = jar.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(c) = jar.header(&url.host, &url.path, url.scheme == Scheme::Https) {
                add("Cookie", c);
            }
        }
        for (k, v) in &req.headers {
            add(k, v.clone());
        }
        if let ReqBody::Bytes(b) = req.body {
            add("Content-Length", b.len().to_string());
        } else if req.method == "POST" && matches!(req.body, ReqBody::None) {
            add("Content-Length", "0".into());
        }
        if upgrade {
            add("Connection", "Upgrade, HTTP2-Settings".into());
        }
        (target, h)
    }

    /// The request head in HTTP/1 form (`version` is `HTTP/1.1` or, for
    /// the trace of an HTTP/2 request, `HTTP/2`).
    fn request_head(
        method: &str,
        fields: &(String, Vec<(String, String)>),
        version: &str,
        h2: bool,
    ) -> Vec<u8> {
        let mut h = format!("{method} {} {version}\r\n", fields.0);
        for (k, v) in &fields.1 {
            if h2 && !h2_allowed(k) && !k.eq_ignore_ascii_case("host") {
                continue;
            }
            h.push_str(&format!("{k}: {v}\r\n"));
        }
        h.push_str("\r\n");
        h.into_bytes()
    }

    /// Send a request on an HTTP/2 connection and read the response head.
    /// The flag in the error says whether the request may be sent again.
    fn exchange_h2(
        &self,
        mut h: Box<H2Pooled>,
        fields: &(String, Vec<(String, String)>),
        req: &mut Request<'_>,
    ) -> Result<Response, (bool, Transport)> {
        let url = req.url;
        self.tracer.info(&format!(
            "[HTTP/2] OPENED stream for {}",
            url.without_credentials()
        ));
        let mut wire: Vec<(String, String)> = vec![
            (":method".into(), req.method.into()),
            (":scheme".into(), url.scheme.as_str().into()),
            (":authority".into(), url.authority()),
            (":path".into(), url.path.clone()),
        ];
        for (k, v) in &fields.1 {
            if h2_allowed(k) {
                wire.push((k.to_ascii_lowercase(), v.clone()));
            }
        }
        self.tracer
            .send_header(&Client::request_head(req.method, fields, "HTTP/2", true));
        let tracer = self.tracer;
        let mut total = 0usize;
        let has_body = !matches!(req.body, ReqBody::None);
        let mut first = true;
        let mut buf = vec![0u8; 64 * 1024];
        let body = &mut req.body;
        let mut next = |out: &mut Vec<u8>| -> io::Result<bool> {
            match body {
                ReqBody::None => Ok(true),
                ReqBody::Bytes(b) => {
                    out.extend_from_slice(b);
                    Ok(true)
                }
                ReqBody::Chunked { head, rest } => {
                    if std::mem::take(&mut first) && !head.is_empty() {
                        out.extend_from_slice(head);
                        return Ok(false);
                    }
                    loop {
                        match rest.read(&mut buf) {
                            Ok(0) => return Ok(true),
                            Ok(n) => {
                                out.extend_from_slice(&buf[..n]);
                                return Ok(false);
                            }
                            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                            Err(e) => return Err(e),
                        }
                    }
                }
            }
        };
        let sent = h.conn.send_request(&wire, has_body, &mut next, &mut |d| {
            total += d.len();
            tracer.send_data(d);
        });
        if let Err(e) = sent {
            let retry = matches!(e, H2Error::Refused | H2Error::Io(_));
            return Err((retry, h2_transport(&tracer, e, true)));
        }
        if has_body {
            self.tracer
                .info(&format!("upload completely sent off: {total} bytes"));
        }
        let head = h
            .conn
            .read_head(&mut |status, headers| trace_h2_head(&tracer, status, headers))
            .map_err(|e| {
                (
                    matches!(e, H2Error::Refused),
                    h2_transport(&tracer, e, false),
                )
            })?;
        self.h2_response(h, head, url)
    }

    /// The response for a received HTTP/2 head.
    fn h2_response(
        &self,
        h: Box<H2Pooled>,
        head: crate::h2::Head,
        url: &Url,
    ) -> Result<Response, (bool, Transport)> {
        trace_h2_head(&self.tracer, head.status, &head.headers);
        if let Some(jar) = &self.cookies {
            let mut jar = jar.lock().unwrap_or_else(|p| p.into_inner());
            jar.store(&url.host, &url.path, &head.headers);
        }
        let raw = H2Body {
            conn: Some(h),
            buf: Vec::new(),
            pos: 0,
            pool: self.pool.clone(),
            tracer: self.tracer,
        };
        let mut raw = raw;
        if head.end_stream {
            raw.finish();
        }
        let encoding = head
            .headers
            .iter()
            .find(|(k, _)| k == "content-encoding")
            .map(|(_, v)| v.trim().to_ascii_lowercase());
        Ok(Response {
            status: head.status,
            headers: head.headers,
            body: decoded(raw, encoding.as_deref()),
        })
    }

    /// Write the request and read the response head. The flag in the error
    /// says whether nothing was received (a stale pooled connection).
    fn exchange(
        &self,
        mut conn: Conn,
        fields: &(String, Vec<(String, String)>),
        upgrade: bool,
        req: &mut Request<'_>,
    ) -> Result<Response, (bool, Transport)> {
        let head = &Client::request_head(req.method, fields, "HTTP/1.1", false);
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
            let mut headers: Vec<(String, String)> = Vec::new();
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
                // An obsolete line folding (RFC 9112, 5.2) continues the
                // previous header; empty continuations add nothing.
                if l.starts_with([' ', '\t']) {
                    let more = l.trim();
                    if let (Some((_, v)), false) = (headers.last_mut(), more.is_empty()) {
                        if !v.is_empty() {
                            v.push(' ');
                        }
                        v.push_str(more);
                    }
                    continue;
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
        let upgraded = status == 101
            && upgrade
            && headers.iter().any(|(k, v)| {
                k.eq_ignore_ascii_case("upgrade") && v.trim().eq_ignore_ascii_case("h2c")
            });
        if upgraded {
            self.tracer.info("Received 101, Switching to HTTP/2");
            let key = conn.key.clone();
            let h2 = H2Conn::upgraded(conn.io)
                .map_err(|e| (false, h2_transport(&self.tracer, e, true)))?;
            let mut h = Box::new(H2Pooled { conn: h2, key });
            let tracer = self.tracer;
            let head = h
                .conn
                .read_head(&mut |status, headers| trace_h2_head(&tracer, status, headers))
                .map_err(|e| (false, h2_transport(&tracer, e, false)))?;
            return self.h2_response(h, head, req.url);
        }
        if let Some(jar) = &self.cookies {
            let mut jar = jar.lock().unwrap_or_else(|p| p.into_inner());
            jar.store(&req.url.host, &req.url.path, &headers);
        }
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
        let encoding = get("content-encoding").map(|v| v.trim().to_string());
        let body = decoded(raw, encoding.as_deref());
        Ok(Response {
            status,
            headers,
            body,
        })
    }

    /// The addresses of `host:port`: `http.curloptResolve` first, then
    /// the system resolver.
    fn resolve(&self, host: &str, port: u16, is_proxy: bool) -> Result<Vec<SocketAddr>, Transport> {
        if let Some(addrs) = resolve_override(&self.opts.resolve, host, port)? {
            return Ok(addrs);
        }
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
        Ok(addrs)
    }

    fn connect(&self, url: &Url, proxy: Option<&Proxy>, key: PoolKey) -> Result<Pooled, Transport> {
        let port = url.port_or_default();
        let tcp = match proxy {
            Some(p) => {
                let addrs = self.resolve(&p.host, p.port, true)?;
                let tcp = tcp_connect(&addrs, &p.host, p.port, &self.tracer).map_err(|ms| {
                    Transport::new(
                        7,
                        messages::failed_to_connect_over_proxy(&url.host, port, &p.host, ms),
                    )
                })?;
                let resolve = |h: &str, port: u16| self.resolve(h, port, false);
                socks::handshake(&tcp, p, &url.host, port, &resolve, &self.tracer)?;
                tcp
            }
            None => {
                let addrs = self.resolve(&url.host, port, false)?;
                tcp_connect(&addrs, &url.host, port, &self.tracer).map_err(|ms| {
                    let shown = if url.host.contains(':') {
                        format!("[{}]", url.host)
                    } else {
                        url.host.clone()
                    };
                    Transport::new(7, messages::failed_to_connect(&shown, port, ms))
                })?
            }
        };
        let _ = tcp.set_nodelay(true);
        if let (Some(p), Scheme::Https) = (proxy, url.scheme) {
            if p.kind == ProxyKind::Http {
                self.tunnel(&tcp, url, p)?;
            }
        }
        let mut h2 = false;
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
                match c.alpn_protocol() {
                    Some(p) => {
                        h2 = p == b"h2";
                        self.tracer.info(&format!(
                            "ALPN: server accepted {}",
                            String::from_utf8_lossy(p)
                        ));
                    }
                    None => self
                        .tracer
                        .info("ALPN: server did not agree on a protocol. Uses default."),
                }
                Stream::Tls(Box::new(StreamOwned::new(c, sock)))
            }
        };
        let guarded = Guarded::new(stream, self.opts.low_speed_limit, self.opts.low_speed_time)
            .map_err(|e| io_to_transport(&e, 7, "Failed to set socket options"))?;
        let io = BufReader::with_capacity(64 * 1024, guarded);
        if h2 {
            self.tracer.info("using HTTP/2");
            return Ok(Pooled::H2(Box::new(H2Pooled {
                conn: H2Conn::new(io),
                key,
            })));
        }
        Ok(Pooled::H1(Conn { io, key }))
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

/// `http.curloptResolve`: the addresses configured for `host:port`, if
/// any. Entries are `[+]HOST:PORT:ADDRESS[,ADDRESS]` and `-HOST:PORT`
/// (which removes earlier entries for that pair).
fn resolve_override(
    entries: &[String],
    host: &str,
    port: u16,
) -> Result<Option<Vec<SocketAddr>>, Transport> {
    let mut found: Option<Vec<SocketAddr>> = None;
    for e in entries {
        let bad = || Transport::new(3, messages::bad_resolve_entry(e));
        let (remove, spec) = match e.strip_prefix('-') {
            Some(s) => (true, s),
            None => (false, e.strip_prefix('+').unwrap_or(e)),
        };
        let (h, rest) = spec.split_once(':').ok_or_else(bad)?;
        let (p, addrs) = match rest.split_once(':') {
            Some((p, a)) => (p, Some(a)),
            None => (rest, None),
        };
        let p: u16 = p.parse().map_err(|_| bad())?;
        let matches = h.eq_ignore_ascii_case(host) && p == port;
        if remove {
            if matches {
                found = None;
            }
            continue;
        }
        let addrs = addrs.ok_or_else(bad)?;
        let mut list = Vec::new();
        for a in addrs.split(',') {
            let a = a.trim().trim_start_matches('[').trim_end_matches(']');
            let ip: IpAddr = a.parse().map_err(|_| bad())?;
            list.push(SocketAddr::new(ip, port));
        }
        if list.is_empty() {
            return Err(bad());
        }
        if matches {
            found = Some(list);
        }
    }
    Ok(found)
}

/// Connect to the first reachable address; the error is the time spent.
fn tcp_connect(
    addrs: &[SocketAddr],
    host: &str,
    port: u16,
    tracer: &Tracer,
) -> Result<TcpStream, u128> {
    let start = Instant::now();
    for a in addrs {
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
    Err(start.elapsed().as_millis())
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
                pool.push(Pooled::H1(conn));
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

/// Wrap a raw body in the decoder for its `Content-Encoding`.
fn decoded<R: Read + Send + 'static>(raw: R, encoding: Option<&str>) -> Box<dyn Read + Send> {
    match encoding {
        Some("gzip") | Some("x-gzip") => Box::new(flate2::read::MultiGzDecoder::new(raw)),
        Some("deflate") => Box::new(flate2::read::ZlibDecoder::new(raw)),
        _ => Box::new(raw),
    }
}

/// Header fields that may be sent on HTTP/2 (RFC 9113, 8.2.2: no
/// connection-specific fields; `Host` becomes `:authority`).
fn h2_allowed(name: &str) -> bool {
    ![
        "host",
        "connection",
        "proxy-connection",
        "keep-alive",
        "transfer-encoding",
        "upgrade",
        "http2-settings",
    ]
    .iter()
    .any(|n| name.eq_ignore_ascii_case(n))
}

/// Trace an HTTP/2 response head the way libcurl shows it.
fn trace_h2_head(tracer: &Tracer, status: u16, headers: &[(String, String)]) {
    if !tracer.enabled() {
        return;
    }
    tracer.recv_header(format!("HTTP/2 {status} \r\n").as_bytes());
    for (k, v) in headers {
        tracer.recv_header(format!("{k}: {v}\r\n").as_bytes());
    }
    tracer.recv_header(b"\r\n");
}

/// Whether a pooled HTTP/2 connection can carry another request: handle
/// what arrived while it was idle (`GOAWAY`, `PING`, `SETTINGS`) without
/// blocking.
fn h2_alive(h: &mut H2Pooled) -> bool {
    if !h.conn.usable() {
        return false;
    }
    if h.conn.buffered() == 0 {
        let tcp = h.conn.io.get_ref().inner.tcp();
        if tcp.set_nonblocking(true).is_err() {
            return false;
        }
        h.conn.io.get_mut().polling = true;
        let r = h.conn.io.fill_buf().map(|b| b.len());
        h.conn.io.get_mut().polling = false;
        if h.conn
            .io
            .get_ref()
            .inner
            .tcp()
            .set_nonblocking(false)
            .is_err()
        {
            return false;
        }
        match r {
            Ok(0) => return false,
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return true,
            Err(_) => return false,
        }
    }
    h.conn.process_idle().is_ok() && h.conn.usable()
}

/// The body of an HTTP/2 response. Returns the connection to the pool at
/// the end of the stream.
struct H2Body {
    conn: Option<Box<H2Pooled>>,
    buf: Vec<u8>,
    pos: usize,
    pool: Pool,
    tracer: Tracer,
}

impl H2Body {
    fn finish(&mut self) {
        if let Some(h) = self.conn.take() {
            if h.conn.usable() {
                let mut pool = self.pool.lock().unwrap_or_else(|p| p.into_inner());
                pool.push(Pooled::H2(h));
            }
        }
    }
}

impl Read for H2Body {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        while self.pos >= self.buf.len() {
            let Some(h) = self.conn.as_mut() else {
                return Ok(0);
            };
            match h.conn.read_data() {
                Ok(Some(d)) => {
                    self.tracer.recv_data(&d);
                    self.buf = d;
                    self.pos = 0;
                }
                Ok(None) => {
                    self.finish();
                    return Ok(0);
                }
                Err(e) => {
                    self.conn = None;
                    let t = h2_transport(&self.tracer, e, false);
                    return Err(io::Error::other(H2Failure(t.code, t.reason)));
                }
            }
        }
        let n = (self.buf.len() - self.pos).min(out.len());
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Drop for H2Body {
    fn drop(&mut self) {
        // The caller stopped early (often only the empty end-of-stream
        // frame is left): cancel the stream, keep the connection.
        if let Some(h) = self.conn.as_mut() {
            h.conn.abandon();
        }
        self.finish();
    }
}
