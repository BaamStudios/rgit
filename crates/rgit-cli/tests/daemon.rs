// SPDX-License-Identifier: MIT
//! `rgit daemon` against the reference `git daemon`.
//!
//! - Command line: the same invalid invocations must fail alike.
//! - `--inetd`: the same first packets are sent to both daemons on
//!   standard input; the answer, the log (`--log-destination=stderr`,
//!   process ids made equal) and the exit code must be equal.
//! - Listening: `rgit daemon` on a free port of 127.0.0.1 serves the
//!   reference client (clone, fetch, ls-remote with protocol v0, v1 and
//!   v2); limits, time-outs, the pid file and `--detach` are checked on
//!   the running daemon.
//!
//! No test binds another address than 127.0.0.1 or a privileged port, and
//! every daemon is ended when its test ends, also a failing one.

#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use rgit_difftest::Fixture;

mod common;
use common::servers::{Server, daemon_command, free_port, isolated, server_path};

fn rgit() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rgit"))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `srv` (three commits, a tag, with a work tree), `bare.git` (exported
/// with `git-daemon-export-ok`) and `vhost/myhost.example/r.git`.
fn fixture() -> Fixture {
    let mut fx = Fixture::new().rgit(rgit()).git(["init", "-q", "srv"]);
    if fx.is_skipped() {
        return fx;
    }
    for name in ["one", "two", "three"] {
        fx = fx
            .write(format!("srv/{name}"), format!("{name}\n"))
            .git(["-C", "srv", "add", name])
            .git(["-C", "srv", "commit", "-q", "-m", name])
            .tick();
    }
    fx.git(["-C", "srv", "tag", "-a", "-m", "tag", "v1"])
        .git(["clone", "-q", "--bare", "srv", "bare.git"])
        .write("bare.git/git-daemon-export-ok", "")
        .git(["clone", "-q", "--bare", "srv", "vhost/myhost.example/r.git"])
}

/// The daemon command of one implementation, isolated from the user's
/// configuration.
fn daemon(fx: &Fixture, ours: bool) -> Command {
    let mut cmd = if ours {
        let mut c = Command::new(rgit());
        c.arg("daemon");
        c
    } else {
        daemon_command(fx.reference_git())
    };
    isolated(fx, &mut cmd);
    if !ours {
        // Without the `git` wrapper the daemon finds `git upload-pack`
        // on PATH.
        cmd.env("PATH", server_path(fx));
    }
    cmd.current_dir(fx.work_dir());
    cmd
}

/// Run a daemon that is expected to end by itself, with `input` on
/// standard input.
fn run_daemon(fx: &Fixture, ours: bool, args: &[String], input: &[u8]) -> Output {
    let mut cmd = daemon(fx, ours);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let mut pipe = child.stdin.take().unwrap();
    let input = input.to_vec();
    let feeder = std::thread::spawn(move || {
        let _ = pipe.write_all(&input);
    });
    let out = child.wait_with_output().unwrap();
    let _ = feeder.join();
    out
}

