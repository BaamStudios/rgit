// SPDX-License-Identifier: MIT
//! The `http.*` settings of issue #170 end to end against the test server:
//! 429 retries, cookies, `http.curloptResolve`, SOCKS proxies, proxy URL
//! checks, pre-encoded (Bearer) credentials from a helper, and the
//! `http.postBuffer` chunking rule.

#![allow(clippy::field_reassign_with_default)]

mod support;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use rgit_http::{Credential, CredentialProvider, Error, HttpOptions, RpcBody, Service, SmartHttp};
use support::Server;

fn diagnostics(h: &mut SmartHttp<'_>) -> Arc<Mutex<Vec<String>>> {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let l2 = lines.clone();
    h.set_diagnostics(move |l| l2.lock().unwrap().push(l.to_string()));
    lines
}

#[test]
fn retry_after_429() {
    let s = Server::start(false);
    s.create_repo("repo.git");
    let mut o = HttpOptions::default();
    o.max_retries = 3;
    let mut h = SmartHttp::new(&s.url("/once429/1/smart/repo.git"), o).unwrap();
    let lines = diagnostics(&mut h);
    h.discover(Service::UploadPack).unwrap();
    assert_eq!(
        *lines.lock().unwrap(),
        ["warning: rate limited, waiting 1 seconds before retry"]
    );
    let log = s.log();
    assert_eq!(log.len(), 2);
    assert_eq!((log[0].status, log[1].status), (429, 200));
}

#[test]
fn no_retries_by_default_and_limits() {
    let s = Server::start(false);
    s.create_repo("repo.git");
    let mut h =
        SmartHttp::new(&s.url("/once429/1/smart/repo.git"), HttpOptions::default()).unwrap();
    let e = h.discover(Service::UploadPack).unwrap_err();
    assert_eq!(e.status(), Some(429));
    assert_eq!(e.remote_lines(), ["Rate limited"]);

    // The server asks for longer than http.maxRetryTime allows.
    let mut o = HttpOptions::default();
    o.max_retries = 3;
    o.max_retry_time = 0;
    let mut h = SmartHttp::new(&s.url("/once429/5/smart/repo.git"), o).unwrap();
    let lines = diagnostics(&mut h);
    let e = h.discover(Service::UploadPack).unwrap_err();
    assert_eq!(e.status(), Some(429));
    assert_eq!(
        *lines.lock().unwrap(),
        ["error: response requested a delay greater than http.maxRetryTime (5 > 0 seconds)"]
    );

    // The last allowed attempt fails with "rate limited by".
    let mut o = HttpOptions::default();
    o.max_retries = 1;
    let mut h = SmartHttp::new(&s.url("/once429/none/smart/other.git"), o).unwrap();
    let e = h.discover(Service::UploadPack).unwrap_err();
    assert!(matches!(e, Error::RateLimited { secs: 0, .. }));
    assert_eq!(
        e.to_string(),
        format!(
            "rate limited by '{}', please try again in 0 seconds",
            s.url("/once429/none/smart/other.git/")
        )
    );
}

#[test]
fn cookies_are_saved_and_sent() {
    let s = Server::start(false);
    s.create_repo("repo.git");
    let jar = s.dir.path().join("cookies.txt");
    std::fs::write(&jar, "127.0.0.1\tFALSE\t/cookie\tFALSE\t0\told\tone\n").unwrap();
    let mut o = HttpOptions::default();
    o.cookie_file = Some(jar.clone());
    o.save_cookies = true;
    let mut h = SmartHttp::new(&s.url("/cookie/smart/repo.git"), o).unwrap();
    h.discover(Service::UploadPack).unwrap();
    let _ = h.rpc(Service::UploadPack, RpcBody::Bytes(b"0000")).unwrap();
    let log = s.log();
    assert_eq!(log[0].header("cookie"), Some("old=one"));
    // The cookie set for /cookie/smart/repo.git/info does not apply to the
    // POST one level up.
    assert_eq!(log[1].header("cookie"), Some("old=one"));
    let saved = std::fs::read_to_string(&jar).unwrap();
    let lines: Vec<&str> = saved.lines().filter(|l| !l.starts_with('#')).collect();
    assert!(
        lines.contains(&"127.0.0.1\tFALSE\t/cookie/smart/repo.git/info\tFALSE\t0\tname\tvalue"),
        "{saved}"
    );
    assert!(
        lines.contains(&"127.0.0.1\tFALSE\t/cookie/smart/repo.git\tFALSE\t0\tname\tvalue"),
        "{saved}"
    );
}

