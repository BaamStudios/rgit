// SPDX-License-Identifier: MIT
//! `GIT_TRACE_CURL` for HTTP/2 (its own test binary: the trace keys are
//! process-wide). The format follows git 2.56 with libcurl 8: the request
//! in HTTP/1 form with `HTTP/2` as version, `HTTP/2 <status>` and the
//! lower-case response fields.

#[path = "support/h2server.rs"]
mod h2server;
mod support;

use std::ffi::OsStr;

use rgit_http::{HttpOptions, RpcBody, Service, SmartHttp};

fn opts() -> HttpOptions {
    HttpOptions::default()
}

#[test]
fn h2_trace_lines() {
    let s = h2server::start_h2();
    s.create_repo("repo.git");
    let trace = s.dir.path().join("trace");
    rgit_trace::TRACE_BARE.init_from(Some(OsStr::new("1")));
    rgit_trace::TRACE_CURL.init_from(Some(trace.as_os_str()));

    let mut o = opts();
    o.ssl_ca_info = Some(s.cert_file());
    let mut h = SmartHttp::new(&s.url("/smart/repo.git"), o).unwrap();
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
    assert!(
        lines.contains(&"== Info: ALPN: server accepted h2"),
        "{text}"
    );
    let get: Vec<&str> = lines
        .iter()
        .copied()
        .skip_while(|l| !l.starts_with("=> Send header, "))
        .take(8)
        .collect();
    let ua = format!(
        "=> Send header: User-Agent: git/{}",
        rgit_protocol::agent::version()
    );
    assert_eq!(
        get[1..],
        [
            "=> Send header: GET /smart/repo.git/info/refs?service=git-upload-pack HTTP/2",
            &format!("=> Send header: Host: 127.0.0.1:{port}"),
            &ua,
            "=> Send header: Accept: */*",
            "=> Send header: Accept-Encoding: deflate, gzip",
            "=> Send header: Pragma: no-cache",
            "=> Send header: Git-Protocol: version=2",
        ]
    );
    let i = lines
        .iter()
        .position(|l| *l == "<= Recv header: HTTP/2 200")
        .expect(&text);
    // libcurl counts `HTTP/2 200 \r\n`.
    assert_eq!(
        lines[i - 1],
        "<= Recv header, 0000000013 bytes (0x0000000d)"
    );
    assert!(
        lines
            .contains(&"<= Recv header: content-type: application/x-git-upload-pack-advertisement"),
        "{text}"
    );
    assert!(lines.contains(&"=> Send header: POST /smart/repo.git/git-upload-pack HTTP/2"));
    assert!(lines.contains(&"=> Send header: Content-Length: 24"));
    assert!(lines.contains(&"== Info: upload completely sent off: 24 bytes"));
    assert!(lines.contains(&"== Info: Reusing existing https: connection with host 127.0.0.1"));
    assert!(!text.contains("Transfer-Encoding"));
}