/// Text with what differs between the implementations and between runs
/// made equal: process ids in the log, the `agent` capability.
fn normalized(fx: &Fixture, bytes: &[u8]) -> String {
    let work = fx.work_dir();
    let mut out = String::new();
    for line in text(bytes).lines() {
        let mut line = line.replace(work.to_str().unwrap(), "WORK");
        if let Some(rest) = line.strip_prefix('[') {
            if let Some((pid, message)) = rest.split_once("] ") {
                if pid.chars().all(|c| c.is_ascii_digit()) {
                    line = format!("[PID] {message}");
                }
            }
        }
        if let Some(at) = line.find(" agent=") {
            line.truncate(at);
            line.push_str(" agent=X");
        }
        if let Some(rest) = line.strip_prefix("agent=") {
            line.truncate(line.len() - rest.len());
            line.push('X');
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// The packets of an answer as text (the lengths differ with the
/// `agent`).
fn packets(fx: &Fixture, mut data: &[u8]) -> String {
    let mut out = Vec::new();
    while data.len() >= 4 {
        let Some(len) = usize::from_str_radix(&text(&data[..4]), 16)
            .ok()
            .filter(|len| *len <= data.len() && (*len == 0 || *len >= 4))
        else {
            out.extend_from_slice(b"GARBAGE\n");
            break;
        };
        if len == 0 {
            out.extend_from_slice(b"0000\n");
            data = &data[4..];
            continue;
        }
        out.extend_from_slice(&data[4..len]);
        if !data[..len].ends_with(b"\n") {
            out.push(b'\n');
        }
        data = &data[len..];
    }
    normalized(fx, &out)
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_owned()).collect()
}

#[test]
fn command_line_equals_git() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    // Invocations that end before anything is served. With `--inetd` the
    // log would go to syslog: every one names standard error.
    let cases: &[&[&str]] = &[
        &["-h"],
        &["--bogus"],
        &["--port=abc"],
        &["--init-timeout=3a"],
        &["--init-timeout=-3"],
        &["--timeout=3a"],
        &["--timeout=-3"],
        &["--max-connections=3a"],
        &["--log-destination=bogus"],
        &["--enable=bogus"],
        &["--disable=bogus"],
        &["--allow-override=bogus"],
        &["--forbid-override=bogus"],
        &["--group=nobody"],
        &["--strict-paths"],
        &["--base-path=/nonexistent/dir"],
        &["--user=no-such-user-rgit"],
        &["--inetd", "--log-destination=stderr", "--port=1234"],
        &["--inetd", "--log-destination=stderr", "--listen=127.0.0.1"],
        &["--inetd", "--log-destination=stderr", "--detach"],
        &["--inetd", "--log-destination=stderr", "--user=nobody"],
        &["--inetd", "--log-destination=stderr", "--strict-paths"],
        // Nothing on standard input.
        &["--inetd", "--log-destination=stderr"],
        &[
            "--inetd",
            "--log-destination=stderr",
            "--max-connections=-3",
        ],
        &[
            "--inetd",
            "--log-destination=stderr",
            "--user-path=/abs",
            "/nonexistent",
        ],
    ];
    for args in cases {
        let args = strings(args);
        let git = run_daemon(&fx, false, &args, b"");
        let ours = run_daemon(&fx, true, &args, b"");
        assert_eq!(
            (git.status.code(), text(&git.stdout), text(&git.stderr)),
            (ours.status.code(), text(&ours.stdout), text(&ours.stderr)),
            "{args:?}"
        );
        assert_ne!(ours.status.code(), Some(0), "{args:?}");
    }
}

/// `--user` and `--group`: the names are looked up, and a daemon that may
/// not change its user ends (the tests do not run as root).
#[test]
fn user_and_group_equal_git() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let me = Command::new("id").arg("-un").output().unwrap();
    let me = text(&me.stdout).trim().to_owned();
    if me.is_empty() || me == "root" {
        eprintln!("skipping: needs an ordinary user");
        return;
    }
    let port = free_port();
    let listen = [
        "--listen=127.0.0.1".to_owned(),
        "--reuseaddr".to_owned(),
        format!("--port={port}"),
    ];
    for (extra, message) in [
        (vec![format!("--user={me}")], "cannot drop privileges"),
        (
            vec![
                format!("--user={me}"),
                "--group=no-such-group-rgit".to_owned(),
            ],
            "group not found - no-such-group-rgit",
        ),
    ] {
        let args: Vec<String> = listen.iter().cloned().chain(extra).collect();
        let git = run_daemon(&fx, false, &args, b"");
        let ours = run_daemon(&fx, true, &args, b"");
        assert_eq!(
            (git.status.code(), text(&git.stderr)),
            (ours.status.code(), text(&ours.stderr)),
            "{args:?}"
        );
        assert_eq!(text(&ours.stderr), format!("fatal: {message}\n"));
    }
}

/// The first packet of a connection: the line and the parameters, each
/// followed by NUL, and a flush packet for the service.
fn first_packet(line: &str, params: &[&str]) -> Vec<u8> {
    let mut body = line.as_bytes().to_vec();
    body.push(0);
    for p in params {
        body.extend_from_slice(p.as_bytes());
        body.push(0);
    }
    let mut out = format!("{:04x}", body.len() + 4).into_bytes();
    out.extend_from_slice(&body);
    out.extend_from_slice(b"0000");
    out
}

