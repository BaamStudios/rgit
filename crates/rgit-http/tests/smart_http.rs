// SPDX-License-Identifier: MIT
//! End-to-end tests of the smart HTTP client against the reference
//! `git http-backend` behind the test server in `support/`.

mod support;

use std::cell::RefCell;
use std::io::Read;
use std::path::Path;
use std::rc::Rc;

use rgit_http::{
    Credential, CredentialProvider, Error, FollowRedirects, HttpOptions, RpcBody, Service,
    SmartHttp,
};
use support::{Server, git, run};

fn opts() -> HttpOptions {
    HttpOptions::default()
}

fn pkt(data: &str) -> Vec<u8> {
    let mut v = format!("{:04x}", data.len() + 4).into_bytes();
    v.extend_from_slice(data.as_bytes());
    v
}

/// Split a pkt-line stream into payloads (`None` for flush/delim).
fn pkts(mut data: &[u8]) -> Vec<Option<Vec<u8>>> {
    let mut out = Vec::new();
    while data.len() >= 4 {
        let n = usize::from_str_radix(std::str::from_utf8(&data[..4]).unwrap(), 16).unwrap();
        if n < 4 {
            out.push(None);
            data = &data[4..];
            continue;
        }
        out.push(Some(data[4..n].to_vec()));
        data = &data[n..];
    }
    out
}

fn head_of(repo: &Path) -> String {
    let out = run(&mut git(repo, &["rev-parse", "main"]));
    String::from_utf8(out).unwrap().trim().to_string()
}

#[test]
fn discovery_v0_lists_refs() {
    let s = Server::start(false);
    let bare = s.create_repo("repo.git");
    let mut h = SmartHttp::new(&s.url("/smart/repo.git/"), opts()).unwrap();
    let adv = h.discover(Service::UploadPack).unwrap();
    assert!(adv.had_service_line);
    let text = String::from_utf8_lossy(&adv.data);
    assert!(
        text.contains(&format!("{} refs/heads/main", head_of(&bare))),
        "{text}"
    );

    let log = s.log();
    assert_eq!(log.len(), 1);
    let r = &log[0];
    assert_eq!(r.method, "GET");
    assert_eq!(
        r.target,
        "/smart/repo.git/info/refs?service=git-upload-pack"
    );
    assert_eq!(r.header("pragma"), Some("no-cache"));
    assert_eq!(r.header("accept"), Some("*/*"));
    assert!(r.header("accept-encoding").unwrap().contains("gzip"));
    assert!(
        r.header("user-agent")
            .unwrap()
            .starts_with("git/2.56.1.rgit.")
    );
    assert_eq!(r.header("git-protocol"), None);
}

#[test]
fn protocol_v2_ls_refs_and_fetch_pack() {
    let s = Server::start(false);
    let bare = s.create_repo("repo.git");
    let tip = head_of(&bare);
    let mut o = opts();
    o.extra_headers = vec!["X-Extra: 1".into()];
    o.accept_language = Some("ko-KR, *;q=0.9".into());
    let mut h = SmartHttp::new(&s.url("/smart/repo.git"), o).unwrap();
    h.set_git_protocol(Some("version=2".into()));
    let adv = h.discover(Service::UploadPack).unwrap();
    assert!(!adv.had_service_line);
    assert!(adv.data.starts_with(b"000eversion 2\n"));

    let mut req = pkt("command=ls-refs\n");
    req.extend_from_slice(b"0001");
    req.extend(pkt("symrefs\n"));
    req.extend(pkt("ref-prefix refs/heads/\n"));
    req.extend_from_slice(b"0000");
    let mut resp = h.rpc(Service::UploadPack, RpcBody::Bytes(&req)).unwrap();
    assert_eq!(
        resp.content_type(),
        Some("application/x-git-upload-pack-result")
    );
    let mut body = Vec::new();
    resp.read_to_end(&mut body).unwrap();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains(&format!("{tip} refs/heads/main")), "{text}");

    let mut req = pkt("command=fetch\n");
    req.extend_from_slice(b"0001");
    req.extend(pkt(&format!("want {tip}\n")));
    req.extend(pkt("no-progress\n"));
    req.extend(pkt("done\n"));
    req.extend_from_slice(b"0000");
    let mut resp = h.rpc(Service::UploadPack, RpcBody::Bytes(&req)).unwrap();
    let mut body = Vec::new();
    resp.read_to_end(&mut body).unwrap();
    let mut pack = Vec::new();
    let mut in_pack = false;
    for p in pkts(&body).into_iter().flatten() {
        if p == b"packfile\n" {
            in_pack = true;
        } else if in_pack && p[0] == 1 {
            pack.extend_from_slice(&p[1..]);
        }
    }
    assert!(pack.starts_with(b"PACK"));

    // The pack is complete: index it into an empty repository.
    let dst = s.dir.path().join("dst.git");
    run(git(s.dir.path(), &["init", "-q", "--bare"]).arg(&dst));
    let pack_file = s.dir.path().join("in.pack");
    std::fs::write(&pack_file, &pack).unwrap();
    let mut cmd = git(&dst, &["index-pack", "--stdin"]);
    cmd.stdin(std::fs::File::open(&pack_file).unwrap());
    run(&mut cmd);
    let t = run(&mut git(&dst, &["cat-file", "-t", &tip]));
    assert_eq!(t, b"commit\n");

    let log = s.log();
    assert_eq!(log.len(), 3);
    for r in &log {
        assert_eq!(r.header("git-protocol"), Some("version=2"));
        assert_eq!(r.header("x-extra"), Some("1"));
        assert_eq!(r.header("accept-language"), Some("ko-KR, *;q=0.9"));
    }
    assert_eq!(log[1].method, "POST");
    assert_eq!(log[1].target, "/smart/repo.git/git-upload-pack");
    assert_eq!(
        log[1].header("content-type"),
        Some("application/x-git-upload-pack-request")
    );
    assert_eq!(
        log[1].header("accept"),
        Some("application/x-git-upload-pack-result")
    );
}

