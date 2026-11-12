// SPDX-License-Identifier: MIT
//! A small HTTP/1.1 test server that runs the reference `git http-backend`
//! as a CGI program, optionally over TLS with a self-signed certificate.
//!
//! Routes (the first path component selects the behaviour):
//!
//! - `/smart/<repo>/...`: `git http-backend`.
//! - `/auth/smart/<repo>/...`: Basic auth (`user:pass`), then the backend
//!   with `REMOTE_USER` set.
//! - `/redir/<rest>`: `301` to `/<rest>` (query kept).
//! - `/redir-elsewhere/<rest>`: `301` to `/smart/other.git/...`, which does
//!   not end in the requested suffix.
//! - `/dumb/<repo>/info/refs`: a static dumb `info/refs` file.
//! - `/broken/<repo>/info/refs`: a smart-looking body with a wrong
//!   content type.
//! - `/status/<code>/...`: always answers with that status.
//! - `/slow/...`: sends headers, then stalls.
//!
//! Requests in absolute form (`GET http://host/path`) are accepted, so a
//! [`Proxy`] can pass them through unchanged. Every request is recorded in
//! [`Server::log`].

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

/// One request as the server saw it.
#[derive(Debug, Clone)]
pub struct Logged {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body_len: usize,
    pub chunked: bool,
    pub status: u16,
}

impl Logged {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

struct Shared {
    root: PathBuf,
    log: Mutex<Vec<Logged>>,
    stop: AtomicBool,
}

pub struct Server {
    pub port: u16,
    pub tls: bool,
    pub cert_pem: String,
    pub dir: tempfile::TempDir,
    shared: Arc<Shared>,
}

impl Server {
    pub fn start(tls: bool) -> Server {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("www");
        std::fs::create_dir_all(&root).unwrap();
        let shared = Arc::new(Shared {
            root,
            log: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
        });
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let (cert_pem, server_config) = if tls {
            let ck = rcgen::generate_simple_self_signed(vec![
                "localhost".to_string(),
                "127.0.0.1".to_string(),
            ])
            .unwrap();
            let cert_der: CertificateDer<'static> = ck.cert.der().clone();
            let key =
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(ck.signing_key.serialize_der()));
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let cfg = rustls::ServerConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![cert_der], key)
                .unwrap();
            (ck.cert.pem(), Some(Arc::new(cfg)))
        } else {
            (String::new(), None)
        };

        let sh = shared.clone();
        thread::spawn(move || {
            for conn in listener.incoming() {
                if sh.stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(conn) = conn else { continue };
                let sh = sh.clone();
                let server_config = server_config.clone();
                thread::spawn(move || {
                    let _ = conn.set_read_timeout(Some(Duration::from_secs(30)));
                    let stream: Box<dyn Stream> = match server_config {
                        Some(cfg) => {
                            let sc = rustls::ServerConnection::new(cfg).unwrap();
                            Box::new(rustls::StreamOwned::new(sc, conn))
                        }
                        None => Box::new(conn),
                    };
                    let _ = serve_connection(&sh, stream, port);
                });
            }
        });

