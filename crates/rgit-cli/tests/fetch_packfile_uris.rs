// SPDX-License-Identifier: MIT
//! Packfile URIs (`fetch.uriProtocols`, the `packfile-uris` section of a
//! protocol v2 `fetch`) against the reference git as the server, with
//! `uploadpack.blobPackfileUri` pointing at packs served over plain HTTP.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

use rgit_difftest::{Check, Fixture, Normalizer};

mod common;
use common::servers::{isolated, server_path, start_http};

const CHECKS: [Check; 4] = [Check::ExitCode, Check::Stderr, Check::Refs, Check::Objects];

/// Serve the files of `dir` over HTTP/1.0 (404 for anything else);
/// returns `http://127.0.0.1:<port>`.
fn serve_files(dir: PathBuf) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let mut line = String::new();
            let mut r = BufReader::new(s.try_clone().unwrap());
            if r.read_line(&mut line).is_err() {
                continue;
            }
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                    break;
                }
            }
            let name = line.split_whitespace().nth(1).unwrap_or("/");
            let file = dir.join(name.trim_start_matches('/'));
            let _ = match std::fs::read(&file) {
                Ok(data) if file.is_file() => write!(
                    s,
                    "HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n",
                    data.len()
                )
                .and_then(|_| s.write_all(&data)),
                _ => write!(s, "HTTP/1.0 404 Not Found\r\nContent-Length: 0\r\n\r\n"),
            };
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// Exclude `file` of `srv` from fetch responses: its blob goes into a pack
/// of its own under `www/`, announced as `<base>/<name>` (`<base>/mypack-
/// <hash>.pack` by default); with `wrong_hash` the server announces the
/// blob's id as the pack's hash.
fn exclude(fx: &Fixture, file: &str, base: &str, name: Option<&str>, wrong_hash: bool) {
    let blob = fx.git_stdout(["-C", "srv", "hash-object", file]);
    let blob = String::from_utf8(blob).unwrap().trim().to_owned();
    let mut cmd = Command::new(fx.reference_git());
    isolated(fx, &mut cmd);
    let out = cmd
        .current_dir(fx.work_dir().join("srv"))
        .args(["pack-objects", "../www/mypack"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            c.stdin
                .take()
                .unwrap()
                .write_all(format!("{blob}\n").as_bytes())?;
            c.wait_with_output()
        })
        .unwrap();
    let pack = String::from_utf8(out.stdout).unwrap().trim().to_owned();
    let name = name.map_or_else(|| format!("mypack-{pack}.pack"), str::to_owned);
    let hash = if wrong_hash { &blob } else { &pack };
    fx.git_output([
        "-C",
        "srv",
        "config",
        "--add",
        "uploadpack.blobPackfileUri",
        &format!("{blob} {hash} {base}/{name}"),
    ]);
}

/// `srv` with `my-blob` and `other-blob`, both served as packfile URIs
/// (the second as `second`: `ok`, `bad-hash` or `missing`).
fn fixture(second: &str) -> (Fixture, String) {
    let fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"));
    if fx.is_skipped() {
        return (fx, String::new());
    }
    let fx = fx
        .git(["init", "-q", "srv"])
        .git(["-C", "srv", "config", "uploadpack.allowSidebandAll", "true"])
        .write("srv/my-blob", "my-blob\n")
        .write("srv/other-blob", "other-blob\n")
        .git(["-C", "srv", "add", "."])
        .git(["-C", "srv", "commit", "-q", "-m", "x"])
        .git(["init", "-q", "dst"]);
    std::fs::create_dir_all(fx.work_dir().join("www")).unwrap();
    let base = serve_files(fx.work_dir().join("www"));
    exclude(&fx, "my-blob", &base, None, false);
    match second {
        "bad-hash" => exclude(&fx, "other-blob", &base, None, true),
        "missing" => exclude(&fx, "other-blob", &base, Some("nothere.pack"), false),
        _ => exclude(&fx, "other-blob", &base, None, false),
    }
    (fx, base)
}

fn uri_args<'a>(extra: &[&'a str]) -> Vec<&'a str> {
    let mut a = vec![
        "-c",
        "protocol.version=2",
        "-c",
        "fetch.uriProtocols=http,https",
    ];
    a.extend_from_slice(extra);
    a
}

/// The pack files in `dir` (`.pack`, `.idx`, `.keep` counts).
fn pack_files(dir: &Path) -> (usize, usize, usize) {
    let mut counts = (0, 0, 0);
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if n.ends_with(".pack") {
            counts.0 += 1;
        } else if n.ends_with(".idx") {
            counts.1 += 1;
        } else if n.ends_with(".keep") {
            counts.2 += 1;
        }
    }
    counts
}

