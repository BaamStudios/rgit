// SPDX-License-Identifier: MIT
//! `fetch` against the reference git: summary on stderr, exit code, refs,
//! reflogs, `FETCH_HEAD` and objects, over a local path, `file://`,
//! `git daemon` and smart HTTP (`git http-backend` behind a small CGI
//! server in this file). The packet trace check compares the number of
//! `have` lines.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use rgit_difftest::{Check, Fixture};

const CHECKS: [Check; 7] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Refs,
    Check::Reflogs,
    Check::GitFiles,
    Check::Objects,
];

fn commit(fx: Fixture, dir: &str, file: &str, msg: &str) -> Fixture {
    fx.write(format!("{dir}/{file}"), format!("{msg}\n"))
        .git(["-C", dir, "add", file])
        .git(["-C", dir, "commit", "-q", "-m", msg])
        .tick()
}

/// `srv` with branches and tags, `empty` (a repository with `origin`),
/// `cl` (a clone of `srv`), then new history in `srv`: a new commit and
/// tags, a deleted and a new branch, a rewritten branch.
fn fixture() -> Fixture {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "srv"]);
    let fx = commit(fx, "srv", "a", "one")
        .git(["-C", "srv", "tag", "v1"])
        .git(["-C", "srv", "tag", "-a", "-m", "ann", "v1a"])
        .git([
            "-C",
            "srv",
            "checkout",
            "-q",
            "-b",
            "a-very-long-branch-name-here",
        ]);
    let fx = commit(fx, "srv", "b", "two")
        .git(["-C", "srv", "checkout", "-q", "main"])
        .git(["-C", "srv", "branch", "side"])
        .git(["init", "-q", "empty"])
        .git(["-C", "empty", "remote", "add", "origin", "../srv"])
        .git(["clone", "-q", "srv", "cl"]);
    let fx = commit(fx, "srv", "c", "three")
        .git(["-C", "srv", "tag", "v2"])
        .git([
            "-C",
            "srv",
            "branch",
            "-q",
            "-D",
            "a-very-long-branch-name-here",
        ])
        .git(["-C", "srv", "branch", "newb"])
        .git(["-C", "srv", "checkout", "-q", "side"]);
    commit(fx, "srv", "s", "side")
        .git(["-C", "srv", "commit", "-q", "--amend", "-m", "side-amended"])
        .git(["-C", "srv", "tag", "-a", "-m", "x", "v3"])
        .git(["-C", "srv", "checkout", "-q", "main"])
}