        Server {
            port,
            tls,
            cert_pem,
            dir,
            shared,
        }
    }

    pub fn root(&self) -> &Path {
        &self.shared.root
    }

    pub fn url(&self, path: &str) -> String {
        let scheme = if self.tls { "https" } else { "http" };
        format!("{scheme}://127.0.0.1:{}{path}", self.port)
    }

    pub fn log(&self) -> Vec<Logged> {
        self.shared.log.lock().unwrap().clone()
    }

    pub fn clear_log(&self) {
        self.shared.log.lock().unwrap().clear();
    }

    /// Write the certificate to a file and return its path.
    pub fn cert_file(&self) -> PathBuf {
        let p = self.dir.path().join("cert.pem");
        std::fs::write(&p, &self.cert_pem).unwrap();
        p
    }

    /// Create a bare repository `<name>` with one commit on `main`.
    pub fn create_repo(&self, name: &str) -> PathBuf {
        let bare = self.root().join(name);
        git(self.dir.path(), &["init", "-q", "--bare", "-b", "main"])
            .arg(&bare)
            .status()
            .unwrap();
        let work = self.dir.path().join(format!("{name}.work"));
        run(git(self.dir.path(), &["init", "-q", "-b", "main"]).arg(&work));
        std::fs::write(work.join("file"), "content\n").unwrap();
        run(&mut git(&work, &["add", "file"]));
        run(&mut git(&work, &["commit", "-q", "-m", "one"]));
        run(git(&work, &["push", "-q"]).arg(&bare).arg("main:main"));
        run(&mut git(&bare, &["config", "http.receivepack", "true"]));
        bare
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

/// A `git` command with a clean, deterministic environment.
pub fn git(dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new("git");
    c.current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE");
    c
}

pub fn run(c: &mut Command) -> Vec<u8> {
    let out = c.output().unwrap();
    assert!(
        out.status.success(),
        "{c:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn read_line(r: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut buf = Vec::new();
    let n = r.read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    while buf.last() == Some(&b'\n') || buf.last() == Some(&b'\r') {
        buf.pop();
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

fn read_body(r: &mut impl BufRead, headers: &[(String, String)]) -> io::Result<(Vec<u8>, bool)> {
    let get = |n: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(n))
            .map(|(_, v)| v.clone())
    };
    if get("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        let mut body = Vec::new();
        loop {
            let line = read_line(r)?.unwrap_or_default();
            let size = usize::from_str_radix(line.split(';').next().unwrap().trim(), 16)
                .map_err(|_| io::Error::other("bad chunk"))?;
            if size == 0 {
                while let Some(l) = read_line(r)? {
                    if l.is_empty() {
                        break;
                    }
                }
                return Ok((body, true));
            }
            let mut chunk = vec![0; size];
            r.read_exact(&mut chunk)?;
            body.extend_from_slice(&chunk);
            read_line(r)?;
        }
    }
    let len: usize = get("content-length")
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; len];
    r.read_exact(&mut body)?;
    Ok((body, false))
}

fn serve_connection(sh: &Shared, stream: Box<dyn Stream>, port: u16) -> io::Result<()> {
    let mut r = BufReader::new(stream);
    loop {
        let Some(line) = read_line(&mut r)? else {
            return Ok(());
        };
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split(' ');
        let method = parts.next().unwrap_or("").to_string();
        let mut target = parts.next().unwrap_or("").to_string();
        let mut headers = Vec::new();
        while let Some(h) = read_line(&mut r)? {
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                headers.push((k.trim().to_string(), v.trim().to_string()));
            }
        }

        // Absolute form: act as a proxy for ourselves.
        if let Some(rest) = target
            .strip_prefix("http://")
            .or_else(|| target.strip_prefix("https://"))
        {
            target = rest
                .find('/')
                .map(|i| rest[i..].to_string())
                .unwrap_or("/".into());
        }

        let (body, chunked) = read_body(&mut r, &headers)?;
        let (status, resp_headers, resp_body, close) =
            handle(sh, &method, &target, &headers, &body, port);
        sh.log.lock().unwrap().push(Logged {
            method: method.clone(),
            target: target.clone(),
            headers: headers.clone(),
            body_len: body.len(),
            chunked,
            status,
        });
        let w = r.get_mut();
        let mut head = format!("HTTP/1.1 {status} {}\r\n", reason(status));
        let has_len = resp_headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("content-length"));
        for (k, v) in &resp_headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        if target.starts_with("/slow/") {
            head.push_str("Content-Length: 1000\r\n\r\n");
            w.write_all(head.as_bytes())?;
            w.flush()?;
            thread::sleep(Duration::from_secs(5));
            return Ok(());
        }
        if has_len {
            head.push_str("\r\n");
            w.write_all(head.as_bytes())?;
            w.write_all(&resp_body)?;
        } else {
            // Like a CGI behind Apache: chunked response.
            head.push_str("Transfer-Encoding: chunked\r\n\r\n");
            w.write_all(head.as_bytes())?;
            for piece in resp_body.chunks(7000) {
                write!(w, "{:x}\r\n", piece.len())?;
                w.write_all(piece)?;
                w.write_all(b"\r\n")?;
            }
            w.write_all(b"0\r\n\r\n")?;
        }
        w.flush()?;
        if close {
            return Ok(());
        }
    }
}

