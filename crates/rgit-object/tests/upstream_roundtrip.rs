// SPDX-License-Identifier: MIT

//! Round-trip property over a real repository: for every commit, tree and
//! tag in the `upstream/git` clone, `serialize(parse(x)) == x` and the id of
//! the serialised object equals the id git reports.
//!
//! The repository is taken from `RGIT_UPSTREAM_GIT`, or from `upstream/git`
//! in an ancestor directory of this crate (see `scripts/fetch-upstream.sh`).
//! The test is skipped when neither exists or no `git` binary is available.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::Stdio;

use common::{git, git_available, git_cmd};
use rgit_hash::{HashKind, ObjectId, hash_object};
use rgit_object::{Object, ObjectKind, fsck};

fn upstream_repo() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("RGIT_UPSTREAM_GIT") {
        return Some(PathBuf::from(p));
    }
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.ancestors()
        .map(|a| a.join("upstream/git"))
        .find(|p| p.join(".git").exists())
}

#[test]
fn every_upstream_object_round_trips() {
    let Some(repo) = upstream_repo().filter(|_| git_available()) else {
        eprintln!("skipped: no upstream/git clone or no git binary");
        return;
    };
    let list = git(
        &repo,
        &[
            "cat-file",
            "--batch-all-objects",
            "--batch-check=%(objectname) %(objecttype)",
            "--unordered",
        ],
        b"",
    );
    let mut wanted = Vec::new();
    let mut blobs = 0usize;
    for line in list.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
        let (oid, kind) = line.split_at(line.iter().position(|&b| b == b' ').unwrap());
        // Blobs round-trip trivially; check a sample of them.
        if &kind[1..] == b"blob" {
            blobs += 1;
            if blobs % 64 != 0 {
                continue;
            }
        }
        wanted.extend_from_slice(oid);
        wanted.push(b'\n');
    }
    let hash = HashKind::from_hex_len(wanted.iter().position(|&b| b == b'\n').unwrap()).unwrap();

    let mut child = git_cmd(&repo)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || stdin.write_all(&wanted).unwrap());
    let mut out = BufReader::with_capacity(1 << 20, child.stdout.take().unwrap());

    let mut counts = [0usize; 4];
    let mut fsck_findings = 0usize;
    let mut line = Vec::new();
    let mut buf = Vec::new();
    loop {
        line.clear();
        if out.read_until(b'\n', &mut line).unwrap() == 0 {
            break;
        }
        let text = std::str::from_utf8(&line).unwrap().trim_end();
        let mut parts = text.split(' ');
        let oid: ObjectId = parts.next().unwrap().parse().unwrap();
        let kind: ObjectKind = parts.next().unwrap().parse().unwrap();
        let size: usize = parts.next().unwrap().parse().unwrap();
        buf.resize(size + 1, 0);
        out.read_exact(&mut buf).unwrap();
        let data = &buf[..size];

        let obj = Object::parse(kind, hash, data)
            .unwrap_or_else(|e| panic!("{oid} ({kind}) does not parse: {e}"));
        let ser = obj.serialize();
        assert!(ser == data, "{oid} ({kind}) does not round-trip");
        assert_eq!(hash_object(hash, kind.name(), &ser).unwrap(), oid);
        // The strict layer must not crash on real data either; count what
        // it finds (old history has a few odd objects).
        for f in fsck::findings(kind, hash, data) {
            fsck_findings += 1;
            if std::env::var_os("RGIT_SHOW_FINDINGS").is_some() {
                eprintln!("{oid}: {f}");
            }
        }
        counts[kind as usize] += 1;
    }
    writer.join().unwrap();
    assert!(child.wait().unwrap().success());
    eprintln!(
        "round-tripped blobs (sampled) {}, trees {}, commits {}, tags {}; fsck findings {}",
        counts[0], counts[1], counts[2], counts[3], fsck_findings
    );
    assert!(counts[2] > 0 && counts[1] > 0);
}