#[test]
fn large_upload_pack_request_is_gzipped() {
    let s = Server::start(false);
    let bare = s.create_repo("repo.git");
    let tip = head_of(&bare);
    let mut h = SmartHttp::new(&s.url("/smart/repo.git"), opts()).unwrap();
    h.discover(Service::UploadPack).unwrap();
    let mut req = pkt(&format!("want {tip} ofs-delta\n"));
    req.extend_from_slice(b"0000");
    for i in 0..40 {
        req.extend(pkt(&format!("have {:040x}\n", i + 1)));
    }
    req.extend(pkt("done\n"));
    assert!(req.len() > 1024);
    let mut resp = h.rpc(Service::UploadPack, RpcBody::Bytes(&req)).unwrap();
    let mut body = Vec::new();
    resp.read_to_end(&mut body).unwrap();
    assert!(String::from_utf8_lossy(&body).contains("NAK"));
    let log = s.log();
    assert_eq!(log[1].header("content-encoding"), Some("gzip"));
    assert!(log[1].body_len < req.len());
}

fn new_commit_pack(s: &Server, name: &str) -> (String, String, Vec<u8>) {
    let work = s.dir.path().join(format!("{name}.work"));
    let old = head_of(&work);
    std::fs::write(work.join("file"), "more\n").unwrap();
    run(&mut git(&work, &["commit", "-qam", "two"]));
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
fn push_with_content_length() {
    let s = Server::start(false);
    let bare = s.create_repo("repo.git");
    let (old, new, pack) = new_commit_pack(&s, "repo.git");
    let mut o = opts();
    o.verbosity = 2;
    let mut h = SmartHttp::new(&s.url("/smart/repo.git"), o).unwrap();
    let lines = Rc::new(RefCell::new(Vec::new()));
    let l2 = lines.clone();
    h.set_diagnostics(move |l| l2.borrow_mut().push(l.to_string()));
    let adv = h.discover(Service::ReceivePack).unwrap();
    assert!(String::from_utf8_lossy(&adv.data).contains("report-status"));
    let req = push_request(&old, &new, &pack);
    let mut resp = h.rpc(Service::ReceivePack, RpcBody::Bytes(&req)).unwrap();
    let mut body = Vec::new();
    resp.read_to_end(&mut body).unwrap();
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("unpack ok") && text.contains("ok refs/heads/main"),
        "{text}"
    );
    assert_eq!(head_of(&bare), new);
    assert_eq!(
        *lines.borrow(),
        vec![format!("POST git-receive-pack ({} bytes)", req.len())]
    );
    let log = s.log();
    assert_eq!(log.len(), 2);
    assert!(!log[1].chunked);
}