#[test]
fn curlopt_resolve() {
    let s = Server::start(false);
    s.create_repo("repo.git");
    let mut o = HttpOptions::default();
    o.resolve = vec![format!("rgit-test.invalid:{}:127.0.0.1", s.port)];
    let url = format!("http://rgit-test.invalid:{}/smart/repo.git", s.port);
    let mut h = SmartHttp::new(&url, o.clone()).unwrap();
    h.discover(Service::UploadPack).unwrap();
    assert_eq!(
        s.log()[0].header("host"),
        Some(format!("rgit-test.invalid:{}", s.port).as_str())
    );
    o.resolve.push(format!("-rgit-test.invalid:{}", s.port));
    let mut h = SmartHttp::new(&url, o.clone()).unwrap();
    assert!(h.discover(Service::UploadPack).is_err());
    o.resolve = vec!["bad".into()];
    let mut h = SmartHttp::new(&url, o).unwrap();
    let e = h.discover(Service::UploadPack).unwrap_err().to_string();
    assert!(
        e.ends_with("Could not parse CURLOPT_RESOLVE entry 'bad'"),
        "{e}"
    );
}

/// A one-connection SOCKS5 proxy (no authentication, domain or IPv4
/// targets) that relays to the target.
fn socks5_proxy() -> (u16, thread::JoinHandle<String>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let t = thread::spawn(move || {
        let (mut c, _) = l.accept().unwrap();
        let mut g = [0u8; 3];
        c.read_exact(&mut g).unwrap();
        c.write_all(&[5, 0]).unwrap();
        let mut head = [0u8; 4];
        c.read_exact(&mut head).unwrap();
        let host = match head[3] {
            3 => {
                let mut n = [0u8; 1];
                c.read_exact(&mut n).unwrap();
                let mut h = vec![0u8; n[0] as usize];
                c.read_exact(&mut h).unwrap();
                String::from_utf8(h).unwrap()
            }
            _ => {
                let mut a = [0u8; 4];
                c.read_exact(&mut a).unwrap();
                format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3])
            }
        };
        let mut p = [0u8; 2];
        c.read_exact(&mut p).unwrap();
        let port = u16::from_be_bytes(p);
        let up = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).unwrap();
        let (mut c2, mut up2) = (c.try_clone().unwrap(), up.try_clone().unwrap());
        let a = thread::spawn(move || {
            let _ = std::io::copy(&mut c2, &mut up2);
            let _ = up2.shutdown(std::net::Shutdown::Write);
        });
        let (mut up, mut c) = (up, c);
        let _ = std::io::copy(&mut up, &mut c);
        let _ = c.shutdown(std::net::Shutdown::Write);
        let _ = a.join();
        host
    });
    (port, t)
}

#[test]
fn socks5h_proxy() {
    let s = Server::start(false);
    s.create_repo("repo.git");
    let (pport, t) = socks5_proxy();
    let mut o = HttpOptions::default();
    o.proxy = Some(format!("socks5h://127.0.0.1:{pport}"));
    let url = format!("http://localhost:{}/smart/repo.git", s.port);
    {
        let mut h = SmartHttp::new(&url, o).unwrap();
        h.discover(Service::UploadPack).unwrap();
        // Requests through a SOCKS proxy use the origin form.
        assert_eq!(
            s.log()[0].target,
            "/smart/repo.git/info/refs?service=git-upload-pack"
        );
    }
    assert_eq!(t.join().unwrap(), "localhost");
}

