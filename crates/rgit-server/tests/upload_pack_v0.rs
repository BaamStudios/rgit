// SPDX-License-Identifier: MIT
//! `upload_pack` with protocol v0 and v1, driven in memory: requests are
//! byte strings, responses are read back as pkt-lines. Everything that
//! needs a pack (and so a `pack-objects` process), and the comparison
//! with git, is in `rgit-cli/tests/upload_pack_v0.rs`.

use std::path::Path;

use rgit_config::{ConfigSet, Origin, Scope, parse_bytes};
use rgit_difftest::Fixture;
use rgit_hash::HashKind;
use rgit_odb::{LooseOptions, ObjectDatabase};
use rgit_protocol::ProtocolVersion;
use rgit_refs::files::FilesStore;
use rgit_server::upload_pack::{Error, Options, Repository, upload_pack};

/// `srv`: three commits on `main`, branch `side` at the first, a
/// lightweight and an annotated tag at the second.
fn fixture() -> Fixture {
    let mut fx = Fixture::new().git(["init", "-q", "srv"]);
    for name in ["one", "two", "three"] {
        fx = fx
            .write(format!("srv/{name}"), format!("{name}\n"))
            .git(["-C", "srv", "add", name])
            .git(["-C", "srv", "commit", "-q", "-m", name])
            .tick();
    }
    fx.git(["-C", "srv", "branch", "side", "HEAD~2"])
        .git(["-C", "srv", "tag", "light", "HEAD~1"])
        .git(["-C", "srv", "tag", "-a", "-m", "annotated", "v1", "HEAD~1"])
}

fn rev(fx: &Fixture, name: &str) -> String {
    String::from_utf8(fx.git_stdout(["-C", "srv", "rev-parse", name]))
        .unwrap()
        .trim()
        .to_owned()
}

/// Pack `lines` as pkt-lines; `0000` stands for itself.
fn request(lines: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for line in lines {
        match *line {
            "0000" => out.extend_from_slice(line.as_bytes()),
            _ => out.extend_from_slice(format!("{:04x}{line}\n", line.len() + 5).as_bytes()),
        }
    }
    out
}

/// The packets of a response.
fn packets(mut data: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    while data.len() >= 4 {
        let len = usize::from_str_radix(std::str::from_utf8(&data[..4]).unwrap(), 16).unwrap();
        if len < 4 {
            out.push(String::from_utf8_lossy(&data[..4]).into_owned());
            data = &data[4..];
            continue;
        }
        out.push(String::from_utf8_lossy(&data[4..len]).into_owned());
        data = &data[len..];
    }
    assert!(data.is_empty(), "trailing bytes");
    out
}

struct Served {
    packets: Vec<String>,
    result: Result<(), Error>,
}

/// How to serve.
#[derive(Clone, Copy)]
struct Mode {
    protocol: ProtocolVersion,
    stateless: bool,
    advertise: bool,
}

const V0: Mode = Mode {
    protocol: ProtocolVersion::V0,
    stateless: false,
    advertise: false,
};
const STATELESS: Mode = Mode {
    stateless: true,
    ..V0
};

fn serve(dir: &Path, config: &str, mode: Mode, input: &[u8]) -> Served {
    let git_dir = dir.join("srv/.git");
    let odb = ObjectDatabase::open_with_packs(
        git_dir.join("objects"),
        HashKind::Sha1,
        LooseOptions::default(),
    );
    let refs = FilesStore::new(&git_dir, HashKind::Sha1);
    let config: ConfigSet = parse_bytes(config.as_bytes(), Origin::CommandLine, Scope::Command)
        .expect("test configuration");
    let repo = Repository {
        hash: HashKind::Sha1,
        odb: &odb,
        refs: &refs,
        config: &config,
        shallow: Vec::new(),
    };
    let mut opts = Options::new(vec!["git".into()]);
    opts.protocol = mode.protocol;
    opts.stateless_rpc = mode.stateless;
    opts.advertise_refs = mode.advertise;
    let mut output = Vec::new();
    let result = upload_pack(&repo, &opts, input, &mut output);
    Served {
        packets: packets(&output),
        result,
    }
}

