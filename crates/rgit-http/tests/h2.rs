// SPDX-License-Identifier: MIT
//! HTTP/2 (ALPN and `h2c` upgrade) and GSS-Negotiate against the test
//! servers in `support/`. The HTTP/2 server is the `h2` crate, so these
//! tests check rgit's own framing layer against an independent
//! implementation. Negotiate uses a mock [`Negotiator`]: no KDC is
//! available in tests.

#[path = "support/h2server.rs"]
mod h2server;
mod support;

use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Mutex};

use rgit_http::gss::Negotiator;
use rgit_http::{
    Credential, CredentialProvider, Delegation, EmptyAuth, Error, HttpOptions, HttpVersion,
    RpcBody, Service, SmartHttp,
};
use support::{Server, git, run};

fn opts() -> HttpOptions {
    HttpOptions::default()
}

fn head_of(repo: &Path) -> String {
    let out = run(&mut git(repo, &["rev-parse", "main"]));
    String::from_utf8(out).unwrap().trim().to_string()
}

fn pkt(data: &str) -> Vec<u8> {
    let mut v = format!("{:04x}", data.len() + 4).into_bytes();
    v.extend_from_slice(data.as_bytes());
    v
}

fn tls_opts(s: &Server) -> HttpOptions {
    let mut o = opts();
    o.ssl_ca_info = Some(s.cert_file());
    o
}

#[test]
fn alpn_h2_discovery_and_ls_refs() {
    let s = h2server::start_h2();
    let bare = s.create_repo("repo.git");
    let mut h = SmartHttp::new(&s.url("/smart/repo.git"), tls_opts(&s)).unwrap();
    h.set_git_protocol(Some("version=2".into()));
    let adv = h.discover(Service::UploadPack).unwrap();
    assert!(String::from_utf8_lossy(&adv.data).contains("ls-refs"));
    let mut req = pkt("command=ls-refs\n");
    req.extend_from_slice(b"0000");
    let mut resp = h.rpc(Service::UploadPack, RpcBody::Bytes(&req)).unwrap();
    let mut body = Vec::new();
    resp.read_to_end(&mut body).unwrap();
    assert!(
        String::from_utf8_lossy(&body).contains(&format!("{} refs/heads/main", head_of(&bare))),
        "{}",
        String::from_utf8_lossy(&body)
    );
    let log = s.log();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].method, "GET");
    assert_eq!(log[1].method, "POST");
    // No connection-specific fields on HTTP/2; the rest arrives.
    for l in &log {
        assert_eq!(l.header("host"), None);
        assert_eq!(l.header("git-protocol"), Some("version=2"));
        assert!(l.header("user-agent").unwrap().starts_with("git/"));
    }
    assert_eq!(
        log[1].header("content-length"),
        Some(req.len().to_string().as_str())
    );
}

fn new_commit_pack(s: &Server, name: &str, size: usize) -> (String, String, Vec<u8>) {
    let work = s.dir.path().join(format!("{name}.work"));
    let old = head_of(&work);
    // Incompressible content so the pack exceeds the HTTP/2 windows.
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let data: Vec<u8> = (0..size)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect();
    std::fs::write(work.join("big"), data).unwrap();
    run(&mut git(&work, &["add", "big"]));
    run(&mut git(&work, &["commit", "-qm", "big"]));
    let new = head_of(&work);
    let mut c = git(&work, &["pack-objects", "--stdout", "--revs"]);
    c.stdin(std::process::Stdio::piped());
    c.stdout(std::process::Stdio::piped());
    let mut child = c.spawn().unwrap();
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, "{new}\n^{old}").unwrap();
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    (old, new, out.stdout)
}

fn push_request(old: &str, new: &str, pack: &[u8]) -> Vec<u8> {
    let mut req = pkt(&format!("{old} {new} refs/heads/main\0report-status\n"));
    req.extend_from_slice(b"0000");
    req.extend_from_slice(pack);
    req
}

