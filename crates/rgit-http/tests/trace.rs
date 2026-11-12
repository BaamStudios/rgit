// SPDX-License-Identifier: MIT
//! `GIT_TRACE_CURL` output (its own test binary: the trace keys are
//! process-wide).

mod support;

use std::ffi::OsStr;

use rgit_http::{HttpOptions, RpcBody, Service, SmartHttp};
use support::Server;

#[test]
fn curl_trace_format_and_redaction() {
    let s = Server::start(false);
    s.create_repo("repo.git");
    let trace = s.dir.path().join("trace");
    rgit_trace::TRACE_BARE.init_from(Some(OsStr::new("1")));
    rgit_trace::TRACE_CURL.init_from(Some(trace.as_os_str()));

    let url = format!("http://user:pass@127.0.0.1:{}/auth/smart/repo.git", s.port);
    let mut h = SmartHttp::new(&url, HttpOptions::default()).unwrap();
    h.set_diagnostics(|_| {});
    h.set_git_protocol(Some("version=2".into()));
    h.discover(Service::UploadPack).unwrap();
    h.rpc(
        Service::UploadPack,
        RpcBody::Bytes(b"0014command=ls-refs\n0000"),
    )
    .unwrap();
    drop(h);

    let text = std::fs::read_to_string(&trace).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let port = s.port;
    let first_get: Vec<&str> = lines
        .iter()
        .copied()
        .skip_while(|l| !l.starts_with("=> Send header, "))
        .take(9)
        .collect();
    let ua = format!(
        "=> Send header: User-Agent: git/{}",
        rgit_protocol::agent::version()
    );
    assert_eq!(
        first_get,
        vec![
            first_get[0],
            "=> Send header: GET /auth/smart/repo.git/info/refs?service=git-upload-pack HTTP/1.1",
            &format!("=> Send header: Host: 127.0.0.1:{port}"),
            &ua,
            "=> Send header: Accept: */*",
            "=> Send header: Accept-Encoding: deflate, gzip",
            "=> Send header: Pragma: no-cache",
            "=> Send header: Git-Protocol: version=2",
            "=> Send header:",
        ]
    );
    let n: usize = first_get[0][16..26].parse().unwrap();
    assert_eq!(
        first_get[0],
        format!("=> Send header, {n:010} bytes (0x{n:08x})")
    );

    assert!(lines.contains(&"<= Recv header: HTTP/1.1 401 Unauthorized"));
    assert!(lines.contains(&"<= Recv header, 0000000027 bytes (0x0000001b)"));
    assert!(lines.contains(&"=> Send header: Authorization: Basic <redacted>"));
    assert!(!text.contains("dXNlcjpwYXNz"));
    assert!(lines.contains(&"=> Send header: POST /auth/smart/repo.git/git-upload-pack HTTP/1.1"));
    assert!(lines.contains(&"=> Send header: Content-Length: 24"));
    assert!(lines.contains(&"=> Send data, 0000000024 bytes (0x00000018)"));
    assert!(lines.contains(&"=> Send data: 0014command=ls-refs.0000"));
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("<= Recv data: 000eversion 2."))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("== Info: Reusing existing http: connection"))
    );
}