#[test]
fn advertisement() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let dir = fx.work_dir();
    let (main, side) = (rev(&fx, "main"), rev(&fx, "side"));
    let (light, v1, v1_commit) = (rev(&fx, "light"), rev(&fx, "v1"), rev(&fx, "v1^{}"));
    let agent = rgit_protocol::agent::agent();
    let caps = "multi_ack thin-pack side-band side-band-64k ofs-delta shallow deepen-since \
                deepen-not deepen-relative no-progress include-tag multi_ack_detailed";
    let refs = [
        format!("{main} refs/heads/main\n"),
        format!("{side} refs/heads/side\n"),
        format!("{light} refs/tags/light\n"),
        format!("{v1} refs/tags/v1\n"),
        format!("{v1_commit} refs/tags/v1^{{}}\n"),
        "0000".to_owned(),
    ];
    let tail = format!("symref=HEAD:refs/heads/main object-format=sha1 agent={agent}\n");

    let served = serve(&dir, "", V0, b"0000");
    served.result.unwrap();
    assert_eq!(served.packets[0], format!("{main} HEAD\0{caps} {tail}"));
    assert_eq!(served.packets[1..], refs);

    // Protocol v1 says so first.
    let v1_mode = Mode {
        protocol: ProtocolVersion::V1,
        ..V0
    };
    let served = serve(&dir, "", v1_mode, b"0000");
    assert_eq!(served.packets[0], "version 1\n");
    assert_eq!(served.packets[1], format!("{main} HEAD\0{caps} {tail}"));

    // `--advertise-refs` reads nothing and offers `no-done`; what the
    // configuration allows is advertised.
    let advertise = Mode {
        advertise: true,
        ..STATELESS
    };
    let config = "[uploadpack]\nallowFilter = true\nallowAnySHA1InWant = true\n";
    let served = serve(&dir, config, advertise, b"");
    served.result.unwrap();
    assert_eq!(
        served.packets[0],
        format!(
            "{main} HEAD\0{caps} allow-tip-sha1-in-want allow-reachable-sha1-in-want no-done \
             symref=HEAD:refs/heads/main filter object-format=sha1 agent={agent}\n"
        )
    );
    // A stateless request has no advertisement.
    let served = serve(&dir, "", STATELESS, b"0000");
    served.result.unwrap();
    assert_eq!(served.packets, [""; 0]);

    // Hidden references; with nothing left the capabilities still go
    // out, and `HEAD` stays a symbolic reference for the client.
    let config = "[transfer]\nhideRefs = refs/tags\n[uploadpack]\nhideRefs = refs/heads/side\n";
    let served = serve(&dir, config, V0, b"0000");
    assert_eq!(
        served.packets[1..],
        [format!("{main} refs/heads/main\n"), "0000".to_owned()]
    );
    let config = "[transfer]\nhideRefs = refs\nhideRefs = HEAD\n";
    let served = serve(&dir, config, V0, b"0000");
    let zero = "0".repeat(40);
    assert_eq!(
        served.packets,
        [
            format!("{zero} capabilities^{{}}\0{caps} {tail}"),
            "0000".to_owned()
        ]
    );
}

#[test]
fn wants() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let dir = fx.work_dir();
    let main = rev(&fx, "main");
    let (root_tree, blob) = (rev(&fx, "main^{tree}"), rev(&fx, "main:one"));
    let zero = "0".repeat(40);

    // The connection ends before the request does.
    for (mode, lines) in [
        (STATELESS, &[][..]),
        (STATELESS, &[&format!("want {main}")[..]]),
        (V0, &[&format!("want {main}"), "0000"]),
        (
            V0,
            &[&format!("want {main}"), "0000", &format!("have {main}")],
        ),
        (
            STATELESS,
            &[&format!("want {main}"), "0000", &format!("have {main}")],
        ),
    ] {
        let served = serve(&dir, "", mode, &request(lines));
        match served.result {
            Err(Error::Fatal(m)) => assert_eq!(m, "the remote end hung up unexpectedly"),
            other => panic!("{lines:?}: {other:?}"),
        }
    }
    // A stateless request may end with the `want` list.
    let served = serve(
        &dir,
        "",
        STATELESS,
        &request(&[&format!("want {main}"), "0000"]),
    );
    served.result.unwrap();
    assert_eq!(served.packets, [""; 0]);
    // An object that is not there: the client is told, the error is
    // fatal.
    let served = serve(
        &dir,
        "",
        STATELESS,
        &request(&[&format!("want {zero}"), "0000"]),
    );
    assert_eq!(
        served.packets,
        [format!("ERR upload-pack: not our ref {zero}")]
    );
    assert!(
        matches!(&served.result, Err(Error::Fatal(m)) if *m == format!("git upload-pack: not our ref {zero}"))
    );
    // An object that is no tip is refused (an `error:` for the caller).
    for id in [&root_tree, &blob] {
        let input = request(&[&format!("want {id}"), "0000", "done"]);
        let served = serve(&dir, "", V0, &input);
        let answer = served.packets.last().unwrap();
        assert_eq!(*answer, format!("ERR upload-pack: not our ref {id}"));
        assert!(
            matches!(&served.result, Err(Error::Refused(m)) if *m == format!("git upload-pack: not our ref {id}"))
        );
    }
    // Lines that are no request.
    for (line, message) in [
        (
            "foo",
            "git upload-pack: protocol error, expected to get object ID, not 'foo'",
        ),
        (
            "want xyz",
            "git upload-pack: protocol error, expected to get object ID, not 'want xyz'",
        ),
        ("deepen 0", "Invalid deepen: deepen 0"),
        ("deepen-since x", "Invalid deepen-since: deepen-since x"),
        (
            "deepen-not nope",
            "git upload-pack: deepen-not is not a ref: deepen-not nope",
        ),
        ("shallow xyz", "invalid shallow line: shallow xyz"),
        (
            "filter blob:none",
            "git upload-pack: filtering capability not negotiated",
        ),
    ] {
        let input = request(&[&format!("want {main}"), line, "0000"]);
        let served = serve(&dir, "", STATELESS, &input);
        match served.result {
            Err(Error::Fatal(m)) => assert_eq!(m, message),
            other => panic!("{line}: {other:?}"),
        }
        assert_eq!(served.packets, [""; 0], "{line}");
    }
}

