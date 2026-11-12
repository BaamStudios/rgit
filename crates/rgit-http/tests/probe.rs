// SPDX-License-Identifier: MIT
//! Manual helper: `RGIT_HTTP_PROBE=1 cargo test -p rgit-http --test probe -- --nocapture`
//! starts the test server and keeps it running, to compare with `git`.
mod support;

#[test]
fn probe() {
    if std::env::var_os("RGIT_HTTP_PROBE").is_none() {
        return;
    }
    let tls = std::env::var_os("RGIT_HTTP_PROBE_TLS").is_some();
    let s = support::Server::start(tls);
    s.create_repo("repo.git");
    let p = support::Proxy::start();
    println!(
        "URL {} proxy {} cert {}",
        s.url("/smart/repo.git"),
        p.port,
        s.cert_file().display()
    );
    std::thread::sleep(std::time::Duration::from_secs(900));
}