#[test]
fn clone_downloads_packfile_uris() {
    let (mut fx, base) = fixture("ok");
    if fx.is_skipped() {
        return;
    }
    let path = server_path(&fx);
    fx.compare(uri_args(&["clone", "-q", "--no-local", "srv", "c1"]))
        .env("PATH", &path)
        .checks(CHECKS)
        .run();
    // The inline pack and one pack per URI, no `.keep` left.
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rgit"));
    isolated(&fx, &mut cmd);
    let st = cmd
        .env("PATH", &path)
        .current_dir(fx.work_dir())
        .args(uri_args(&["clone", "-q", "--no-local", "srv", "r1"]))
        .status()
        .unwrap();
    assert!(st.success());
    assert_eq!(
        pack_files(&fx.work_dir().join("r1/.git/objects/pack")),
        (3, 3, 0)
    );
    // Without fetch.uriProtocols the server sends one pack.
    fx.compare(["clone", "-q", "--no-local", "srv", "c2"])
        .env("PATH", &path)
        .checks(CHECKS)
        .run();
    // Over smart HTTP; the URIs' paths are redacted in the packet trace
    // (t5702 "packfile-uri path redacted in trace").
    let url = format!("{}/srv", start_http(&fx));
    fx.compare(uri_args(&["clone", "-q", &url, "c3"]))
        .env("PATH", &path)
        .checks(CHECKS)
        .run();
    let trace = fx.work_dir().join("trace");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rgit"));
    isolated(&fx, &mut cmd);
    let st = cmd
        .env("PATH", &path)
        .env("GIT_TRACE_PACKET", &trace)
        .current_dir(fx.work_dir())
        .args(uri_args(&["clone", "-q", &url, "r3"]))
        .status()
        .unwrap();
    assert!(st.success());
    assert_eq!(
        pack_files(&fx.work_dir().join("r3/.git/objects/pack")),
        (3, 3, 0)
    );
    let trace = std::fs::read_to_string(trace).unwrap();
    let redacted = format!(" {base}/<redacted>");
    assert_eq!(trace.matches(&redacted).count(), 2, "{trace}");
    assert!(trace.contains("clone< \\1packfile-uris"), "{trace}");
}

#[test]
fn fetch_downloads_packfile_uris() {
    let (mut fx, _) = fixture("ok");
    if fx.is_skipped() {
        return;
    }
    let path = server_path(&fx);
    fx.compare(uri_args(&["fetch", "-q", "../srv"]))
        .in_dir("dst")
        .repo("dst")
        .env("PATH", &path)
        .checks(CHECKS)
        .run();
}

#[test]
fn packfile_uri_failures() {
    for second in ["bad-hash", "missing"] {
        let (mut fx, base) = fixture(second);
        if fx.is_skipped() {
            return;
        }
        let path = server_path(&fx);
        let port = Normalizer::replace(base.clone(), "http://127.0.0.1:PORT");
        fx.compare(uri_args(&["clone", "-q", "--no-local", "srv", "c1"]))
            .env("PATH", &path)
            .normalize_stderr(port.clone())
            .checks([Check::ExitCode, Check::Stderr])
            .run();
        fx.compare(uri_args(&["clone", "-q", "--no-local", "srv", "c2"]))
            .env("PATH", &path)
            .env("GIT_TRACE_REDACT", "0")
            .normalize_stderr(port)
            .checks([Check::ExitCode, Check::Stderr])
            .run();
    }
}

#[test]
fn packfile_uri_with_fsck_and_gitmodules() {
    for (name, ok) in [("libfoo", true), ("\"..\"", false)] {
        let fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"));
        if fx.is_skipped() {
            return;
        }
        let gitmodules = format!(
            "[submodule {name}]\npath = include/foo\nurl = git://example.com/git/lib.git\n"
        );
        let fx = fx
            .git(["init", "-q", "srv"])
            .git(["-C", "srv", "config", "uploadpack.allowSidebandAll", "true"])
            .write("srv/.gitmodules", gitmodules)
            .git(["-C", "srv", "add", ".gitmodules"])
            .git(["-C", "srv", "commit", "-q", "-m", "x"])
            .git(["init", "-q", "dst"]);
        std::fs::create_dir_all(fx.work_dir().join("www")).unwrap();
        let base = serve_files(fx.work_dir().join("www"));
        exclude(&fx, ".gitmodules", &base, None, false);
        let mut fx = fx;
        let path = server_path(&fx);
        let fsck = ["-c", "transfer.fsckObjects=true"];
        let out = fx
            .compare(uri_args(&[
                fsck[0],
                fsck[1],
                "clone",
                "-q",
                "--no-local",
                "srv",
                "c1",
            ]))
            .env("PATH", &path)
            .checks([Check::ExitCode, Check::Stderr])
            .run();
        if let Some(out) = out {
            assert_eq!(out.git.code == Some(0), ok, "{name}");
        }
        // git's fetch does not check a `.gitmodules` blob that arrives in
        // another pack than its tree.
        fx.compare(uri_args(&[fsck[0], fsck[1], "fetch", "-q", "../srv"]))
            .in_dir("dst")
            .repo("dst")
            .env("PATH", &path)
            .checks([Check::ExitCode, Check::Stderr])
            .run();
    }
}