#[test]
fn negotiation() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let dir = fx.work_dir();
    let (main, second, first) = (rev(&fx, "main"), rev(&fx, "main~1"), rev(&fx, "main~2"));
    let tree = rev(&fx, "main^{tree}");
    let unknown = "1".repeat(40);
    // A stateless request without `done` ends after the acknowledgments:
    // no pack, no `pack-objects`.
    let round = |caps: &str, haves: &[&str]| {
        let want = format!("want {main} {caps}");
        let haves: Vec<String> = haves.iter().map(|h| format!("have {h}")).collect();
        let mut lines = vec![want.as_str(), "0000"];
        lines.extend(haves.iter().map(String::as_str));
        lines.push("0000");
        let served = serve(&dir, "", STATELESS, &request(&lines));
        served.result.unwrap();
        served.packets
    };

    // Without `multi_ack`: the first common object only; its parent
    // counts as the same find.
    assert_eq!(round("", &[&unknown]), ["NAK\n"]);
    assert_eq!(
        round("", &[&second, &first]),
        [format!("ACK {second}\n"), format!("ACK {first}\n")]
    );
    assert_eq!(round("", &[&first, &second]), [format!("ACK {first}\n")]);
    // `multi_ack`: every common object, and unknown ones once the pack
    // can be made.
    assert_eq!(
        round("multi_ack", &[&unknown, &first, &unknown, &tree]),
        [
            format!("ACK {first} continue\n"),
            format!("ACK {unknown} continue\n"),
            format!("ACK {tree} continue\n"),
            "NAK\n".to_owned()
        ]
    );
    // `multi_ack_detailed`: `ready` when a round had common objects only.
    assert_eq!(
        round("multi_ack_detailed", &[&first, &second]),
        [
            format!("ACK {first} common\n"),
            format!("ACK {second} common\n"),
            format!("ACK {second} ready\n"),
            "NAK\n".to_owned()
        ]
    );
    assert_eq!(
        round("multi_ack_detailed", &[&unknown, &first]),
        [format!("ACK {first} common\n"), "NAK\n".to_owned()]
    );
    assert_eq!(
        round("multi_ack_detailed", &[&first, &unknown]),
        [
            format!("ACK {first} common\n"),
            format!("ACK {unknown} ready\n"),
            "NAK\n".to_owned()
        ]
    );
    // A tree tells nothing about history.
    assert_eq!(
        round("multi_ack_detailed", &[&tree]),
        [format!("ACK {tree} common\n"), "NAK\n".to_owned()]
    );

    // A depth request is answered before the negotiation; a stateless
    // client may leave it at that.
    let input = request(&[&format!("want {main}"), "deepen 1", "0000"]);
    let served = serve(&dir, "", STATELESS, &input);
    assert_eq!(
        served.packets,
        [format!("shallow {main}"), "0000".to_owned()]
    );
    served.result.unwrap();
    let served = serve(&dir, "", V0, &input);
    assert_eq!(
        served.packets[served.packets.len() - 2..],
        [format!("shallow {main}"), "0000".to_owned()]
    );
    assert!(matches!(served.result, Err(Error::Fatal(_))));
    let input = request(&[
        &format!("want {main}"),
        &format!("shallow {second}"),
        "deepen 4",
        "0000",
    ]);
    let served = serve(&dir, "", STATELESS, &input);
    assert_eq!(
        served.packets,
        [format!("unshallow {second}"), "0000".to_owned()]
    );
    // A line that is neither `have` nor `done`.
    let input = request(&[&format!("want {main}"), "0000", "junk"]);
    let served = serve(&dir, "", STATELESS, &input);
    assert!(
        matches!(&served.result, Err(Error::Fatal(m)) if m == "git upload-pack: expected SHA1 list, got 'junk'")
    );
}