#[test]
fn push_larger_than_post_buffer_is_chunked_after_probe() {
    let s = Server::start(false);
    let bare = s.create_repo("repo.git");
    let (old, new, pack) = new_commit_pack(&s, "repo.git");
    let mut o = opts();
    o.post_buffer = 64;
    o.verbosity = 2;
    let mut h = SmartHttp::new(&s.url("/smart/repo.git"), o).unwrap();
    let lines = Rc::new(RefCell::new(Vec::new()));
    let l2 = lines.clone();
    h.set_diagnostics(move |l| l2.borrow_mut().push(l.to_string()));
    h.discover(Service::ReceivePack).unwrap();
    let req = push_request(&old, &new, &pack);
    let mut cursor = std::io::Cursor::new(req.clone());
    let mut resp = h
        .rpc(Service::ReceivePack, RpcBody::Stream(&mut cursor))
        .unwrap();
    let mut body = Vec::new();
    resp.read_to_end(&mut body).unwrap();
    assert!(String::from_utf8_lossy(&body).contains("ok refs/heads/main"));
    assert_eq!(head_of(&bare), new);
    assert_eq!(
        *lines.borrow(),
        vec!["POST git-receive-pack (chunked)".to_string()]
    );
    let log = s.log();
    assert_eq!(log.len(), 3);
    assert_eq!(log[1].body_len, 4, "probe");
    assert!(!log[1].chunked);
    assert!(log[2].chunked);
    assert_eq!(log[2].body_len, req.len());
    assert_eq!(log[2].header("transfer-encoding"), Some("chunked"));
}

#[derive(Default)]
struct Helper {
    user: String,
    pass: String,
    filled: Vec<Credential>,
    approved: Vec<Credential>,
    rejected: Vec<Credential>,
}

impl CredentialProvider for Helper {
    fn fill(&mut self, cred: &mut Credential) -> Result<(), String> {
        self.filled.push(cred.clone());
        cred.username = Some(self.user.clone());
        cred.password = Some(self.pass.clone());
        Ok(())
    }
    fn approve(&mut self, cred: &Credential) {
        self.approved.push(cred.clone());
    }
    fn reject(&mut self, cred: &Credential) {
        self.rejected.push(cred.clone());
    }
}

#[test]
fn auth_401_fill_retry_approve() {
    let s = Server::start(false);
    s.create_repo("repo.git");
    let mut helper = Helper {
        user: "user".into(),
        pass: "pass".into(),
        ..Default::default()
    };
    {
        let mut h = SmartHttp::new(&s.url("/auth/smart/repo.git"), opts())
            .unwrap()
            .with_credentials(&mut helper);
        h.discover(Service::UploadPack).unwrap();
        let mut req = pkt("command=ls-refs\n");
        req.extend_from_slice(b"0000");
        // v0 server ignores the v2 command but answers 200; only auth matters.
        let _ = h.rpc(Service::UploadPack, RpcBody::Bytes(b"0000")).unwrap();
    }
    assert_eq!(helper.filled.len(), 1);
    let f = &helper.filled[0];
    assert_eq!(f.protocol, "http");
    assert_eq!(f.host, format!("127.0.0.1:{}", s.port));
    assert_eq!(f.path.as_deref(), Some("auth/smart/repo.git"));
    assert_eq!(helper.approved.len(), 1);
    assert!(helper.rejected.is_empty());
    let log = s.log();
    assert_eq!(log.len(), 3);
    assert_eq!(log[0].status, 401);
    assert!(log[0].header("authorization").is_none());
    assert_eq!(log[1].status, 200);
    // The POST sends the credential right away.
    assert_eq!(log[2].status, 200);
    assert!(log[2].header("authorization").is_some());
}

#[test]
fn auth_wrong_password_is_rejected() {
    let s = Server::start(false);
    s.create_repo("repo.git");
    let mut helper = Helper {
        user: "user".into(),
        pass: "wrong".into(),
        ..Default::default()
    };
    let url = s.url("/auth/smart/repo.git");
    let err = {
        let mut h = SmartHttp::new(&url, opts())
            .unwrap()
            .with_credentials(&mut helper);
        h.discover(Service::UploadPack).unwrap_err()
    };
    assert_eq!(
        err.to_string(),
        format!("Authentication failed for '{url}/'")
    );
    assert_eq!(helper.rejected.len(), 1);
    assert!(helper.approved.is_empty());
}

