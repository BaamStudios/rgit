// SPDX-License-Identifier: MIT
//! End-to-end connections to a real `git-upload-pack`: the local transport,
//! and the SSH transport through a fake `ssh` that runs the remote command
//! locally (like git's own test helper does). Skipped without a reference
//! git.

#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use rgit_protocol::{Packet, ProtocolVersion, Reader};
use rgit_transport::url::{Remote, parse_remote};
use rgit_transport::{
    ConnectOptions, Connection, IpVersion, LocalTransport, Service, SshCommand, SshTransport,
    Transport, Variant,
};

/// Runs the last argument (the remote command) with the shell.
const FAKE_SSH: &str = r#"#!/bin/sh
for a in "$@"; do last=$a; done
exec /bin/sh -c "$last"
"#;

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
    base_env: Vec<(OsString, OsString)>,
}

fn setup() -> Option<Env> {
    let git = rgit_difftest::env::reference_git()?;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let run = |args: &[&str]| {
        let st = Command::new(&git)
            .args(args)
            .current_dir(&root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@example.com")
            .status()
            .unwrap();
        assert!(st.success(), "{args:?}");
    };
    run(&["init", "-q", "-b", "main", "repo"]);
    run(&["-C", "repo", "commit", "-q", "--allow-empty", "-m", "x"]);
    let exec_path = Command::new(&git).arg("--exec-path").output().unwrap();
    let exec_path = String::from_utf8(exec_path.stdout).unwrap();
    let ssh = root.join("ssh");
    fs::write(&ssh, FAKE_SSH).unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
    let mut dirs = vec![PathBuf::from(exec_path.trim())];
    dirs.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let base_env = vec![
        (OsString::from("PATH"), std::env::join_paths(dirs).unwrap()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
    ];
    Some(Env {
        _dir: dir,
        root,
        base_env,
    })
}

fn first_packet(conn: &mut Box<dyn Connection>) -> Vec<u8> {
    let mut r = Reader::new(conn.as_mut(), "test");
    match r.read().unwrap() {
        Packet::Data(d) => d.to_vec(),
        other => panic!("unexpected {other:?}"),
    }
}

/// Ask for `ls-refs` (v2) or read the ref advertisement (v0), then end.
fn exercise(mut conn: Box<dyn Connection>, version: ProtocolVersion) {
    assert!(!conn.is_stateless());
    let first = first_packet(&mut conn);
    match version {
        ProtocolVersion::V2 => {
            assert_eq!(first, b"version 2");
            // Skip the capability advertisement.
            let mut r = Reader::new(conn.as_mut(), "test");
            while !matches!(r.read().unwrap(), Packet::Flush) {}
            drop(r);
            conn.write_all(b"0014command=ls-refs\n00010000").unwrap();
            conn.end_request().unwrap();
            let mut r = Reader::new(conn.as_mut(), "test");
            let mut refs = Vec::new();
            loop {
                match r.read().unwrap() {
                    Packet::Data(d) => refs.push(String::from_utf8_lossy(d).into_owned()),
                    Packet::Flush => break,
                    other => panic!("unexpected {other:?}"),
                }
            }
            drop(r);
            assert!(refs.iter().any(|r| r.ends_with(" HEAD")), "{refs:?}");
            conn.write_all(b"0000").unwrap();
        }
        _ => {
            assert!(
                first.windows(5).any(|w| w == b" HEAD"),
                "{:?}",
                String::from_utf8_lossy(&first)
            );
            let mut r = Reader::new(conn.as_mut(), "test");
            while !matches!(r.read().unwrap(), Packet::Flush) {}
            drop(r);
            conn.write_all(b"0000").unwrap();
        }
    }
    conn.end_request().unwrap();
    conn.close().unwrap();
}

fn opts(version: ProtocolVersion) -> ConnectOptions {
    let mut o = ConnectOptions::new(Service::UploadPack);
    o.protocol_version = version;
    o
}

fn repo_path(root: &Path) -> String {
    root.join("repo").display().to_string()
}

#[test]
fn local_transport() {
    let Some(env) = setup() else { return };
    for version in [ProtocolVersion::V0, ProtocolVersion::V2] {
        for url in [
            repo_path(&env.root),
            format!("file://{}", repo_path(&env.root)),
        ] {
            let path = match parse_remote(&url, &|p| Path::new(p).exists()).unwrap() {
                Remote::Local { path } => path,
                other => panic!("{other:?}"),
            };
            let mut t = LocalTransport {
                path,
                base_env: env.base_env.clone(),
            };
            exercise(t.connect(&opts(version)).unwrap(), version);
        }
    }
}

#[test]
fn ssh_transport_through_fake_ssh() {
    let Some(env) = setup() else { return };
    for version in [ProtocolVersion::V0, ProtocolVersion::V2] {
        let url = format!("ssh://example.org{}", repo_path(&env.root));
        let target = match parse_remote(&url, &|_| false).unwrap() {
            Remote::Ssh(t) => t,
            other => panic!("{other:?}"),
        };
        let mut t = SshTransport {
            target,
            ssh: SshCommand {
                command: env.root.join("ssh").display().to_string().into(),
                is_cmdline: false,
                variant: Some(Variant::Ssh.name().into()),
            },
            ip: IpVersion::Any,
            base_env: env.base_env.clone(),
        };
        exercise(t.connect(&opts(version)).unwrap(), version);
    }
}

#[test]
fn failing_service_is_reported() {
    let Some(env) = setup() else { return };
    let mut t = LocalTransport {
        path: env.root.join("missing").display().to_string().into(),
        base_env: env.base_env.clone(),
    };
    let mut conn = t.connect(&opts(ProtocolVersion::V0)).unwrap();
    let mut buf = Vec::new();
    let _ = std::io::Read::read_to_end(&mut conn, &mut buf);
    assert!(buf.is_empty());
    assert!(matches!(
        conn.close(),
        Err(rgit_transport::Error::ServiceFailed(Some(_)))
    ));
}

/// `ls-refs` with the protocol v2 client over a transport connection.
fn ls_refs(conn: Box<dyn Connection>) -> Vec<String> {
    use rgit_protocol::v2::{Client, LsRefsArgs};
    let mut client = Client::connect(conn, "test").unwrap();
    let refs = client.ls_refs(&LsRefsArgs::standard()).unwrap();
    client.close().unwrap();
    client.into_inner().close().unwrap();
    let mut names: Vec<String> = refs
        .iter()
        .map(|r| String::from_utf8_lossy(&r.name).into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn v2_client_over_local_and_ssh() {
    let Some(env) = setup() else { return };
    let want = ["HEAD", "refs/heads/main"];
    let mut local = LocalTransport {
        path: repo_path(&env.root).into(),
        base_env: env.base_env.clone(),
    };
    assert_eq!(
        ls_refs(local.connect(&opts(ProtocolVersion::V2)).unwrap()),
        want
    );

    let url = format!("example.org:{}", repo_path(&env.root));
    let Remote::Ssh(target) = parse_remote(&url, &|_| false).unwrap() else {
        panic!("not ssh");
    };
    // The fake program is named `ssh`, so the OpenSSH variant is detected
    // and GIT_PROTOCOL reaches upload-pack through the environment.
    let mut ssh = SshTransport {
        target,
        ssh: SshCommand {
            command: env.root.join("ssh").display().to_string().into(),
            is_cmdline: false,
            variant: None,
        },
        ip: IpVersion::Any,
        base_env: env.base_env.clone(),
    };
    assert_eq!(
        ls_refs(ssh.connect(&opts(ProtocolVersion::V2)).unwrap()),
        want
    );
}