/// A forward proxy that logs the first request of every connection and
/// then tunnels bytes: `CONNECT host:port` opens a tunnel, absolute-form
/// requests are passed on unchanged (the test server accepts them).
pub struct Proxy {
    pub port: u16,
    log: Arc<Mutex<Vec<Logged>>>,
}

impl Proxy {
    pub fn start() -> Proxy {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let log = Arc::new(Mutex::new(Vec::new()));
        let l2 = log.clone();
        thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                let log = l2.clone();
                thread::spawn(move || {
                    let _ = proxy_connection(conn, &log);
                });
            }
        });
        Proxy { port, log }
    }

    pub fn log(&self) -> Vec<Logged> {
        self.log.lock().unwrap().clone()
    }
}

fn proxy_connection(conn: TcpStream, log: &Mutex<Vec<Logged>>) -> io::Result<()> {
    let mut r = BufReader::new(conn.try_clone()?);
    let Some(line) = read_line(&mut r)? else {
        return Ok(());
    };
    let mut parts = line.split(' ');
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    let mut headers = Vec::new();
    let mut raw_head = format!("{line}\r\n");
    while let Some(h) = read_line(&mut r)? {
        if h.is_empty() {
            break;
        }
        raw_head.push_str(&h);
        raw_head.push_str("\r\n");
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    raw_head.push_str("\r\n");
    log.lock().unwrap().push(Logged {
        method: method.clone(),
        target: target.clone(),
        headers,
        body_len: 0,
        chunked: false,
        status: 200,
    });
    let mut client = conn;
    let mut up = if method == "CONNECT" {
        let up = TcpStream::connect(&target)?;
        client.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")?;
        up
    } else {
        let rest = target.split_once("://").map(|x| x.1).unwrap_or(&target);
        let hostport = rest.split('/').next().unwrap();
        let mut up = TcpStream::connect(hostport)?;
        up.write_all(raw_head.as_bytes())?;
        up
    };
    let buffered = r.buffer().to_vec();
    up.write_all(&buffered)?;
    let mut up_r = up.try_clone()?;
    let mut client_w = client.try_clone()?;
    let t = thread::spawn(move || {
        let _ = io::copy(&mut up_r, &mut client_w);
        let _ = client_w.shutdown(Shutdown::Both);
    });
    let mut rr = r.into_inner();
    let _ = io::copy(&mut rr, &mut up);
    let _ = up.shutdown(Shutdown::Both);
    let _ = t.join();
    Ok(())
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        301 => "Moved Permanently",
        302 => "Found",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Intentional Breakage",
        _ => "Status",
    }
}

type Reply = (u16, Vec<(String, String)>, Vec<u8>, bool);

