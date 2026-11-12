// SPDX-License-Identifier: MIT
//! Reproduce the client side of `GIT_TRACE_PACKET=... git ls-remote` with
//! rgit's reader and writer.
//!
//! For protocol versions 0, 1 and 2 the reference `git ls-remote` runs
//! against a local repository with packet tracing on. Then rgit's
//! [`Reader`]/[`Writer`] talk to `git upload-pack` directly and replay what
//! ls-remote does on the wire. The `ls-remote` lines of both traces and the
//! listed refs must be identical.
//!
//! This test binary owns the process-wide `GIT_TRACE_PACKET` key, so it
//! contains a single test.

mod common;

use std::ffi::OsStr;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use rgit_protocol::capabilities::split_ref_line;
use rgit_protocol::{Capabilities, Packet, ProtocolVersion, Reader, Writer};

const USER_AGENT: &str = "rgit-protocol-test";

fn reference_trace(git_bin: &Path, home: &Path, repo: &Path, version: u8) -> (String, Vec<u8>) {
    let trace = home.join(format!("ref-trace-v{version}"));
    let mut c = common::git(git_bin, home);
    c.env("GIT_TRACE_PACKET", &trace)
        .env("GIT_TRACE_BARE", "1")
        .env("GIT_USER_AGENT", USER_AGENT)
        .arg("-c")
        .arg(format!("protocol.version={version}"))
        .arg("ls-remote")
        .arg(repo);
    let out = common::run(c);
    let text = std::fs::read_to_string(&trace).unwrap();
    let client: String = text
        .lines()
        .filter(|l| l.starts_with("packet:    ls-remote"))
        .map(|l| format!("{l}\n"))
        .collect();
    (client, out.stdout)
}

/// What `git ls-remote` does on the wire, written with rgit's types.
/// Returns ls-remote's standard output.
fn rgit_ls_remote(git_bin: &Path, home: &Path, repo: &Path, version: u8) -> Vec<u8> {
    let mut c: Command = common::git(git_bin, home);
    c.env("GIT_USER_AGENT", USER_AGENT)
        .arg("upload-pack")
        .arg(repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    if version > 0 {
        c.env("GIT_PROTOCOL", format!("version={version}"));
    }
    let mut child = c.spawn().unwrap();
    let mut reader = Reader::new(child.stdout.take().unwrap(), "ls-remote");
    let mut writer = Writer::new(child.stdin.take().unwrap(), "ls-remote");
    let agent = rgit_protocol::agent::agent_from(Some(OsStr::new(USER_AGENT)), "Linux");
    let mut listing = Vec::new();

    match ProtocolVersion::read(&mut reader).unwrap() {
        ProtocolVersion::V2 => {
            assert_eq!(version, 2);
            let caps = Capabilities::read_v2(&mut reader).unwrap();
            assert!(caps.contains("ls-refs"));
            writer.write_line("command=ls-refs").unwrap();
            if caps.contains("agent") {
                writer.write_line(&format!("agent={agent}")).unwrap();
            }
            if let Some(fmt) = caps.object_format() {
                writer.write_line(&format!("object-format={fmt}")).unwrap();
            }
            writer.delim_pkt().unwrap();
            for arg in ["peel", "symrefs", "unborn"] {
                writer.write_line(arg).unwrap();
            }
            writer.flush_pkt().unwrap();
            writer.flush_io().unwrap();
            loop {
                match reader.read().unwrap() {
                    Packet::Flush => break,
                    Packet::Data(line) => {
                        let line = std::str::from_utf8(line).unwrap();
                        let mut words = line.split(' ');
                        let oid = words.next().unwrap();
                        let name = words.next().unwrap();
                        writeln!(listing, "{oid}\t{name}").unwrap();
                        for attr in words {
                            if let Some(peeled) = attr.strip_prefix("peeled:") {
                                writeln!(listing, "{peeled}\t{name}^{{}}").unwrap();
                            }
                        }
                    }
                    other => panic!("unexpected {other:?}"),
                }
            }
        }
        v => {
            assert_eq!(v as u8, version);
            let mut first = true;
            loop {
                match reader.read().unwrap() {
                    Packet::Flush => break,
                    Packet::Data(line) => {
                        let (refpart, caps) = split_ref_line(line);
                        if first {
                            let caps = caps.expect("capabilities on first ref");
                            assert_eq!(caps.agent(), Some(USER_AGENT));
                            assert_eq!(caps.symrefs().next(), Some(("HEAD", "refs/heads/main")));
                        }
                        first = false;
                        let refpart = std::str::from_utf8(refpart).unwrap();
                        let (oid, name) = refpart.split_once(' ').unwrap();
                        writeln!(listing, "{oid}\t{name}").unwrap();
                    }
                    other => panic!("unexpected {other:?}"),
                }
            }
        }
    }
    writer.flush_pkt().unwrap();
    writer.flush_io().unwrap();
    drop(writer);
    assert_eq!(reader.read_or_eof().unwrap(), None);
    assert!(child.wait().unwrap().success());
    listing
}

#[test]
fn ls_remote_trace_is_reproduced() {
    let Some(git_bin) = common::reference_git() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let repo = common::make_repo(&git_bin, home);

    let ours = home.join("rgit-trace");
    rgit_trace::TRACE_BARE.init_from(Some(OsStr::new("1")));
    rgit_trace::TRACE_PACKET.init_from(Some(ours.as_os_str()));
    assert!(rgit_trace::TRACE_PACKET.enabled());

    let mut expected = String::new();
    for version in [0u8, 1, 2] {
        let (trace, stdout) = reference_trace(&git_bin, home, &repo, version);
        assert!(!trace.is_empty());
        expected.push_str(&trace);
        let listing = rgit_ls_remote(&git_bin, home, &repo, version);
        assert_eq!(
            String::from_utf8(listing).unwrap(),
            String::from_utf8(stdout).unwrap(),
            "ls-remote output, protocol v{version}"
        );
    }
    let actual = std::fs::read_to_string(&ours).unwrap();
    assert_eq!(actual, expected);
}