#[test]
fn invalid_proxy_urls() {
    let s = Server::start(false);
    for (proxy, msg) in [
        (
            "htpp://127.0.0.1",
            "Invalid proxy URL 'htpp://127.0.0.1': unsupported proxy scheme 'htpp'",
        ),
        (
            "localhost/path",
            "Invalid proxy URL 'localhost/path': only SOCKS proxies support paths",
        ),
    ] {
        let mut o = HttpOptions::default();
        o.proxy = Some(proxy.into());
        let mut h = SmartHttp::new(&s.url("/smart/repo.git"), o).unwrap();
        assert_eq!(
            h.discover(Service::UploadPack).unwrap_err().to_string(),
            msg
        );
    }
    // A proxy that cannot be reached names the target and the proxy.
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let dead = l.local_addr().unwrap().port();
    drop(l);
    let mut o = HttpOptions::default();
    o.proxy = Some(format!("http://127.0.0.1:{dead}"));
    let mut h = SmartHttp::new("http://example.invalid/r.git", o).unwrap();
    let e = h.discover(Service::UploadPack).unwrap_err().to_string();
    assert!(
        e.contains("Failed to connect to example.invalid:80 over proxy 127.0.0.1 after"),
        "{e}"
    );
}

#[derive(Default)]
struct Bearer {
    filled: Vec<Credential>,
    approved: Vec<Credential>,
}

impl CredentialProvider for Bearer {
    fn fill(&mut self, cred: &mut Credential) -> Result<(), String> {
        self.filled.push(cred.clone());
        cred.authtype = Some("Bearer".into());
        cred.credential = Some("s3cret".into());
        Ok(())
    }
    fn approve(&mut self, cred: &Credential) {
        self.approved.push(cred.clone());
    }
    fn reject(&mut self, _cred: &Credential) {
        panic!("rejected");
    }
}

#[test]
fn bearer_credential_from_helper() {
    let s = Server::start(false);
    s.create_repo("repo.git");
    let mut helper = Bearer::default();
    {
        let mut h = SmartHttp::new(&s.url("/bearer/smart/repo.git"), HttpOptions::default())
            .unwrap()
            .with_credentials(&mut helper);
        h.discover(Service::UploadPack).unwrap();
    }
    assert_eq!(
        helper.filled[0].wwwauth,
        ["Bearer realm=\"t\"", "Basic realm=\"t\""]
    );
    assert_eq!(helper.approved.len(), 1);
    assert!(helper.approved[0].wwwauth.is_empty());
    let log = s.log();
    assert_eq!(log[1].header("authorization"), Some("Bearer s3cret"));
}

#[test]
fn proactive_auth() {
    let s = Server::start(false);
    s.create_repo("repo.git");
    let mut helper = Bearer::default();
    let mut o = HttpOptions::default();
    o.proactive_auth = rgit_http::ProactiveAuth::Auto;
    {
        let mut h = SmartHttp::new(&s.url("/bearer/smart/repo.git"), o)
            .unwrap()
            .with_credentials(&mut helper);
        h.discover(Service::UploadPack).unwrap();
    }
    assert!(helper.filled[0].wwwauth.is_empty());
    let log = s.log();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].header("authorization"), Some("Bearer s3cret"));
}

#[test]
fn post_buffer_leaves_room_for_a_packet() {
    let s = Server::start(false);
    s.create_repo("repo.git");
    for (buffer, chunked) in [(65536, true), (100_000, false)] {
        s.clear_log();
        let mut o = HttpOptions::default();
        o.post_buffer = buffer;
        let mut h = SmartHttp::new(&s.url("/smart/repo.git"), o).unwrap();
        h.discover(Service::UploadPack).unwrap();
        let body = [b"0032want 0000000000000000000000000000000000000000\n".as_slice(); 5].concat();
        let mut resp = h.rpc(Service::UploadPack, RpcBody::Bytes(&body)).unwrap();
        let _ = resp.read_to_end(&mut Vec::new());
        let log = s.log();
        assert_eq!(log.last().unwrap().chunked, chunked, "postBuffer {buffer}");
    }
}