#[test]
fn auth_without_provider_and_with_url_credentials() {
    let s = Server::start(false);
    s.create_repo("repo.git");
    let mut h = SmartHttp::new(&s.url("/auth/smart/repo.git"), opts()).unwrap();
    let err = h.discover(Service::UploadPack).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "could not read Username for 'http://127.0.0.1:{}': terminal prompts disabled",
            s.port
        )
    );

    let url = format!("http://user:pass@127.0.0.1:{}/auth/smart/repo.git", s.port);
    let mut h = SmartHttp::new(&url, opts()).unwrap();
    let lines = Rc::new(RefCell::new(Vec::new()));
    let l2 = lines.clone();
    h.set_diagnostics(move |l| l2.borrow_mut().push(l.to_string()));
    h.discover(Service::UploadPack).unwrap();
    // Like git, the effective URL differs from the one with credentials.
    assert_eq!(
        *lines.borrow(),
        vec![format!(
            "warning: redirecting to http://127.0.0.1:{}/auth/smart/repo.git/",
            s.port
        )]
    );

    let url = format!("http://user:bad@127.0.0.1:{}/auth/smart/repo.git", s.port);
    let mut h = SmartHttp::new(&url, opts()).unwrap();
    let err = h.discover(Service::UploadPack).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "Authentication failed for 'http://127.0.0.1:{}/auth/smart/repo.git/'",
            s.port
        )
    );
}

#[test]
fn redirects_follow_initial_and_update_base() {
    let s = Server::start(false);
    s.create_repo("repo.git");
    let mut h = SmartHttp::new(&s.url("/redir/smart/repo.git"), opts()).unwrap();
    let lines = Rc::new(RefCell::new(Vec::new()));
    let l2 = lines.clone();
    h.set_diagnostics(move |l| l2.borrow_mut().push(l.to_string()));
    h.discover(Service::UploadPack).unwrap();
    assert_eq!(
        *lines.borrow(),
        vec![format!(
            "warning: redirecting to {}/",
            s.url("/smart/repo.git")
        )]
    );
    assert_eq!(h.url().to_string(), s.url("/smart/repo.git"));
    h.rpc(Service::UploadPack, RpcBody::Bytes(b"0000")).unwrap();
    let log = s.log();
    assert_eq!(log[0].status, 301);
    assert_eq!(log[2].target, "/smart/repo.git/git-upload-pack");

    let mut o = opts();
    o.follow_redirects = FollowRedirects::Never;
    let mut h = SmartHttp::new(&s.url("/redir/smart/repo.git"), o).unwrap();
    let err = h.discover(Service::UploadPack).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "unable to access '{}/': The requested URL returned error: 301",
            s.url("/redir/smart/repo.git")
        )
    );

    // A redirect target without the requested suffix.
    s.create_repo("other.git");
    let mut h = SmartHttp::new(&s.url("/redir-elsewhere/x.git"), opts()).unwrap();
    match h.discover(Service::UploadPack).unwrap_err() {
        Error::NotValid { .. } | Error::RedirectBase { .. } => {}
        e => panic!("unexpected {e:?}"),
    }
}

#[test]
fn error_messages() {
    let s = Server::start(false);
    let base = s.url("");
    let msg = |path: &str| {
        let mut h = SmartHttp::new(&format!("{base}{path}"), opts()).unwrap();
        h.discover(Service::UploadPack).unwrap_err()
    };
    assert_eq!(
        msg("/smart/nope.git").to_string(),
        format!("repository '{base}/smart/nope.git/' not found")
    );
    let e = msg("/status/403/x.git");
    assert_eq!(
        e.to_string(),
        format!(
            "unable to access '{base}/status/403/x.git/': The requested URL returned error: 403"
        )
    );
    assert_eq!(e.remote_lines(), ["nope".to_string()]);
    assert_eq!(e.status(), Some(403));
    assert_eq!(
        msg("/broken/x.git").to_string(),
        format!("{base}/broken/x.git/info/refs not valid: is this a git repository?")
    );
    assert!(matches!(msg("/dumb/x.git"), Error::DumbNotSupported { .. }));

    let mut h = SmartHttp::new("http://127.0.0.1:1/x.git", opts()).unwrap();
    let e = h.discover(Service::UploadPack).unwrap_err().to_string();
    assert!(
        e.starts_with(
            "unable to access 'http://127.0.0.1:1/x.git/': Failed to connect to 127.0.0.1:1 after "
        ),
        "{e}"
    );
    assert!(e.ends_with(" ms: Could not connect to server"), "{e}");

    // RPC errors.
    let s2 = Server::start(false);
    s2.create_repo("repo.git");
    let mut h = SmartHttp::new(&s2.url("/smart/repo.git"), opts()).unwrap();
    h.discover(Service::UploadPack).unwrap();
    let mut h = SmartHttp::new(&s2.url("/status/500/repo.git"), opts()).unwrap();
    let e = h
        .rpc(Service::UploadPack, RpcBody::Bytes(b"0000"))
        .err()
        .unwrap();
    assert_eq!(
        e.to_string(),
        "RPC failed; HTTP 500 curl 22 The requested URL returned error: 500"
    );
}

