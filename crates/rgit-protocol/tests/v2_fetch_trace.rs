// SPDX-License-Identifier: MIT
//! The protocol v2 client against `git upload-pack`, compared packet by
//! packet with the reference `git ls-remote`, `git clone` and `git fetch`.
//!
//! Each fetch scenario builds a server and two identical client
//! repositories (fixed commit times, so identical object ids). The
//! reference git fetches into one of them with `GIT_TRACE_PACKET` on; rgit's
//! [`Client`] fetches into the other, driven the way git drives it (same
//! `ref-prefix` lines, wants, local tips). All packets of the client
//! process (`fetch`, `clone`, `ls-remote` and `sideband` lines, requests
//! and responses) must be identical, and the received pack must index.
//!
//! Scenarios:
//! - `ls-remote` (ls-refs with peel, symrefs, unborn; closing flush);
//! - `clone` (wants for every listed ref, `done` at once);
//! - fetch after 100 local commits (have rounds of 16, 32, 64; known
//!   common tip of an unchanged branch first; `ready`);
//! - fetch of a new unrelated branch (ACK without `ready`, the ACKed
//!   commit repeated first in later rounds, `done` after 256 haves in
//!   vain);
//! - fetch with `--deepen` into a shallow clone (`shallow`, `deepen`,
//!   `deepen-relative`, `shallow-info`).
//!
//! This test binary owns the process-wide `GIT_TRACE_PACKET` key, so it
//! contains a single test.

mod common;
mod v2common;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout};

use rgit_protocol::v2::{Client, FetchArgs, FetchOutcome, LsRefsArgs};
use v2common::{Env, Plan, USER_AGENT, drive, fetch_args, oid};

type PipeClient = Client<ChildStdout, ChildStdin>;

/// Run the reference git with packet tracing; returns the trace lines of
/// the client process.
fn reference(env: &Env, cwd: &Path, args: &[&str], n: usize) -> String {
    let trace = env.home.join(format!("ref-trace-{n}"));
    let mut c = env.cmd();
    c.current_dir(cwd)
        .env("GIT_TRACE_PACKET", &trace)
        .env("GIT_TRACE_BARE", "1")
        .env("GIT_USER_AGENT", USER_AGENT)
        .args(["-c", "protocol.version=2"])
        .args(args);
    common::run(c);
    std::fs::read_to_string(&trace)
        .unwrap()
        .lines()
        .filter(|l| {
            l.strip_prefix("packet:")
                .is_some_and(|rest| !rest.trim_start().starts_with("upload-pack"))
        })
        .map(|l| format!("{l}\n"))
        .collect()
}

/// The `ref-prefix` arguments the reference sent with `ls-refs`.
fn ref_prefixes(trace: &str) -> Vec<String> {
    trace
        .lines()
        .take_while(|l| !l.ends_with("> command=fetch"))
        .filter_map(|l| l.split_once("> ref-prefix ").map(|(_, p)| p.to_owned()))
        .collect()
}

fn connect(env: &Env, server: &Path, name: &'static str) -> (Child, PipeClient) {
    let mut child = env.upload_pack(server);
    let client = Client::connect(
        child.stdout.take().unwrap(),
        child.stdin.take().unwrap(),
        false,
        name,
    )
    .unwrap();
    (child, client)
}

fn finish(mut child: Child, client: PipeClient) {
    drop(client);
    assert!(child.wait().unwrap().success());
}

/// Server `srv<n>` with `c1`..`c5` on main (times 1001..1005).
fn server(env: &Env, n: usize) -> PathBuf {
    let srv = env.init(&format!("srv{n}"));
    env.commits(&srv, "main", None, "c", 5, 1000);
    srv
}

struct Scenario<'a> {
    n: usize,
    /// Server changes before the clients clone.
    before_clone: &'a dyn Fn(&Env, &Path),
    /// Changes made in both clients.
    client: &'a dyn Fn(&Env, &Path),
    /// Server changes after the clients cloned.
    after_clone: &'a dyn Fn(&Env, &Path),
    clone_args: &'a [&'a str],
    fetch_args: &'a [&'a str],
    args: &'a dyn Fn(&Path) -> FetchArgs,
}

fn fetch_scenario(env: &Env, s: &Scenario<'_>, expected: &mut String) -> FetchOutcome {
    let srv = server(env, s.n);
    (s.before_clone)(env, &srv);
    let url = format!("file://{}", srv.display());
    let cli_ref = env.clone(Path::new(&url), &format!("cli{}-ref", s.n), s.clone_args);
    let cli_rgit = env.clone(Path::new(&url), &format!("cli{}-rgit", s.n), s.clone_args);
    for cli in [&cli_ref, &cli_rgit] {
        (s.client)(env, cli);
    }
    assert_eq!(env.tips(&cli_ref), env.tips(&cli_rgit));
    (s.after_clone)(env, &srv);

    let mut cmd = vec!["fetch"];
    cmd.extend_from_slice(s.fetch_args);
    cmd.push("origin");
    let trace = reference(env, &cli_ref, &cmd, s.n);
    assert!(trace.contains("> command=fetch"), "{trace}");
    expected.push_str(&trace);

    let (child, mut client) = connect(env, &srv, "fetch");
    let plan = Plan {
        ls_refs: LsRefsArgs {
            ref_prefixes: ref_prefixes(&trace),
            ..LsRefsArgs::standard()
        },
        fetch: (s.args)(&cli_rgit),
        clone: false,
    };
    let result = drive(env, &mut client, Some(&cli_rgit), &plan);
    finish(child, client);
    env.index_pack(&cli_rgit, &result.pack);
    result.outcome
}

