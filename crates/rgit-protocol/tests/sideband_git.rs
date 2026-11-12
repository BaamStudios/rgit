// SPDX-License-Identifier: MIT
//! Differential test of sideband demultiplexing against `git fetch`.
//!
//! A scripted upload-pack (`--upload-pack="cat <adv>; <read request>; cat
//! <response>; :"`) sends a ref advertisement with `side-band-64k`, waits
//! for the client's request, then answers `NAK` and sideband packets:
//! progress split over packets, `\r`, empty lines, and the pack split over
//! several channel 1 packets. The reference git's stderr (not a terminal)
//! and its `sideband` trace lines must equal what [`SidebandReader`]
//! produces for the same bytes, and the pack data must come out unchanged.
//!
//! This test binary owns the process-wide `GIT_TRACE_PACKET` key, so it
//! contains a single test.

mod common;

use std::ffi::OsStr;
use std::path::Path;

use rgit_protocol::pktline::encode;
use rgit_protocol::{Progress, ProgressStyle, Reader, SidebandReader};

fn pack_of_main(git_bin: &Path, home: &Path, repo: &Path) -> (String, Vec<u8>) {
    let mut c = common::git(git_bin, home);
    c.arg("-C").arg(repo).args(["rev-parse", "main"]);
    let oid = String::from_utf8(common::run(c).stdout)
        .unwrap()
        .trim()
        .to_owned();
    let mut c = common::git(git_bin, home);
    c.arg("-C")
        .arg(repo)
        .args(["pack-objects", "--stdout", "--revs", "-q"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());
    let mut child = c.spawn().unwrap();
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, "{oid}").unwrap();
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    (oid, out.stdout)
}

/// The sideband part of a scenario: packets after `NAK`.
fn scenario(n: usize, pack: &[u8]) -> Vec<Vec<u8>> {
    let mut packets: Vec<Vec<u8>> = Vec::new();
    let progress = |s: &[u8]| [&[2u8][..], s].concat();
    let data = |s: &[u8]| [&[1u8][..], s].concat();
    match n {
        0 => {
            for p in [
                &b"Foo.\n"[..],
                b"Hel",
                b"lo\n",
                b"a\rb\n",
                b"\n",
                b"x\r\n",
                b"\r",
                b"Counting: 1\rCounting: 2\rCounting: 3, done.\n",
            ] {
                packets.push(progress(p));
            }
            packets.push(data(pack));
            packets.push(progress(b"tail"));
        }
        1 => {
            packets.push(progress(b"one\ntwo\nthr"));
            packets.push(data(&pack[..20]));
            packets.push(progress(b"ee\n"));
            packets.push(data(&pack[20..]));
            packets.push(progress(b"after\n"));
        }
        _ => unreachable!(),
    }
    packets
}

#[test]
fn sideband_matches_git_fetch() {
    let Some(git_bin) = common::reference_git() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let repo = common::make_repo(&git_bin, home);
    let (oid, pack) = pack_of_main(&git_bin, home, &repo);

    let ours = home.join("rgit-trace");
    rgit_trace::TRACE_BARE.init_from(Some(OsStr::new("1")));
    rgit_trace::TRACE_PACKET.init_from(Some(ours.as_os_str()));

    let mut expected_trace = String::new();
    for n in 0..2 {
        let packets = scenario(n, &pack);
        let mut sideband = Vec::new();
        for p in &packets {
            sideband.extend(encode(p).unwrap());
        }
        sideband.extend_from_slice(b"0000");

        let mut adv = Vec::new();
        adv.extend(
            encode(format!("{oid} HEAD\0side-band-64k ofs-delta agent=foo/1\n").as_bytes())
                .unwrap(),
        );
        adv.extend(encode(format!("{oid} refs/heads/main\n").as_bytes()).unwrap());
        adv.extend_from_slice(b"0000");
        let adv_path = home.join(format!("adv-{n}"));
        std::fs::write(&adv_path, &adv).unwrap();
        let mut response = Vec::new();
        response.extend(encode(b"NAK\n").unwrap());
        response.extend_from_slice(&sideband);
        let response_path = home.join(format!("response-{n}"));
        std::fs::write(&response_path, &response).unwrap();
        // The scripted server answers only after it has read the client's
        // whole request (up to `done`), like a real upload-pack. Answering
        // up front and exiting races with the client's writes and makes
        // `git fetch` fail with EPIPE ("Broken pipe", #225). `read` consumes
        // a pipe byte by byte, so nothing past `done` is swallowed.
        let upload_pack = format!(
            "cat '{}'; while read -r l; do case \"$l\" in *done) break;; esac; done; cat '{}'; :",
            adv_path.display(),
            response_path.display()
        );

        // Reference.
        let clone = home.join(format!("clone-{n}"));
        let mut c = common::git(&git_bin, home);
        c.args(["init", "-q"]).arg(&clone);
        common::run(c);
        let ref_trace = home.join(format!("ref-trace-{n}"));
        let mut c = common::git(&git_bin, home);
        c.env("GIT_TRACE_PACKET", &ref_trace)
            .env("GIT_TRACE_BARE", "1")
            .arg("-C")
            .arg(&clone)
            .arg("fetch")
            .arg("-q")
            .arg(format!("--upload-pack={upload_pack}"))
            .arg(&repo)
            .arg("main");
        let out = common::run(c);
        let text = std::fs::read_to_string(&ref_trace).unwrap();
        for l in text
            .lines()
            .filter(|l| l.starts_with("packet:     sideband"))
        {
            expected_trace.push_str(l);
            expected_trace.push('\n');
        }

        // rgit.
        let reader = Reader::new(&sideband[..], "fetch");
        let mut sb = SidebandReader::new(reader, Progress::new(Vec::new(), ProgressStyle::Spaces));
        let mut data = Vec::new();
        while let Some(d) = sb.next_data().unwrap() {
            data.extend_from_slice(d);
        }
        let (_, progress) = sb.into_parts();
        assert_eq!(data, pack, "scenario {n}: pack data");
        assert_eq!(
            String::from_utf8_lossy(&progress.into_inner()),
            String::from_utf8_lossy(&out.stderr),
            "scenario {n}: stderr"
        );
    }
    assert!(expected_trace.contains("PACK ..."));
    let actual = std::fs::read_to_string(&ours).unwrap();
    assert_eq!(actual, expected_trace);
}