#[test]
fn tls_with_ca_file_and_without_verification() {
    let s = Server::start(true);
    let bare = s.create_repo("repo.git");
    let url = s.url("/smart/repo.git");

    let mut h = SmartHttp::new(&url, opts()).unwrap();
    let e = h.discover(Service::UploadPack).unwrap_err().to_string();
    assert!(e.contains("SSL certificate problem"), "{e}");

    let mut o = opts();
    o.ssl_ca_info = Some(s.cert_file());
    let mut h = SmartHttp::new(&url, o).unwrap();
    let adv = h.discover(Service::UploadPack).unwrap();
    assert!(String::from_utf8_lossy(&adv.data).contains(&head_of(&bare)));
    h.rpc(Service::UploadPack, RpcBody::Bytes(b"0000")).unwrap();

    let capath = s.dir.path().join("capath");
    std::fs::create_dir(&capath).unwrap();
    std::fs::copy(s.cert_file(), capath.join("x.pem")).unwrap();
    let mut o = opts();
    o.ssl_ca_path = Some(capath);
    SmartHttp::new(&url, o)
        .unwrap()
        .discover(Service::UploadPack)
        .unwrap();

    let mut o = opts();
    o.ssl_verify = false;
    let mut h = SmartHttp::new(&url, o).unwrap();
    h.discover(Service::UploadPack).unwrap();

    // The certificate is for 127.0.0.1 and localhost only.
    let mut o = opts();
    o.ssl_ca_info = Some(s.cert_file());
    let wrong = url.replace("127.0.0.1", "[::1]");
    let e = SmartHttp::new(&wrong, o)
        .unwrap()
        .discover(Service::UploadPack);
    assert!(e.is_err());
}

#[test]
fn http_and_https_through_proxy() {
    let s = Server::start(false);
    s.create_repo("repo.git");
    let p = support::Proxy::start();
    let mut o = opts();
    o.proxy = Some(format!("http://pu:pp@127.0.0.1:{}", p.port));
    let mut h = SmartHttp::new(&s.url("/smart/repo.git"), o).unwrap();
    h.discover(Service::UploadPack).unwrap();
    h.rpc(Service::UploadPack, RpcBody::Bytes(b"0000")).unwrap();
    let plog = p.log();
    assert_eq!(plog[0].method, "GET");
    assert_eq!(
        plog[0].target,
        s.url("/smart/repo.git/info/refs?service=git-upload-pack")
    );
    assert_eq!(
        plog[0].header("proxy-authorization"),
        Some("Basic cHU6cHA=")
    );
    assert_eq!(s.log().len(), 2);

    let t = Server::start(true);
    t.create_repo("repo.git");
    let mut o = opts();
    o.proxy = Some(format!("127.0.0.1:{}", p.port));
    o.ssl_ca_info = Some(t.cert_file());
    let mut h = SmartHttp::new(&t.url("/smart/repo.git"), o).unwrap();
    h.discover(Service::UploadPack).unwrap();
    let plog = p.log();
    let last = plog.last().unwrap();
    assert_eq!(last.method, "CONNECT");
    assert_eq!(last.target, format!("127.0.0.1:{}", t.port));
}

#[test]
fn low_speed_limit_aborts() {
    let s = Server::start(false);
    let mut o = opts();
    o.low_speed_limit = 1000;
    o.low_speed_time = 1;
    let mut h = SmartHttp::new(&s.url("/slow/repo.git"), o).unwrap();
    let e = h.discover(Service::UploadPack).unwrap_err().to_string();
    assert!(
        e.ends_with("Operation too slow. Less than 1000 bytes/sec transferred the last 1 seconds"),
        "{e}"
    );
}

#[test]
fn user_agent_and_smart_off() {
    let s = Server::start(false);
    s.create_repo("repo.git");
    let mut o = opts();
    o.user_agent = "ua/1".into();
    let mut h = SmartHttp::new(&s.url("/smart/repo.git"), o).unwrap();
    h.discover(Service::UploadPack).unwrap();
    assert_eq!(s.log()[0].header("user-agent"), Some("ua/1"));

    let mut o = opts();
    o.smart = false;
    let mut h = SmartHttp::new(&s.url("/smart/repo.git"), o).unwrap();
    let e = h.discover(Service::UploadPack).unwrap_err();
    assert!(matches!(e, Error::DumbNotSupported { .. }), "{e:?}");
    assert_eq!(s.log()[1].target, "/smart/repo.git/info/refs");
}
