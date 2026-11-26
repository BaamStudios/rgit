// SPDX-License-Identifier: MIT
//! HTTP/2 test servers: the `h2` crate on tokio behind TLS (ALPN), and a
//! hand-written `h2c` upgrade for one request.

#![allow(dead_code)]

use std::io::{self, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use crate::support::{
    Logged, Server, Shared, handle, read_body, read_line, serve_connection, shared_in, tls_config,
};

/// The routes over HTTP/2 with TLS (ALPN `h2` only).
pub fn start_h2() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let shared = shared_in(&dir);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let (cert_pem, cfg) = tls_config(&[b"h2"]);
    let sh = shared.clone();
    thread::spawn(move || run(listener, cfg, sh, port));
    Server {
        port,
        tls: true,
        cert_pem,
        dir,
        shared,
    }
}

/// Plain HTTP: the first request on the first connection is upgraded to
/// `h2c` (if it asks for it) and answered on stream 1, followed by
/// `GOAWAY`; everything else is HTTP/1.1.
pub fn start_h2c() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let shared = shared_in(&dir);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let sh = shared.clone();
    thread::spawn(move || {
        let mut first = true;
        for conn in listener.incoming() {
            if sh.stop.load(Ordering::SeqCst) {
                break;
            }
            let Ok(conn) = conn else { continue };
            let sh = sh.clone();
            let upgrade = std::mem::take(&mut first);
            thread::spawn(move || {
                let _ = conn.set_read_timeout(Some(Duration::from_secs(30)));
                if upgrade {
                    let _ = serve_h2c(&sh, conn, port);
                } else {
                    let _ = serve_connection(&sh, Box::new(conn), port);
                }
            });
        }
    });
    Server {
        port,
        tls: false,
        cert_pem: String::new(),
        dir,
        shared,
    }
}

/// Fields HTTP/2 does not allow in a response.
fn connection_specific(name: &str) -> bool {
    ["connection", "transfer-encoding", "keep-alive", "upgrade"]
        .iter()
        .any(|n| name.eq_ignore_ascii_case(n))
}

/// Serve the routes over TLS + HTTP/2 until the server is dropped.
fn run(listener: TcpListener, cfg: Arc<rustls::ServerConfig>, sh: Arc<Shared>, port: u16) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(cfg);
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                continue;
            };
            if sh.stop.load(Ordering::SeqCst) {
                break;
            }
            let acceptor = acceptor.clone();
            let sh = sh.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(sock).await else {
                    return;
                };
                let Ok(mut conn) = h2::server::handshake(tls).await else {
                    return;
                };
                while let Some(Ok((req, respond))) = conn.accept().await {
                    let sh = sh.clone();
                    tokio::spawn(async move {
                        let _ = serve_request(sh, req, respond, port).await;
                    });
                }
            });
        }
    });
}

async fn serve_request(
    sh: Arc<Shared>,
    req: http::Request<h2::RecvStream>,
    mut respond: h2::server::SendResponse<bytes::Bytes>,
    port: u16,
) -> Result<(), h2::Error> {
    let (parts, mut body) = req.into_parts();
    let mut data = Vec::new();
    while let Some(chunk) = body.data().await {
        let chunk = chunk?;
        let _ = body.flow_control().release_capacity(chunk.len());
        data.extend_from_slice(&chunk);
    }
    let method = parts.method.to_string();
    let target = parts
        .uri
        .path_and_query()
        .map(|p| p.to_string())
        .unwrap_or_else(|| "/".into());
    let headers: Vec<(String, String)> = parts
        .headers
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let body_len = data.len();
    let (sh2, m2, t2, h2) = (sh.clone(), method.clone(), target.clone(), headers.clone());
    let (status, resp_headers, resp_body, _close) =
        tokio::task::spawn_blocking(move || handle(&sh2, &m2, &t2, &h2, &data, port))
            .await
            .unwrap();
    sh.log.lock().unwrap().push(Logged {
        method,
        target,
        headers,
        body_len,
        chunked: false,
        status,
    });
    let mut resp = http::Response::builder().status(status);
    for (k, v) in &resp_headers {
        if !connection_specific(k) {
            resp = resp.header(k.to_ascii_lowercase(), v);
        }
    }
    let end = resp_body.is_empty();
    let mut send = respond.send_response(resp.body(()).unwrap(), end)?;
    if !end {
        send.send_data(bytes::Bytes::from(resp_body), true)?;
    }
    Ok(())
}

fn frame(kind: u8, flags: u8, stream: u32, payload: &[u8]) -> Vec<u8> {
    let mut f = (payload.len() as u32).to_be_bytes()[1..].to_vec();
    f.push(kind);
    f.push(flags);
    f.extend_from_slice(&stream.to_be_bytes());
    f.extend_from_slice(payload);
    f
}

/// An HPACK string literal without Huffman coding (short strings only).
fn hstr(out: &mut Vec<u8>, s: &str) {
    assert!(s.len() < 127);
    out.push(s.len() as u8);
    out.extend_from_slice(s.as_bytes());
}

/// One connection that upgrades its first request to `h2c`, answers it
/// on stream 1 and sends `GOAWAY`.
fn serve_h2c(sh: &Shared, conn: TcpStream, port: u16) -> io::Result<()> {
    let mut w = conn.try_clone()?;
    let mut r = BufReader::new(conn);
    let line = read_line(&mut r)?.unwrap_or_default();
    let mut parts = line.split(' ');
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    let mut headers = Vec::new();
    while let Some(h) = read_line(&mut r)? {
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let (body, chunked) = read_body(&mut r, &headers)?;
    let (status, resp_headers, resp_body, _) = handle(sh, &method, &target, &headers, &body, port);
    sh.log.lock().unwrap().push(Logged {
        method,
        target,
        headers,
        body_len: body.len(),
        chunked,
        status,
    });
    w.write_all(
        b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: h2c\r\nConnection: Upgrade\r\n\r\n",
    )?;
    let mut preface = [0u8; 24];
    r.read_exact(&mut preface)?;
    assert_eq!(&preface, b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n");
    // Our SETTINGS, the response on stream 1 and GOAWAY in one write.
    let mut out = frame(0x4, 0, 0, &[]);
    let mut block = vec![0x08];
    hstr(&mut block, &status.to_string());
    for (k, v) in &resp_headers {
        if !connection_specific(k) {
            block.push(0x00);
            hstr(&mut block, &k.to_ascii_lowercase());
            hstr(&mut block, v);
        }
    }
    if resp_body.is_empty() {
        out.extend(frame(0x1, 0x4 | 0x1, 1, &block));
    } else {
        out.extend(frame(0x1, 0x4, 1, &block));
        let n = resp_body.chunks(16384).count();
        for (i, c) in resp_body.chunks(16384).enumerate() {
            out.extend(frame(0x0, if i + 1 == n { 0x1 } else { 0 }, 1, c));
        }
    }
    out.extend(frame(0x7, 0, 0, &[0, 0, 0, 1, 0, 0, 0, 0]));
    w.write_all(&out)?;
    w.flush()?;
    // Wait for the client to close.
    let mut sink = [0u8; 4096];
    while r.read(&mut sink)? > 0 {}
    Ok(())
}