/// `listed/` with a repository (`in.git`), a link to a repository
/// outside (`link.git`), a link to the directory outside (`dirlink`) and
/// a work tree whose `.git` file points outside (`gitfile`); `alias` is
/// a link to `listed`.
fn work_tree_links(fx: &Fixture) -> String {
    let work = fx.work_dir();
    let (listed, outside) = (work.join("listed"), work.join("outside"));
    std::fs::create_dir_all(listed.join("gitfile")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let clone = |to: &Path| {
        git_ok(
            fx,
            &work,
            &["clone", "-q", "--bare", "srv", to.to_str().unwrap()],
        )
    };
    clone(&outside.join("o.git"));
    clone(&listed.join("in.git"));
    std::os::unix::fs::symlink(outside.join("o.git"), listed.join("link.git")).unwrap();
    std::os::unix::fs::symlink(&outside, listed.join("dirlink")).unwrap();
    std::os::unix::fs::symlink(&listed, work.join("alias")).unwrap();
    std::fs::write(
        listed.join("gitfile/.git"),
        format!("gitdir: {}\n", outside.join("o.git").display()),
    )
    .unwrap();
    listed.display().to_string()
}

/// Both daemons must answer the request alike; returns rgit's answer and
/// log.
#[track_caller]
fn same_inetd(fx: &Fixture, args: &[&str], input: &[u8]) -> (String, String, Option<i32>) {
    let work = fx.work_dir();
    let mut argv = strings(&["--inetd", "--log-destination=stderr"]);
    argv.extend(
        args.iter()
            .map(|a| a.replace("{WORK}", work.to_str().unwrap())),
    );
    let answer = |ours: bool| {
        let out = run_daemon(fx, ours, &argv, input);
        (
            packets(fx, &out.stdout),
            normalized(fx, &out.stderr),
            out.status.code(),
        )
    };
    let (git, ours) = (answer(false), answer(true));
    assert_eq!(git, ours, "{args:?} with {:?}", text(input));
    ours
}

#[test]
fn inetd_requests_equal_git() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let base = &[
        "--verbose",
        "--export-all",
        "--base-path={WORK}",
        "--informative-errors",
    ][..];
    let with = |extra: &[&'static str]| -> Vec<&'static str> {
        base.iter().chain(extra).copied().collect()
    };
    let up = |dir: &str, params: &[&str]| first_packet(&format!("git-upload-pack {dir}"), params);

    // Served, with the request in the log.
    let (answer, log, code) = same_inetd(&fx, base, &up("/srv", &["host=MyHost:99"]));
    assert_eq!(code, Some(0));
    assert!(answer.contains(" HEAD\0multi_ack ") && answer.contains("refs/tags/v1^{}"));
    assert_eq!(
        log,
        "[PID] Extended attribute \"host\": MyHost:99\n[PID] Request upload-pack for '/srv'\n"
    );
    // The extra parameters choose the protocol.
    let (answer, log, _) = same_inetd(&fx, base, &up("/srv", &["host=h", "", "version=2"]));
    assert!(answer.starts_with("version 2\n"), "{answer}");
    assert!(log.contains("[PID] Extended attribute \"protocol\": version=2\n"));
    let (answer, log, _) = same_inetd(
        &fx,
        base,
        &up("/srv", &["host=h", "", "foo", "version=1", "bar=baz"]),
    );
    assert!(answer.starts_with("version 1\n"), "{answer}");
    assert!(log.contains("\"protocol\": foo:version=1:bar=baz\n"));
    // No host, only extra parameters, a line feed behind the path.
    same_inetd(&fx, base, &up("/srv", &[]));
    same_inetd(&fx, base, &up("/srv", &["", "version=2"]));
    same_inetd(&fx, base, &up("/srv\n", &["host=h"]));
    // The repository as upload-pack finds it.
    same_inetd(&fx, base, &up("/srv/.git", &["host=h"]));
    same_inetd(&fx, base, &up("/srv/", &["host=h"]));
    same_inetd(&fx, base, &up("/bare", &["host=h"]));

    // Requests that are none.
    for input in [
        &b""[..],
        b"hello world\n",
        b"0000",
        &first_packet("git-foo /x", &["host=h"]),
        &first_packet("upload-pack /x", &["host=h"]),
        &first_packet("git-upload-pack", &["host=h"]),
        &first_packet("git-upload-pack /srv", &["foo=bar", "host=h"]),
        &first_packet("git-upload-pack /srv", &["host=h", "version=2"]),
    ] {
        let (answer, _, code) = same_inetd(&fx, base, input);
        assert_ne!(code, Some(0));
        assert_eq!(answer, "");
    }

    // Paths that are refused.
    for dir in [
        "srv",
        " /srv",
        "",
        "/srv/.",
        "/./srv",
        "//srv",
        "/srv//",
        "/srv/..",
        "/srv/../srv",
        "/nope",
        "/srv/...",
        "/.srv",
        "/",
        "~root/x",
        "/~root/x",
    ] {
        let (answer, log, code) = same_inetd(&fx, base, &up(dir, &["host=h"]));
        assert_eq!(code, Some(255), "{dir:?}");
        assert_eq!(answer, format!("ERR no such repository: {dir}\n"), "{log}");
    }
    same_inetd(&fx, &with(&["--user-path"]), &up("~root/x", &["host=h"]));
    same_inetd(
        &fx,
        &with(&["--user-path=sub"]),
        &up("~root/x", &["host=h"]),
    );
    same_inetd(
        &fx,
        &with(&["--base-path-relaxed"]),
        &up("/nope", &["host=h"]),
    );
    same_inetd(
        &fx,
        &[
            "--verbose",
            "--export-all",
            "--base-path=/usr",
            "--base-path-relaxed",
        ],
        &up(&format!("{}/srv", fx.work_dir().display()), &["host=h"]),
    );

    // Without `--informative-errors` every refusal reads the same.
    let quiet = &["--verbose", "--export-all", "--base-path={WORK}"][..];
    let (answer, log, _) = same_inetd(&fx, quiet, &up("/nope", &["host=h"]));
    assert_eq!(
        answer,
        "ERR access denied or repository not exported: /nope\n"
    );
    assert!(log.contains("does not appear to be a git repository"));

    // Exported repositories only.
    let closed = &["--verbose", "--base-path={WORK}", "--informative-errors"][..];
    let (answer, log, _) = same_inetd(&fx, closed, &up("/srv", &["host=h"]));
    assert_eq!(answer, "ERR repository not exported: /srv\n");
    assert!(
        log.ends_with("/srv/.git': repository not exported.\n"),
        "{log}"
    );
    let (_, _, code) = same_inetd(&fx, closed, &up("/bare.git", &["host=h"]));
    assert_eq!(code, Some(0));

    // The directory list.
    let listed = |dirs: &[&'static str]| -> Vec<&'static str> {
        ["--verbose", "--export-all", "--informative-errors"]
            .iter()
            .chain(dirs)
            .copied()
            .collect()
    };
    let abs = |dir: &str| format!("{}/{dir}", fx.work_dir().display());
    for (dirs, code) in [
        (&["{WORK}"][..], 0),
        (&["/nonexistent", "{WORK}/srv"], 0),
        (&["{WORK}/srv/.git"], 0),
        (&["{WORK}/sr"], 255),
        (&["{WORK}/srv/"], 255),
        (&["/usr"], 255),
    ] {
        let (_, _, got) = same_inetd(&fx, &listed(dirs), &up(&abs("srv"), &["host=h"]));
        assert_eq!(got, Some(code), "{dirs:?}");
    }
    for (dirs, dir, code) in [
        (&["--strict-paths", "{WORK}/srv/.git"][..], "srv/.git", 0),
        (&["--strict-paths", "{WORK}/srv"], "srv", 255),
        (&["--strict-paths", "{WORK}/srv"], "srv/.git", 255),
    ] {
        let (_, _, got) = same_inetd(&fx, &listed(dirs), &up(&abs(dir), &["host=h"]));
        assert_eq!(got, Some(code), "{dirs:?} {dir}");
    }
    // Links are not followed for the directory list: the path as it was
    // asked for counts, also for a `.git` file that points elsewhere.
    let out = work_tree_links(&fx);
    for (dir, code) in [
        ("listed/link.git", 0),
        ("listed/dirlink/o.git", 0),
        ("listed/gitfile", 0),
        ("alias/in.git", 255),
    ] {
        let dirs = &["{WORK}/listed"][..];
        let (_, _, got) = same_inetd(&fx, &listed(dirs), &up(&abs(dir), &["host=h"]));
        assert_eq!(got, Some(code), "{dir}: {out}");
    }

    // `--strict-paths` does not look for `<path>.git`. (git 2.56 ends
    // with "detected dubious ownership" for a path that does not exist;
    // rgit answers as for any other missing repository.)
    let strict = listed(&["--strict-paths", "{WORK}/bare.git"]);
    let out = run_daemon(
        &fx,
        true,
        &{
            let mut argv = strings(&["--inetd", "--log-destination=stderr"]);
            argv.extend(
                strict
                    .iter()
                    .map(|a| a.replace("{WORK}", fx.work_dir().to_str().unwrap())),
            );
            argv
        },
        &up(&abs("bare"), &["host=h"]),
    );
    assert_eq!(out.status.code(), Some(255));
    assert_eq!(
        packets(&fx, &out.stdout),
        "ERR no such repository: WORK/bare\n"
    );
}

#[test]
fn inetd_services_and_overrides_equal_git() {
    let fx = fixture()
        .git(["clone", "-q", "--bare", "srv", "off.git"])
        .git(["-C", "off.git", "config", "daemon.uploadpack", "false"])
        .git(["clone", "-q", "--bare", "srv", "bad.git"])
        .git(["-C", "bad.git", "config", "daemon.uploadpack", "junk"]);
    if fx.is_skipped() {
        return;
    }
    let base = [
        "--verbose",
        "--export-all",
        "--base-path={WORK}",
        "--informative-errors",
    ];
    let with = |extra: &[&'static str]| -> Vec<&'static str> {
        base.iter().chain(extra).copied().collect()
    };
    let request =
        |service: &str, dir: &str| first_packet(&format!("git-{service} {dir}"), &["host=h"]);

    // Services that are off, here or in the repository.
    for (args, service, dir) in [
        (with(&[]), "receive-pack", "/srv"),
        (with(&[]), "upload-archive", "/srv"),
        (with(&["--disable=upload-pack"]), "upload-pack", "/srv"),
        (with(&[]), "upload-pack", "/off.git"),
        (
            with(&["--forbid-override=receive-pack"]),
            "receive-pack",
            "/srv",
        ),
        // The service is checked before the path.
        (
            with(&["--forbid-override=receive-pack"]),
            "receive-pack",
            "/nope",
        ),
        (
            with(&["--disable=upload-pack", "--forbid-override=upload-pack"]),
            "upload-pack",
            "/srv",
        ),
    ] {
        let (answer, log, code) = same_inetd(&fx, &args, &request(service, dir));
        assert_eq!(code, Some(255), "{args:?}");
        assert_eq!(answer, format!("ERR service not enabled: {dir}\n"), "{log}");
        assert!(
            log.contains(&format!("'{service}': service not enabled")),
            "{log}"
        );
    }
    // A repository's setting does not count when overriding is forbidden.
    let (_, _, code) = same_inetd(
        &fx,
        &with(&["--forbid-override=upload-pack"]),
        &request("upload-pack", "/off.git"),
    );
    assert_eq!(code, Some(0));
    let (_, _, code) = same_inetd(
        &fx,
        &with(&[
            "--forbid-override=upload-pack",
            "--allow-override=upload-pack",
        ]),
        &request("upload-pack", "/off.git"),
    );
    assert_eq!(code, Some(255));
    let (_, log, code) = same_inetd(&fx, &with(&[]), &request("upload-pack", "/bad.git"));
    assert_eq!(code, Some(128));
    assert!(log.contains("bad boolean config value 'junk' for 'daemon.uploadpack'"));
}

#[test]
fn inetd_interpolation_and_hook_equal_git() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let work = fx.work_dir();
    let up = |dir: &str, params: &[&str]| first_packet(&format!("git-upload-pack {dir}"), params);
    let args = |template: &'static str| -> Vec<&'static str> {
        vec![
            "--verbose",
            "--export-all",
            "--informative-errors",
            template,
        ]
    };

    // Virtual hosts: the host name is made safe and lower case.
    let by_host = args("--interpolated-path={WORK}/vhost/%H%D");
    let (_, log, code) = same_inetd(&fx, &by_host, &up("/r.git", &["host=MyHost.Example:1234"]));
    assert_eq!(code, Some(0));
    assert!(log.contains("[PID] Interpolated dir 'WORK/vhost/myhost.example/r.git'\n"));
    for host in [
        "..",
        "a/b",
        "../myhost.example",
        "MYHOST.EXAMPLE.",
        ".myhost..example",
    ] {
        same_inetd(&fx, &by_host, &up("/r.git", &[&format!("host={host}")]));
    }
    // Without a host nothing is interpolated.
    same_inetd(&fx, &by_host, &up("/r.git", &[]));
    // The other placeholders. `%CH` and `%IP` look the name up: only
    // names that need no network are tried here.
    let ports = args("--interpolated-path={WORK}/vhost/<%H|%P|%X|%%>%D");
    for host in ["a:1:2", "h:port", "[::1]:55", "[::1]", "::1"] {
        let (_, log, _) = same_inetd(&fx, &ports, &up("/r.git", &[&format!("host={host}")]));
        assert!(log.contains("Interpolated dir 'WORK/vhost/<"), "{log}");
    }
    let all = args("--interpolated-path={WORK}/vhost/<%H|%P|%CH|%IP>%D");
    for host in ["[::1]:55", "127.0.0.1:1", "localhost:9"] {
        let (_, log, _) = same_inetd(&fx, &all, &up("/r.git", &[&format!("host={host}")]));
        assert!(log.contains("Interpolated dir 'WORK/vhost/<"), "{log}");
    }
    // The base path does not apply to an interpolated path.
    let both = vec![
        "--verbose",
        "--export-all",
        "--base-path=/usr",
        "--interpolated-path={WORK}/vhost/%H%D",
    ];
    let (_, _, code) = same_inetd(&fx, &both, &up("/r.git", &["host=myhost.example"]));
    assert_eq!(code, Some(0));

    // The access hook: its arguments and directory, and what the client
    // is told when it says no.
    let seen = work.join("hook-args");
    rgit_difftest::write_executable(
        work.join("hook-ok"),
        format!(
            "#!/bin/sh\n{{ echo \"$# $*\"; pwd; }} >'{}'\nexit 0\n",
            seen.display()
        ),
    );
    rgit_difftest::write_executable(
        work.join("hook-deny"),
        "#!/bin/sh\necho 'go away'\necho 'second line'\nexit 1\n",
    );
    rgit_difftest::write_executable(work.join("hook-silent"), "#!/bin/sh\nexit 3\n");
    let hooked = |hook: &'static str, informative: bool| -> Vec<&'static str> {
        let mut v = vec!["--verbose", "--export-all", "--base-path={WORK}", hook];
        if informative {
            v.push("--informative-errors");
        }
        v
    };
    let mut hook_saw = Vec::new();
    for ours in [false, true] {
        let _ = std::fs::remove_file(&seen);
        let mut argv = strings(&["--inetd", "--log-destination=stderr", "--export-all"]);
        argv.push(format!("--base-path={}", work.display()));
        argv.push(format!("--access-hook={}/hook-ok", work.display()));
        let out = run_daemon(&fx, ours, &argv, &up("/srv", &["host=[::1]:77"]));
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        hook_saw.push(std::fs::read_to_string(&seen).unwrap());
    }
    assert_eq!(hook_saw[0], hook_saw[1]);
    assert_eq!(
        hook_saw[1],
        format!(
            "6 upload-pack {0}/srv/.git ::1 ::1 ::1 77\n{0}/srv/.git\n",
            work.display()
        )
    );
    let request = up("/srv", &["host=localhost"]);
    let deny = "--access-hook={WORK}/hook-deny";
    let (answer, _, code) = same_inetd(&fx, &hooked(deny, true), &request);
    assert_eq!((answer.as_str(), code), ("ERR go away: /srv\n", Some(255)));
    let (answer, _, _) = same_inetd(&fx, &hooked(deny, false), &request);
    assert_eq!(
        answer,
        "ERR access denied or repository not exported: /srv\n"
    );
    let silent = "--access-hook={WORK}/hook-silent";
    let (answer, _, _) = same_inetd(&fx, &hooked(silent, true), &request);
    assert_eq!(answer, "ERR service rejected: /srv\n");
    let missing = "--access-hook={WORK}/no-such-hook";
    let (answer, log, _) = same_inetd(&fx, &hooked(missing, true), &request);
    assert_eq!(answer, "ERR service rejected: /srv\n");
    assert!(log.contains("failed to start"), "{log}");
    // The hook is not asked about a repository that is not there.
    let (answer, _, _) = same_inetd(&fx, &hooked(deny, true), &up("/nope", &["host=localhost"]));
    assert_eq!(answer, "ERR no such repository: /nope\n");
}

// ----------------------------------------------------------------- listening

/// A running `rgit daemon` on 127.0.0.1.
struct Running {
    _server: Server,
    port: u16,
    log: PathBuf,
    pid_file: PathBuf,
}

impl Running {
    fn url(&self, path: &str) -> String {
        format!("git://127.0.0.1:{}/{path}", self.port)
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// Wait until the log has `needle`.
    #[track_caller]
    fn wait_for_log(&self, needle: &str) -> String {
        let started = Instant::now();
        loop {
            let log = self.log();
            if log.contains(needle) {
                return log;
            }
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "no {needle:?} in the log: {log}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn wait_for_file(path: &Path) -> bool {
    for _ in 0..500 {
        if std::fs::read_to_string(path).is_ok_and(|s| s.ends_with('\n')) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// Start `rgit daemon --listen=127.0.0.1` on a free port with the
/// fixture's work directory as base path. `name` keeps the files of
/// several daemons of one fixture apart.
fn start(fx: &Fixture, name: &str, args: &[&str]) -> Running {
    let work = fx.work_dir();
    for _ in 0..5 {
        let port = free_port();
        let log = work.join(format!("{name}.log"));
        let pid_file = work.join(format!("{name}.pid"));
        let _ = std::fs::remove_file(&pid_file);
        let mut cmd = daemon(fx, true);
        cmd.args(["--listen=127.0.0.1", "--reuseaddr", "--verbose"])
            .arg(format!("--port={port}"))
            .arg(format!("--pid-file={}", pid_file.display()))
            .arg(format!("--base-path={}", work.display()))
            .args(
                args.iter()
                    .map(|a| a.replace("{WORK}", work.to_str().unwrap())),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap());
        let server = Server(cmd.spawn().unwrap());
        // The pid file is written once the sockets are bound.
        if wait_for_file(&pid_file) {
            return Running {
                _server: server,
                port,
                log,
                pid_file,
            };
        }
        // Somebody else got the port in between.
    }
    panic!("rgit daemon did not start");
}

fn git(fx: &Fixture, dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(fx.reference_git());
    isolated(fx, &mut cmd);
    cmd.args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[track_caller]
fn git_ok(fx: &Fixture, dir: &Path, args: &[&str]) -> String {
    let out = git(fx, dir, args);
    assert!(out.status.success(), "{args:?}: {}", text(&out.stderr));
    text(&out.stdout)
}

#[test]
fn serves_the_reference_client() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let work = fx.work_dir();
    let d = start(&fx, "d", &["--export-all"]);
    let log = d.wait_for_log("Ready to rumble");
    assert!(log.starts_with('['), "{log}");
    // The pid file names the daemon itself.
    let pid = std::fs::read_to_string(&d.pid_file).unwrap();
    assert_eq!(pid, format!("{}\n", d._server.0.id()));

    let refs = git_ok(&fx, &work, &["ls-remote", "srv"]);
    assert_eq!(git_ok(&fx, &work, &["ls-remote", &d.url("srv")]), refs);
    for version in ["0", "1", "2"] {
        let config = format!("protocol.version={version}");
        let listed = git_ok(&fx, &work, &["-c", &config, "ls-remote", &d.url("srv")]);
        assert_eq!(listed, refs, "v{version}");
        let clone = format!("clone-{version}");
        git_ok(
            &fx,
            &work,
            &["-c", &config, "clone", "-q", &d.url("srv"), &clone],
        );
        let dir = work.join(&clone);
        git_ok(&fx, &dir, &["fsck", "--strict"]);
        assert_eq!(
            git_ok(&fx, &dir, &["rev-parse", "HEAD", "v1"]),
            git_ok(&fx, &work.join("srv"), &["rev-parse", "HEAD", "v1"])
        );
        let shallow = format!("shallow-{version}");
        git_ok(
            &fx,
            &work,
            &[
                "-c",
                &config,
                "clone",
                "-q",
                "--depth",
                "1",
                &d.url("srv"),
                &shallow,
            ],
        );
        let dir = work.join(&shallow);
        assert_eq!(git_ok(&fx, &dir, &["rev-list", "--count", "HEAD"]), "1\n");
        git_ok(&fx, &dir, &["-c", &config, "fetch", "-q", "--unshallow"]);
        assert_eq!(git_ok(&fx, &dir, &["rev-list", "--count", "HEAD"]), "3\n");
    }
    // The log tells about every connection, each served by a process of
    // its own.
    let log = d.wait_for_log("Disconnected");
    assert!(log.contains("] Connection from 127.0.0.1:"), "{log}");
    assert!(
        log.contains("] Extended attribute \"protocol\": version=2\n"),
        "{log}"
    );
    assert!(log.contains("] Request upload-pack for '/srv'\n"), "{log}");
    let daemon_pid = pid.trim();
    let connection = log.lines().find(|l| l.contains("Connection from")).unwrap();
    assert!(
        !connection.starts_with(&format!("[{daemon_pid}]")),
        "{connection}"
    );
    assert!(
        log.lines()
            .any(|l| l.starts_with(&format!("[{daemon_pid}] [")) && l.ends_with("] Disconnected")),
        "{log}"
    );

    // What is not served.
    let out = git(&fx, &work, &["ls-remote", &d.url("nope")]);
    assert_eq!(
        text(&out.stderr),
        "fatal: remote error: access denied or repository not exported: /nope\n"
    );
    d.wait_for_log("Disconnected (with error)");
    let out = git(
        &fx,
        &work.join("srv"),
        &["push", &d.url("bare.git"), "HEAD:refs/heads/x"],
    );
    assert!(
        text(&out.stderr)
            .contains("fatal: remote error: access denied or repository not exported: /bare.git"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn informative_errors_and_virtual_hosts() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let work = fx.work_dir();
    let d = start(&fx, "d", &["--informative-errors"]);
    let error = |args: &[&str]| text(&git(&fx, &work, args).stderr);
    assert_eq!(
        error(&["ls-remote", &d.url("nope")]),
        "fatal: remote error: no such repository: /nope\n"
    );
    assert_eq!(
        error(&["ls-remote", &d.url("srv")]),
        "fatal: remote error: repository not exported: /srv\n"
    );
    let pushed = git(
        &fx,
        &work.join("srv"),
        &["push", &d.url("bare.git"), "HEAD:refs/heads/x"],
    );
    assert!(
        text(&pushed.stderr).contains("fatal: remote error: service not enabled: /bare.git"),
        "{}",
        text(&pushed.stderr)
    );
    git_ok(&fx, &work, &["ls-remote", &d.url("bare.git")]);
    drop(d);

    let d = start(
        &fx,
        "v",
        &["--export-all", "--interpolated-path={WORK}/vhost/%H%D"],
    );
    let ls = |host: &str| {
        let mut cmd = Command::new(fx.reference_git());
        isolated(&fx, &mut cmd);
        cmd.args(["ls-remote", &d.url("r.git")])
            .env("GIT_OVERRIDE_VIRTUAL_HOST", host)
            .current_dir(&work)
            .output()
            .unwrap()
    };
    assert!(ls("myhost.example").status.success());
    assert!(ls("MYHOST.Example").status.success());
    assert!(!ls("..").status.success());
    assert!(!ls("other.example").status.success());
}

/// A connection that sends `request` and then waits; returns what the
/// daemon sent before it closed the connection, or `None` if it was
/// still open after `wait`.
fn hold(port: u16, request: &[u8], wait: Duration) -> Option<Vec<u8>> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.write_all(request).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let started = Instant::now();
    let mut seen = Vec::new();
    let mut buf = [0u8; 4096];
    while started.elapsed() < wait {
        match stream.read(&mut buf) {
            Ok(0) => return Some(seen),
            Ok(n) => seen.extend_from_slice(&buf[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return Some(seen),
        }
    }
    None
}

fn upload_pack_request(dir: &str) -> Vec<u8> {
    let mut packet = first_packet(&format!("git-upload-pack {dir}"), &["host=h"]);
    // Without the flush packet: the service waits for the client.
    packet.truncate(packet.len() - 4);
    packet
}

#[test]
fn timeouts_and_connection_limit() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    // A client that never sends its request.
    let d = start(&fx, "i", &["--export-all", "--init-timeout=1"]);
    let seen = hold(d.port, b"", Duration::from_secs(20));
    assert_eq!(
        seen.as_deref(),
        Some(&b""[..]),
        "the connection stayed open"
    );
    d.wait_for_log("Disconnected (with error)");
    drop(d);

    // A client that stops talking to the service.
    let d = start(&fx, "t", &["--export-all", "--timeout=1"]);
    let seen = hold(
        d.port,
        &upload_pack_request("/srv"),
        Duration::from_secs(20),
    );
    let seen = seen.expect("the connection stayed open");
    assert!(text(&seen).contains(" HEAD\0multi_ack "), "{}", text(&seen));
    let log = d.wait_for_log("Disconnected (with error)");
    assert!(
        log.contains("error: upload-pack died of signal 14\n"),
        "{log}"
    );
    drop(d);

    // One connection at a time: a second client is turned away after a
    // second, and served again once the first one is gone.
    let d = start(&fx, "m", &["--export-all", "--max-connections=1"]);
    let port = d.port;
    let request = upload_pack_request("/srv");
    let first = {
        let request = request.clone();
        std::thread::spawn(move || hold(port, &request, Duration::from_secs(4)))
    };
    d.wait_for_log("Request upload-pack");
    let second = hold(port, &request, Duration::from_secs(20));
    assert_eq!(
        second.as_deref(),
        Some(&b""[..]),
        "the second connection was served"
    );
    d.wait_for_log("Too many children, dropping connection");
    // Still open, with the advertisement.
    assert_eq!(first.join().unwrap(), None);
    d.wait_for_log("Disconnected");
    let work = fx.work_dir();
    git_ok(&fx, &work, &["ls-remote", &d.url("srv")]);
}

/// Ends the process named in a pid file when the test ends.
struct PidFile(PathBuf);

impl PidFile {
    fn pid(&self) -> Option<String> {
        let pid = std::fs::read_to_string(&self.0).ok()?;
        let pid = pid.trim();
        (!pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit())).then(|| pid.to_owned())
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        if let Some(pid) = self.pid() {
            let _ = Command::new("kill").arg(&pid).status();
        }
    }
}

#[test]
fn detach() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let work = fx.work_dir();
    let guard = PidFile(work.join("detached.pid"));
    let port = free_port();
    let mut cmd = daemon(&fx, true);
    let out = cmd
        .args([
            "--detach",
            "--listen=127.0.0.1",
            "--reuseaddr",
            "--export-all",
        ])
        .arg("--log-destination=none")
        .arg(format!("--port={port}"))
        .arg(format!("--pid-file={}", guard.0.display()))
        .arg(format!("--base-path={}", work.display()))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    // The command returns at once; the daemon runs on.
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(wait_for_file(&guard.0), "no pid file");
    let pid = guard.pid().unwrap();
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    if !stat.is_empty() {
        // A session of its own: the session id (field 6) is its pid.
        let fields: Vec<&str> = stat
            .rsplit(')')
            .next()
            .unwrap()
            .split_whitespace()
            .collect();
        assert_eq!(fields[3], pid, "{stat}");
    }
    let url = format!("git://127.0.0.1:{port}/srv");
    assert_eq!(
        git_ok(&fx, &work, &["ls-remote", &url]),
        git_ok(&fx, &work, &["ls-remote", "srv"])
    );
    drop(guard);
    for _ in 0..200 {
        if TcpStream::connect(("127.0.0.1", port)).is_err() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the detached daemon still listens");
}

/// A second daemon on a port that is taken says so and fails.
#[test]
fn port_in_use() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let d = start(&fx, "d", &["--export-all"]);
    let mut cmd = daemon(&fx, true);
    let out = cmd
        .args(["--listen=127.0.0.1"])
        .arg(format!("--port={}", d.port))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(128));
    let err = normalized(&fx, &out.stderr);
    assert_eq!(
        err,
        format!(
            "[PID] Could not bind to 127.0.0.1: Address already in use\n\
             [PID] unable to allocate any listen sockets for host 127.0.0.1 on port {0}\n\
             fatal: unable to allocate any listen sockets on port {0}\n",
            d.port
        )
    );
}