fn handle(
    sh: &Shared,
    method: &str,
    target: &str,
    headers: &[(String, String)],
    body: &[u8],
    port: u16,
) -> Reply {
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.to_string(), String::new()),
    };
    let hdr = |n: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(n))
            .map(|(_, v)| v.clone())
    };
    let text = |status: u16, s: &str| -> Reply {
        (
            status,
            vec![
                ("Content-Type".into(), "text/plain".into()),
                ("Content-Length".into(), s.len().to_string()),
            ],
            s.as_bytes().to_vec(),
            false,
        )
    };

    if let Some(rest) = path.strip_prefix("/redir/") {
        let q = if query.is_empty() {
            String::new()
        } else {
            format!("?{query}")
        };
        let _ = port;
        return (
            301,
            vec![
                ("Location".into(), format!("/{rest}{q}")),
                ("Content-Length".into(), "0".into()),
            ],
            Vec::new(),
            false,
        );
    }
    if path.starts_with("/redir-elsewhere/") {
        return (
            301,
            vec![
                ("Location".into(), "/smart/other.git/info/refs".into()),
                ("Content-Length".into(), "0".into()),
            ],
            Vec::new(),
            false,
        );
    }
    if path.starts_with("/slow/") {
        // The connection loop adds `Content-Length: 1000` and stalls.
        return (
            200,
            vec![(
                "Content-Type".into(),
                "application/x-git-upload-pack-advertisement".into(),
            )],
            Vec::new(),
            false,
        );
    }
    if let Some(rest) = path.strip_prefix("/status/") {
        let code: u16 = rest.split('/').next().unwrap().parse().unwrap();
        return text(code, "nope\n");
    }
    if let Some(rest) = path.strip_prefix("/dumb/") {
        if rest.ends_with("/info/refs") {
            return text(
                200,
                "95dcfa3633004da0049d3d0fa03f80589cbcaf31\trefs/heads/main\n",
            );
        }
        return text(404, "not found\n");
    }
    if path.starts_with("/broken/") {
        let body = b"001e# service=git-upload-pack\n0000".to_vec();
        return (
            200,
            vec![
                ("Content-Type".into(), "text/html".into()),
                ("Content-Length".into(), body.len().to_string()),
            ],
            body,
            false,
        );
    }
    let mut remote_user = None;
    let mut cgi_path = path.clone();
    if let Some(rest) = path.strip_prefix("/auth/") {
        let expected = format!("Basic {}", b64("user:pass"));
        if hdr("authorization").as_deref() != Some(expected.as_str()) {
            return (
                401,
                vec![
                    ("WWW-Authenticate".into(), "Basic realm=\"git\"".into()),
                    ("Content-Length".into(), "0".into()),
                ],
                Vec::new(),
                false,
            );
        }
        remote_user = Some("user".to_string());
        cgi_path = format!("/{rest}");
    }
    if let Some(rest) = cgi_path.strip_prefix("/smart/") {
        return cgi(
            sh,
            method,
            &format!("/{rest}"),
            &query,
            headers,
            body,
            remote_user,
        );
    }
    let _ = method;
    text(404, "not found\n")
}

fn cgi(
    sh: &Shared,
    method: &str,
    path_info: &str,
    query: &str,
    headers: &[(String, String)],
    body: &[u8],
    remote_user: Option<String>,
) -> Reply {
    let hdr = |n: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(n))
            .map(|(_, v)| v.clone())
    };
    let mut c = Command::new("git");
    c.arg("http-backend")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_PROJECT_ROOT", &sh.root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("REQUEST_METHOD", method)
        .env("PATH_INFO", path_info)
        .env("QUERY_STRING", query)
        .env("REMOTE_ADDR", "127.0.0.1")
        .env("CONTENT_LENGTH", body.len().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(ct) = hdr("content-type") {
        c.env("CONTENT_TYPE", ct);
    }
    if let Some(p) = hdr("git-protocol") {
        c.env("HTTP_GIT_PROTOCOL", p);
    }
    if let Some(e) = hdr("content-encoding") {
        c.env("HTTP_CONTENT_ENCODING", e);
    }
    if let Some(u) = remote_user {
        c.env("REMOTE_USER", u);
    }
    let mut child = c.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let body = body.to_vec();
    let writer = thread::spawn(move || {
        let _ = stdin.write_all(&body);
    });
    let out = child.wait_with_output().unwrap();
    let _ = writer.join();
    let data = out.stdout;
    let split = data
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, 4))
        .or_else(|| data.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2)));
    let (head, rest) = match split {
        Some((i, n)) => (&data[..i], data[i + n..].to_vec()),
        None => (&data[..], Vec::new()),
    };
    let mut status = 200;
    let mut hs = Vec::new();
    for line in String::from_utf8_lossy(head).lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        if k.eq_ignore_ascii_case("status") {
            status = v.split(' ').next().unwrap().parse().unwrap_or(500);
        } else {
            hs.push((k.to_string(), v.to_string()));
        }
    }
    (status, hs, rest, false)
}

pub fn b64(s: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let b = s.as_bytes();
    let mut out = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[allow(unused)]
pub fn env_map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}
