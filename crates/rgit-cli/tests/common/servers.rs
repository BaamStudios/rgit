// SPDX-License-Identifier: MIT
//! Servers for differential transport tests: `git daemon` and smart HTTP
//! (`git http-backend` behind a small CGI server), both from the reference
//! git and isolated from the user's configuration.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use rgit_difftest::Fixture;

/// `PATH` with the reference git's directory first: a local `upload-pack`
/// runs `git pack-objects`, which must be the reference git on both sides
/// (the fixture puts the binary under test first).
pub fn server_path(fx: &Fixture) -> std::ffi::OsString {
    let mut dirs: Vec<PathBuf> = fx
        .reference_git()
        .parent()
        .into_iter()
        .map(Path::to_path_buf)
        .collect();
    if let Some(p) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    std::env::join_paths(dirs).unwrap()
}

/// A child process killed on drop.
pub struct Server(pub Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Clear the environment except `PATH`; `HOME`, `XDG_CONFIG_HOME` and
/// `GIT_CONFIG_NOSYSTEM` point into the fixture.
pub fn isolated(fx: &Fixture, cmd: &mut Command) {
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", fx.home_dir())
        .env("XDG_CONFIG_HOME", fx.home_dir().join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1");
}

pub fn wait_for_port(port: u16) -> bool {
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

/// Start `git daemon` exporting the fixture's work directory; returns the
/// server and its `git://127.0.0.1:<port>` base URL, or `None` if it did
/// not start.
pub fn start_daemon(fx: &Fixture) -> Option<(Server, String)> {
    let port = free_port();
    let mut cmd = Command::new(fx.reference_git());
    isolated(fx, &mut cmd);
    cmd.args([
        "daemon",
        "--export-all",
        "--reuseaddr",
        "--listen=127.0.0.1",
    ])
    .arg(format!("--port={port}"))
    .arg(format!("--base-path={}", fx.work_dir().display()))
    .arg(fx.work_dir())
    .stderr(Stdio::null());
    let daemon = Server(cmd.spawn().unwrap());
    if !wait_for_port(port) {
        eprintln!("skipping: git daemon did not start");
        return None;
    }
    Some((daemon, format!("git://127.0.0.1:{port}")))
}

/// Serve the fixture's work directory over smart HTTP (`git
/// http-backend`) in background threads; returns the
/// `http://127.0.0.1:<port>` base URL.
pub fn start_http(fx: &Fixture) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let git = fx.reference_git().to_path_buf();
    let root = fx.work_dir();
    let home = fx.home_dir();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            let (g, r, h) = (git.clone(), root.clone(), home.clone());
            std::thread::spawn(move || serve_cgi(&g, &r, &h, s));
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// Answer one HTTP/1.0 request by running `git http-backend` as CGI.
fn serve_cgi(git: &Path, root: &Path, fx_home: &Path, mut stream: TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let header = |k: &str| {
        headers
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let mut body = Vec::new();
    if let Ok(n) = header("content-length").parse::<usize>() {
        body.resize(n, 0);
        reader.read_exact(&mut body).unwrap();
    } else if header("transfer-encoding").eq_ignore_ascii_case("chunked") {
        loop {
            let mut size = String::new();
            reader.read_line(&mut size).unwrap();
            let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
            let mut chunk = vec![0; n + 2];
            reader.read_exact(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
        }
    }
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let mut cmd = Command::new(git);
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", fx_home)
        .env("XDG_CONFIG_HOME", fx_home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_PROJECT_ROOT", root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("REQUEST_METHOD", &method)
        .env("PATH_INFO", path)
        .env("QUERY_STRING", query)
        .env("CONTENT_TYPE", header("content-type"))
        .env("CONTENT_LENGTH", body.len().to_string())
        .env("HTTP_CONTENT_ENCODING", header("content-encoding"))
        .env("GIT_PROTOCOL", header("git-protocol"))
        .env("REMOTE_ADDR", "127.0.0.1")
        .arg("http-backend")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&body);
    });
    let out = child.wait_with_output().unwrap();
    let _ = writer.join();
    let split = out
        .stdout
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(out.stdout.len());
    let head = String::from_utf8_lossy(&out.stdout[..split]).into_owned();
    let rest = out.stdout.get(split + 4..).unwrap_or(&[]);
    let mut status = "200 OK".to_string();
    let mut resp = String::new();
    for h in head.split("\r\n") {
        match h.split_once(':') {
            Some((k, v)) if k.eq_ignore_ascii_case("status") => status = v.trim().to_string(),
            Some(_) => {
                resp.push_str(h);
                resp.push_str("\r\n");
            }
            None => {}
        }
    }
    let _ = write!(
        stream,
        "HTTP/1.0 {status}\r\n{resp}Content-Length: {}\r\nConnection: close\r\n\r\n",
        rest.len()
    );
    let _ = stream.write_all(rest);
}