/// `PATH` with the reference git's directory first: the local
/// `upload-pack` runs `git pack-objects`, which must be the reference git
/// on both sides (the fixture puts the binary under test first).
fn server_path(fx: &Fixture) -> std::ffi::OsString {
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

fn compare(fx: &mut Fixture, repo: &str, args: &[&str]) {
    let path = server_path(fx);
    fx.compare(args)
        .in_dir(repo)
        .repo(repo)
        .env("PATH", path)
        .checks(CHECKS)
        .run();
}

#[test]
fn into_empty_repository() {
    let cases: &[&[&str]] = &[
        &["fetch"],
        &["fetch", "origin"],
        &["fetch", "../srv", "main"],
        &["fetch", "origin", "main:refs/heads/x", "tag", "v1"],
        &["fetch", "--dry-run", "origin"],
        &["fetch", "-q"],
        &["fetch", "--no-tags"],
        &["fetch", "--tags"],
        &["fetch", "-v", "origin", "HEAD"],
    ];
    for args in cases {
        let mut fx = fixture();
        if fx.is_skipped() {
            return;
        }
        compare(&mut fx, "empty", args);
    }
}

#[test]
fn updates() {
    let cases: &[&[&str]] = &[
        &["fetch"],
        &["fetch", "--prune"],
        &["fetch", "--prune", "--dry-run"],
        &["fetch", "-v"],
        &[
            "fetch",
            "-f",
            "origin",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
        &[
            "fetch",
            "origin",
            "main:refs/remotes/origin/main",
            "side:refs/remotes/origin/side",
        ],
        &["fetch", "-q", "origin", "side:refs/remotes/origin/side"],
        &["fetch", "origin", "side"],
        &["fetch", "origin", "main:main"],
        &["fetch", "origin", "nope"],
        &["fetch", "nosuchremote"],
        &["fetch", "origin", "refs/heads/*:refs/x/*"],
        &["fetch", ".", "refs/remotes/origin/main"],
        &["fetch", "--tags"],
    ];
    for args in cases {
        let mut fx = fixture();
        if fx.is_skipped() {
            return;
        }
        compare(&mut fx, "cl", args);
    }
}

#[test]
fn configuration() {
    let mut fx = fixture()
        .git(["-C", "cl", "checkout", "-q", "-b", "work", "origin/main"])
        .git(["-C", "cl", "config", "branch.work.remote", "origin"])
        .git(["-C", "cl", "config", "branch.work.merge", "refs/heads/main"]);
    if fx.is_skipped() {
        return;
    }
    compare(&mut fx, "cl", &["fetch"]);

    let mut fx = fixture().git(["-C", "cl", "config", "--unset", "remote.origin.fetch"]);
    compare(&mut fx, "cl", &["fetch"]);

    let mut fx = fixture().git(["-C", "srv", "tag", "-f", "v1", "main"]);
    compare(&mut fx, "cl", &["fetch", "--tags"]);
    let mut fx = fixture().git(["-C", "srv", "tag", "-f", "v1", "main"]);
    compare(&mut fx, "cl", &["fetch", "--tags", "--force"]);
}

#[test]
fn column_width() {
    let mut fx = fixture()
        .git(["-C", "srv", "branch", "bbbbbbbbbbbbbbbbbbbbbbb"])
        .git(["-C", "srv", "branch", "bbbbbbbbbbbbbbbbbbbbbbbb"]);
    if fx.is_skipped() {
        return;
    }
    compare(&mut fx, "cl", &["fetch", "-v"]);
    let mut fx = fixture().git(["-C", "srv", "branch", "bbbbbbbbbbbbbbbbbbbbbbb"]);
    let path = server_path(&fx);
    fx.compare(["fetch"])
        .in_dir("cl")
        .repo("cl")
        .env("PATH", path)
        .env("COLUMNS", "200")
        .checks(CHECKS)
        .run();
}

/// The packet trace of one fetch, as lines.
fn trace(fx: &Fixture, bin: &Path, dir: &Path, out: &Path) -> String {
    let status = Command::new(bin)
        .arg("fetch")
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", fx.home_dir())
        .env("XDG_CONFIG_HOME", fx.home_dir().join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TRACE_PACKET", out)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::read_to_string(out).unwrap()
}

#[test]
fn have_lines_match() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "big"]);
    if fx.is_skipped() {
        return;
    }
    for i in 0..40 {
        fx = commit(fx, "big", "f", &format!("c{i}"));
    }
    fx = fx.git(["clone", "-q", "big", "local"]);
    for i in 0..30 {
        fx = commit(fx, "local", "l", &format!("l{i}"));
    }
    for i in 40..50 {
        fx = commit(fx, "big", "f", &format!("c{i}"));
    }
    let work = fx.work_dir();
    let copy = |name: &str| {
        let dst = work.join(name);
        let st = Command::new("cp")
            .args(["-a"])
            .arg(work.join("local"))
            .arg(&dst)
            .status()
            .unwrap();
        assert!(st.success());
        dst
    };
    let (g, r) = (copy("g"), copy("r"));
    let count = |t: &str| t.lines().filter(|l| l.contains("fetch> have ")).count();
    let git_haves = count(&trace(&fx, fx.reference_git(), &g, &work.join("g.trace")));
    let rgit = PathBuf::from(env!("CARGO_BIN_EXE_rgit"));
    let rgit_haves = count(&trace(&fx, &rgit, &r, &work.join("r.trace")));
    assert!(git_haves > 0);
    assert_eq!(git_haves, rgit_haves);
}

/// A child process killed on drop.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn isolated(fx: &Fixture, cmd: &mut Command) {
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", fx.home_dir())
        .env("XDG_CONFIG_HOME", fx.home_dir().join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1");
}

fn wait_for_port(port: u16) -> bool {
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

#[test]
fn git_daemon() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let port = free_port();
    let mut cmd = Command::new(fx.reference_git());
    isolated(&fx, &mut cmd);
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
    let _daemon = Server(cmd.spawn().unwrap());
    if !wait_for_port(port) {
        eprintln!("skipping: git daemon did not start");
        return;
    }
    let url = format!("git://127.0.0.1:{port}/srv");
    let mut fx = fx.git(["-C", "empty", "remote", "set-url", "origin", &url]);
    compare(&mut fx, "empty", &["fetch"]);
    compare(
        &mut fx,
        "cl",
        &["fetch", &url, "+refs/heads/*:refs/remotes/origin/*"],
    );
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

#[test]
fn smart_http() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
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
    let url = format!("http://127.0.0.1:{port}/srv");
    let mut fx = fx.git(["-C", "empty", "remote", "set-url", "origin", &url]);
    compare(&mut fx, "empty", &["fetch"]);
    compare(
        &mut fx,
        "cl",
        &[
            "fetch",
            "--prune",
            &url,
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
}