fn nothing(_: &Env, _: &Path) {}

#[test]
fn v2_client_trace_matches_git() {
    let Some(git) = common::reference_git() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let env = Env {
        git,
        home: dir.path().to_owned(),
    };
    let ours = env.home.join("rgit-trace");
    rgit_trace::TRACE_BARE.init_from(Some(OsStr::new("1")));
    rgit_trace::TRACE_PACKET.init_from(Some(ours.as_os_str()));
    let mut expected = String::new();

    // ls-remote and clone against a server with tags and a symref.
    let srv = server(&env, 0);
    env.run(&srv, &["tag", "-a", "v1", "-m", "v1", "main~1"]);
    env.run(&srv, &["tag", "light", "main~2"]);
    env.run(
        &srv,
        &["symbolic-ref", "refs/heads/alias", "refs/heads/main"],
    );
    let srv_s = srv.to_str().unwrap();
    expected.push_str(&reference(&env, &env.home, &["ls-remote", srv_s], 100));
    let (child, mut client) = connect(&env, &srv, "ls-remote");
    client.set_agent(USER_AGENT);
    let refs = client.ls_refs(&LsRefsArgs::standard()).unwrap();
    client.close().unwrap();
    finish(child, client);
    assert_eq!(refs.len(), 5);
    assert_eq!(
        refs[1].symref_target.as_deref(),
        Some(&b"refs/heads/main"[..])
    );
    assert!(refs[4].peeled.is_some());

    let url = format!("file://{srv_s}");
    let trace = reference(&env, &env.home, &["clone", "-q", &url, "clone-ref"], 101);
    expected.push_str(&trace);
    let target = env.init("clone-rgit");
    let (child, mut client) = connect(&env, &srv, "clone");
    let plan = Plan {
        ls_refs: LsRefsArgs {
            ref_prefixes: ref_prefixes(&trace),
            ..LsRefsArgs::standard()
        },
        fetch: FetchArgs {
            include_tag: false,
            ..fetch_args()
        },
        clone: true,
    };
    let result = drive(&env, &mut client, None, &plan);
    finish(child, client);
    assert_eq!(result.outcome.rounds, 1);
    env.index_pack(&target, &result.pack);

    // 100 local commits; `side` (unrelated history) is unchanged on the
    // server, so its tip is known to be common and offered first.
    let out = fetch_scenario(
        &env,
        &Scenario {
            n: 1,
            before_clone: &|env, srv| env.commits(srv, "side", None, "s", 3, 1500),
            client: &|env, cli| env.commits(cli, "main", None, "l", 100, 2000),
            after_clone: &|env, srv| env.commits(srv, "main", None, "new", 1, 3000),
            clone_args: &[],
            fetch_args: &[],
            args: &|_| fetch_args(),
        },
        &mut expected,
    );
    assert!(out.packfile);
    assert_eq!(out.rounds, 3);

    // A new unrelated branch: the common base of `main` is found, but the
    // server never gets ready; the client gives up after 256 haves in vain.
    let out = fetch_scenario(
        &env,
        &Scenario {
            n: 2,
            before_clone: &nothing,
            client: &|env, cli| {
                env.commits(cli, "main", None, "l", 40, 2000);
                env.commits(cli, "old", None, "o", 600, 100);
            },
            after_clone: &|env, srv| {
                env.commits(srv, "main", None, "new", 1, 3000);
                env.commits(srv, "orph", None, "x", 1, 3100);
            },
            clone_args: &[],
            fetch_args: &[],
            args: &|_| fetch_args(),
        },
        &mut expected,
    );
    assert_eq!(out.common.len(), 1);
    assert!(out.rounds >= 5);

    // Deepen a shallow clone.
    let out = fetch_scenario(
        &env,
        &Scenario {
            n: 3,
            before_clone: &nothing,
            client: &nothing,
            after_clone: &|env, srv| env.commits(srv, "main", None, "new", 1, 3000),
            clone_args: &["--depth=1"],
            fetch_args: &["--deepen=1"],
            args: &|cli| FetchArgs {
                shallow: std::fs::read_to_string(cli.join(".git/shallow"))
                    .unwrap()
                    .lines()
                    .map(oid)
                    .collect(),
                deepen: Some(1),
                deepen_relative: true,
                ..fetch_args()
            },
        },
        &mut expected,
    );
    let info = out.shallow_info.expect("shallow-info");
    assert_eq!(info.shallow.len(), 1);
    assert_eq!(info.unshallow.len(), 1);

    let actual = std::fs::read_to_string(&ours).unwrap();
    if actual != expected {
        let dump = Path::new(env!("CARGO_TARGET_TMPDIR"));
        std::fs::write(dump.join("v2-trace-expected"), &expected).unwrap();
        std::fs::write(dump.join("v2-trace-actual"), &actual).unwrap();
        let e: Vec<&str> = expected.lines().collect();
        let a: Vec<&str> = actual.lines().collect();
        for (i, (x, y)) in e.iter().zip(a.iter()).enumerate() {
            assert_eq!(x, y, "first difference at trace line {i}");
        }
        assert_eq!(e.len(), a.len(), "trace length");
    }
}