#[test]
fn h2_streamed_push_respects_flow_control() {
    let s = h2server::start_h2();
    let bare = s.create_repo("repo.git");
    // 3 MiB: more than the default 64 KiB windows of the server.
    let (old, new, pack) = new_commit_pack(&s, "repo.git", 3 << 20);
    let mut o = tls_opts(&s);
    o.post_buffer = 64 * 1024;
    let mut h = SmartHttp::new(&s.url("/smart/repo.git"), o).unwrap();
    h.discover(Service::ReceivePack).unwrap();
    let req = push_request(&old, &new, &pack);
    let mut cursor = std::io::Cursor::new(req.clone());
    let mut resp = h
        .rpc(Service::ReceivePack, RpcBody::Stream(&mut cursor))
        .unwrap();
    let mut body = Vec::new();
    resp.read_to_end(&mut body).unwrap();
    assert!(
        String::from_utf8_lossy(&body).contains("ok refs/heads/main"),
        "{}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(head_of(&bare), new);
    let log = s.log();
    assert_eq!(log.len(), 3, "discovery, probe, push");
    assert_eq!(log[2].body_len, req.len());
    assert_eq!(log[2].header("transfer-encoding"), None);
    assert_eq!(log[2].header("content-length"), None);
}

#[test]
fn h2_large_response_and_basic_auth() {
    let s = h2server::start_h2();
    let bare = s.create_repo("repo.git");
    // A large blob makes the fetch response exceed the receive windows.
    new_commit_pack(&s, "repo.git", 2 << 20);
    let work = s.dir.path().join("repo.git.work");
    run(git(&work, &["push", "-q"]).arg(&bare).arg("main:main"));
    let helper = Arc::new(Mutex::new(Helper::default()));
    let mut h = SmartHttp::new(&s.url("/auth/smart/repo.git"), tls_opts(&s))
        .unwrap()
        .with_credentials(Shared(helper.clone()));
    h.set_git_protocol(Some("version=2".into()));
    h.discover(Service::UploadPack).unwrap();
    let mut req = pkt("command=fetch\n");
    req.extend_from_slice(b"0001");
    req.extend(pkt(&format!("want {}\n", head_of(&bare))));
    req.extend(pkt("done\n"));
    req.extend_from_slice(b"0000");
    let mut resp = h.rpc(Service::UploadPack, RpcBody::Bytes(&req)).unwrap();
    let mut body = Vec::new();
    resp.read_to_end(&mut body).unwrap();
    assert!(body.len() > 2 << 20, "{}", body.len());
    let helper = helper.lock().unwrap();
    assert_eq!(helper.filled.len(), 1);
    assert_eq!(helper.approved.len(), 1);
    let log = s.log();
    assert_eq!(log[0].status, 401);
    assert_eq!(log[1].status, 200);
}

#[test]
fn h2c_upgrade_then_goaway_falls_back_to_a_new_connection() {
    let s = h2server::start_h2c();
    s.create_repo("repo.git");
    let mut o = opts();
    o.version = HttpVersion::Http2;
    let mut h = SmartHttp::new(&s.url("/smart/repo.git"), o).unwrap();
    h.set_git_protocol(Some("version=2".into()));
    let adv = h.discover(Service::UploadPack).unwrap();
    assert!(String::from_utf8_lossy(&adv.data).contains("ls-refs"));
    let mut req = pkt("command=ls-refs\n");
    req.extend_from_slice(b"0000");
    let mut resp = h.rpc(Service::UploadPack, RpcBody::Bytes(&req)).unwrap();
    let mut body = Vec::new();
    resp.read_to_end(&mut body).unwrap();
    assert!(String::from_utf8_lossy(&body).contains("refs/heads/main"));
    let log = s.log();
    assert_eq!(log.len(), 2);
    for l in &log {
        assert_eq!(l.header("upgrade"), Some("h2c"));
        assert_eq!(l.header("connection"), Some("Upgrade, HTTP2-Settings"));
        let settings = l.header("http2-settings").unwrap();
        assert!(!settings.contains('=') && !settings.contains('+'));
    }
}

#[test]
fn http11_and_default_versions_on_plain_http() {
    // Without http.version=HTTP/2, plain HTTP never asks for an upgrade.
    let s = Server::start(false);
    s.create_repo("repo.git");
    for v in [HttpVersion::Default, HttpVersion::Http11] {
        let mut o = opts();
        o.version = v;
        let mut h = SmartHttp::new(&s.url("/smart/repo.git"), o).unwrap();
        h.discover(Service::UploadPack).unwrap();
    }
    // HTTP/2 over TLS against a server without ALPN: HTTP/1.1.
    let t = Server::start(true);
    t.create_repo("repo.git");
    let mut o = tls_opts(&t);
    o.version = HttpVersion::Http2;
    let mut h = SmartHttp::new(&t.url("/smart/repo.git"), o).unwrap();
    h.discover(Service::UploadPack).unwrap();
    for l in s.log().iter().chain(t.log().iter()) {
        assert_eq!(l.header("upgrade"), None);
    }
}

#[derive(Default)]
struct Helper {
    filled: Vec<Credential>,
    approved: Vec<Credential>,
    rejected: Vec<Credential>,
}

struct Shared(Arc<Mutex<Helper>>);

impl CredentialProvider for Shared {
    fn fill(&mut self, cred: &mut Credential) -> Result<(), String> {
        self.0.lock().unwrap().filled.push(cred.clone());
        cred.username = Some("user".into());
        cred.password = Some("pass".into());
        Ok(())
    }
    fn approve(&mut self, cred: &Credential) {
        self.0.lock().unwrap().approved.push(cred.clone());
    }
    fn reject(&mut self, cred: &Credential) {
        self.0.lock().unwrap().rejected.push(cred.clone());
    }
}

/// A mock GSS-API: `tok` when it has a "ticket", else an error.
struct Mock {
    ticket: bool,
    calls: Arc<Mutex<Vec<(String, Delegation)>>>,
}

impl Negotiator for Mock {
    fn token(&mut self, host: &str, delegation: Delegation) -> Result<Vec<u8>, String> {
        self.calls
            .lock()
            .unwrap()
            .push((host.to_string(), delegation));
        if self.ticket {
            Ok(b"tok".to_vec())
        } else {
            Err("gss_init_sec_context() failed: no ticket".into())
        }
    }
}

struct Run {
    statuses: Vec<(String, u16, Option<String>)>,
    helper: Helper,
    calls: Vec<(String, Delegation)>,
    result: Result<(), String>,
}

fn negotiate_run(route: &str, empty_auth: EmptyAuth, ticket: bool) -> Run {
    let s = Server::start(false);
    s.create_repo("repo.git");
    let mut o = opts();
    o.empty_auth = empty_auth;
    o.delegation = Delegation::Policy;
    let calls = Arc::new(Mutex::new(Vec::new()));
    let helper = Arc::new(Mutex::new(Helper::default()));
    let mut h = SmartHttp::new(&s.url(&format!("/{route}/smart/repo.git")), o)
        .unwrap()
        .with_credentials(Shared(helper.clone()))
        .with_negotiator(Mock {
            ticket,
            calls: calls.clone(),
        });
    h.set_git_protocol(Some("version=2".into()));
    let result = h
        .discover(Service::UploadPack)
        .and_then(|_| {
            let mut req = pkt("command=ls-refs\n");
            req.extend_from_slice(b"0000");
            let mut r = h.rpc(Service::UploadPack, RpcBody::Bytes(&req))?;
            std::io::copy(&mut r, &mut std::io::sink())?;
            Ok(())
        })
        .map_err(|e: Error| e.to_string());
    drop(h);
    let helper = std::mem::take(&mut *helper.lock().unwrap());
    let calls = calls.lock().unwrap().clone();
    Run {
        statuses: s
            .log()
            .iter()
            .map(|l| {
                (
                    l.method.clone(),
                    l.status,
                    l.header("authorization").map(String::from),
                )
            })
            .collect(),
        helper,
        calls,
        result,
    }
}

const NEG: Option<&str> = Some("Negotiate dG9r");

#[test]
fn negotiate_auto_retries_with_empty_credentials() {
    let r = negotiate_run("negotiate", EmptyAuth::Auto, true);
    assert_eq!(r.result, Ok(()));
    let s: Vec<_> = r
        .statuses
        .iter()
        .map(|(m, s, a)| (m.as_str(), *s, a.as_deref()))
        .collect();
    assert_eq!(
        s,
        vec![
            ("GET", 401, None),
            ("GET", 401, None),
            ("GET", 200, NEG),
            // Later requests carry a fresh token up front.
            ("POST", 200, NEG),
        ]
    );
    assert!(r.helper.filled.is_empty());
    assert_eq!(
        r.calls,
        vec![
            ("127.0.0.1".to_string(), Delegation::Policy),
            ("127.0.0.1".to_string(), Delegation::Policy)
        ]
    );
}

#[test]
fn negotiate_without_ticket_falls_back_to_the_helper() {
    let r = negotiate_run("negotiate", EmptyAuth::Auto, false);
    assert_eq!(r.result, Ok(()));
    let n401 = r.statuses.iter().filter(|s| s.1 == 401).count();
    // Like git with libcurl: three 401s, then Basic from the helper.
    assert_eq!(n401, 3);
    assert_eq!(r.helper.filled.len(), 1);
    assert_eq!(
        r.helper.filled[0].wwwauth,
        vec!["Negotiate".to_string(), "Basic realm=\"t\"".to_string()]
    );
    assert_eq!(r.helper.approved.len(), 1);
    assert!(r.statuses[3].2.as_deref().unwrap().starts_with("Basic "));
}

#[test]
fn negotiate_empty_auth_true_and_false() {
    let r = negotiate_run("negotiate", EmptyAuth::Always, true);
    assert_eq!(r.result, Ok(()));
    let s: Vec<_> = r.statuses.iter().map(|x| (x.1, x.2.as_deref())).collect();
    assert_eq!(s[..2], [(401, None), (200, NEG)]);

    let r = negotiate_run("negotiate", EmptyAuth::Never, true);
    assert_eq!(r.result, Ok(()));
    assert!(r.calls.is_empty());
    assert_eq!(r.statuses.iter().filter(|s| s.1 == 401).count(), 1);
    assert_eq!(r.helper.filled.len(), 1);
}

#[test]
fn negotiate_only_server_does_not_get_basic() {
    let r = negotiate_run("negotiate-only", EmptyAuth::Auto, false);
    assert!(
        r.result
            .as_ref()
            .unwrap_err()
            .contains("Authentication failed"),
        "{:?}",
        r.result
    );
    assert!(r.statuses.iter().all(|s| s.2.is_none()), "{:?}", r.statuses);
    assert_eq!(r.helper.filled.len(), 1);
    assert_eq!(r.helper.rejected.len(), 1);
    assert_eq!(r.helper.rejected[0].wwwauth, vec!["Negotiate".to_string()]);
}
