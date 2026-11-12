// SPDX-License-Identifier: MIT
//! The protocol v2 client over the connection kinds of a real transport:
//! a stateful pipe to `git upload-pack`, stateless requests to
//! `git upload-pack --stateless-rpc` (as smart HTTP does them), and a TCP
//! connection to `git daemon`.

mod common;
mod v2common;

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use rgit_protocol::pktline::encode;
use rgit_protocol::v2::{Client, FetchArgs, LsRefsArgs, Pipe};
use v2common::{Env, Plan, Stateless, drive, fetch_args};

/// Records everything written.
struct Tee<W> {
    inner: W,
    seen: std::rc::Rc<std::cell::RefCell<Vec<u8>>>,
}

impl<W: Write> Write for Tee<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.seen.borrow_mut().extend_from_slice(&buf[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn env() -> Option<(tempfile::TempDir, Env)> {
    let git = common::reference_git()?;
    let dir = tempfile::tempdir().unwrap();
    let env = Env {
        git,
        home: dir.path().to_owned(),
    };
    Some((dir, env))
}

/// Server with 5 commits, a client clone with 100 more, then one new
/// server commit.
fn setup(env: &Env) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let srv = env.init("srv");
    env.commits(&srv, "main", None, "c", 5, 1000);
    let a = env.clone(&srv, "a", &[]);
    let b = env.clone(&srv, "b", &[]);
    for cli in [&a, &b] {
        env.commits(cli, "main", None, "l", 100, 2000);
    }
    env.commits(&srv, "main", None, "new", 1, 3000);
    (srv, a, b)
}

fn plan() -> Plan {
    Plan {
        ls_refs: LsRefsArgs {
            ref_prefixes: vec!["refs/heads/".into()],
            ..LsRefsArgs::standard()
        },
        fetch: fetch_args(),
        clone: false,
    }
}

#[test]
fn stateless_requests_equal_stateful_ones() {
    let Some((_dir, env)) = env() else {
        return;
    };
    let (srv, a, b) = setup(&env);

    let mut child = env.upload_pack(&srv);
    let stateful_bytes = std::rc::Rc::default();
    let mut client = Client::connect(
        Pipe::new(
            child.stdout.take().unwrap(),
            Tee {
                inner: child.stdin.take().unwrap(),
                seen: std::rc::Rc::clone(&stateful_bytes),
            },
            false,
        ),
        "fetch",
    )
    .unwrap();
    let stateful = drive(&env, &mut client, Some(&a), &plan());
    drop(client);
    assert!(child.wait().unwrap().success());
    assert_eq!(stateful.outcome.rounds, 3);
    env.index_pack(&a, &stateful.pack);

    let (r, w) = Stateless::new(&env, &srv, true);
    let stateless_bytes = std::rc::Rc::default();
    let mut client = Client::connect(
        Pipe::new(
            r,
            Tee {
                inner: w,
                seen: std::rc::Rc::clone(&stateless_bytes),
            },
            true,
        ),
        "fetch",
    )
    .unwrap();
    let stateless = drive(&env, &mut client, Some(&b), &plan());
    let (reader, _) = client.into_inner().into_parts();
    // One request for ls-refs, one per negotiation round.
    assert_eq!(reader.requests(), 1 + stateless.outcome.rounds);
    assert_eq!(stateless.outcome, stateful.outcome);
    assert_eq!(stateless.refs, stateful.refs);
    assert_eq!(*stateless_bytes.borrow(), *stateful_bytes.borrow());
    env.index_pack(&b, &stateless.pack);
}

#[test]
fn stateless_ls_refs_without_fetch_sends_no_flush() {
    let Some((_dir, env)) = env() else {
        return;
    };
    let srv = env.init("srv");
    env.commits(&srv, "main", None, "c", 2, 1000);
    let (r, w) = Stateless::new(&env, &srv, false);
    let mut client = Client::connect(Pipe::new(r, w, true), "ls-remote").unwrap();
    let refs = client.ls_refs(&LsRefsArgs::standard()).unwrap();
    assert_eq!(refs.len(), 2);
    client.close().unwrap();
    let (reader, _) = client.into_inner().into_parts();
    assert_eq!(reader.requests(), 1);
}

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_daemon(env: &Env, base: &Path) -> Option<(Daemon, u16)> {
    let port = TcpListener::bind("127.0.0.1:0")
        .ok()?
        .local_addr()
        .ok()?
        .port();
    let mut c = env.cmd();
    c.arg("daemon")
        .arg("--reuseaddr")
        .arg("--export-all")
        .arg(format!("--base-path={}", base.display()))
        .arg("--listen=127.0.0.1")
        .arg(format!("--port={port}"))
        .arg(base)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let daemon = Daemon(c.spawn().ok()?);
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Some((daemon, port));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

#[test]
fn git_daemon_clone() {
    let Some((_dir, env)) = env() else {
        return;
    };
    let srv = env.init("srv");
    env.commits(&srv, "main", None, "c", 20, 1000);
    env.run(&srv, &["tag", "-a", "v1", "-m", "v1", "main~3"]);
    let Some((_daemon, port)) = start_daemon(&env, &env.home) else {
        eprintln!("skipping: git daemon did not start");
        return;
    };
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .write_all(&encode(b"git-upload-pack /srv\0host=127.0.0.1\0\0version=2\0").unwrap())
        .unwrap();
    let reader = stream.try_clone().unwrap();
    let mut client = Client::connect(Pipe::new(reader, stream, false), "clone").unwrap();
    let plan = Plan {
        ls_refs: LsRefsArgs {
            ref_prefixes: vec!["refs/heads/".into(), "refs/tags/".into(), "HEAD".into()],
            ..LsRefsArgs::standard()
        },
        fetch: FetchArgs {
            include_tag: false,
            ..fetch_args()
        },
        clone: true,
    };
    let result = drive(&env, &mut client, None, &plan);
    assert_eq!(result.refs.len(), 3);
    assert_eq!(result.outcome.rounds, 1);
    let target = env.init("target");
    env.index_pack(&target, &result.pack);

    // A second command on the same connection, then the closing flush.
    let refs = client.ls_refs(&LsRefsArgs::default()).unwrap();
    assert_eq!(refs.len(), 3);
    client.close().unwrap();
    let (mut reader, _) = client.into_inner().into_parts();
    let mut rest = Vec::new();
    reader.read_to_end(&mut rest).unwrap();
    assert!(rest.is_empty());
}
