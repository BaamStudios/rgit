// SPDX-License-Identifier: MIT
//! `upload_pack` with protocol v2 driven in memory (v0 and v1:
//! `upload_pack_v0.rs`): requests are byte strings, responses are read
//! back as pkt-lines. The repository is made with the reference
//! git; everything that needs a pack (and so a `pack-objects` process) is
//! tested through the binary in `rgit-cli/tests/upload_pack.rs`.

use std::path::Path;

use rgit_config::{ConfigSet, Origin, Scope, parse_bytes};
use rgit_difftest::Fixture;
use rgit_hash::HashKind;
use rgit_odb::{LooseOptions, ObjectDatabase};
use rgit_protocol::ProtocolVersion;
use rgit_refs::files::FilesStore;
use rgit_server::upload_pack::{Error, Options, Repository, upload_pack};

/// `srv`: three commits on `main`, branch `side` at the first, a
/// lightweight and an annotated tag, a symbolic reference.
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
        .git([
            "-C",
            "srv",
            "symbolic-ref",
            "refs/heads/alias",
            "refs/heads/side",
        ])
}

fn rev(fx: &Fixture, name: &str) -> String {
    String::from_utf8(fx.git_stdout(["-C", "srv", "rev-parse", name]))
        .unwrap()
        .trim()
        .to_owned()
}

/// Pack `lines` as pkt-lines; `0000` and `0001` stand for themselves.
fn request(lines: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for line in lines {
        match *line {
            "0000" | "0001" => out.extend_from_slice(line.as_bytes()),
            _ => out.extend_from_slice(format!("{:04x}{line}\n", line.len() + 5).as_bytes()),
        }
    }
    out
}

/// The packets of a response, one per line.
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

impl Served {
    fn fatal(&self) -> &str {
        match &self.result {
            Err(Error::Fatal(m)) => m,
            other => panic!("expected a fatal error, got {other:?}"),
        }
    }
}

fn serve(dir: &Path, config: &str, stateless: bool, input: &[u8]) -> Served {
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
    opts.protocol = ProtocolVersion::V2;
    opts.stateless_rpc = stateless;
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
    let served = serve(&dir, "", false, b"");
    served.result.unwrap();
    let agent = format!("agent={}\n", rgit_protocol::agent::agent());
    assert_eq!(
        served.packets,
        [
            "version 2\n",
            agent.as_str(),
            "ls-refs=unborn\n",
            "fetch=shallow wait-for-done\n",
            "server-option\n",
            "object-format=sha1\n",
            "0000",
        ]
    );

    let all = "[uploadpack]\nallowFilter = true\nallowRefInWant = true\n\
               allowSidebandAll = true\nadvertiseBundleURIs = true\n\
               [transfer]\nadvertiseSID = true\nadvertiseObjectInfo = true\n\
               [lsrefs]\nunborn = ignore\n";
    let served = serve(&dir, all, false, b"");
    let lines: Vec<&str> = served.packets.iter().map(String::as_str).collect();
    assert_eq!(lines[2], "ls-refs\n");
    assert_eq!(
        lines[3],
        "fetch=shallow wait-for-done filter ref-in-want sideband-all\n"
    );
    assert!(lines[6].starts_with("session-id="), "{lines:?}");
    assert_eq!(
        &lines[7..],
        ["object-info=size type\n", "bundle-uri\n", "0000"]
    );

    // One request and no advertisement with --stateless-rpc.
    let served = serve(&dir, "", true, b"");
    served.result.unwrap();
    assert!(served.packets.is_empty());
}

#[test]
fn ls_refs() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let dir = fx.work_dir();
    let (main, side) = (rev(&fx, "main"), rev(&fx, "side"));
    let (light, tag, peeled) = (rev(&fx, "light"), rev(&fx, "v1"), rev(&fx, "v1^{}"));

    let served = serve(&dir, "", true, &request(&["command=ls-refs", "0000"]));
    served.result.unwrap();
    assert_eq!(
        served.packets,
        [
            format!("{main} HEAD\n"),
            format!("{side} refs/heads/alias\n"),
            format!("{main} refs/heads/main\n"),
            format!("{side} refs/heads/side\n"),
            format!("{light} refs/tags/light\n"),
            format!("{tag} refs/tags/v1\n"),
            "0000".to_owned(),
        ]
    );

    let served = serve(
        &dir,
        "",
        true,
        &request(&[
            "command=ls-refs",
            "agent=test",
            "object-format=sha1",
            "0001",
            "peel",
            "symrefs",
            "ref-prefix HEAD",
            "ref-prefix refs/heads/a",
            "ref-prefix refs/tags/v",
            "0000",
        ]),
    );
    served.result.unwrap();
    assert_eq!(
        served.packets,
        [
            format!("{main} HEAD symref-target:refs/heads/main\n"),
            format!("{side} refs/heads/alias symref-target:refs/heads/side\n"),
            format!("{tag} refs/tags/v1 peeled:{peeled}\n"),
            "0000".to_owned(),
        ]
    );
}

#[test]
fn object_info() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let dir = fx.work_dir();
    let blob = rev(&fx, "main:one");
    let tree = rev(&fx, "main^{tree}");
    let unknown = "1".repeat(40);
    let on = "[transfer]\nadvertiseObjectInfo = true\n";
    let ask = |args: &[&str]| {
        let mut lines = vec!["command=object-info", "0001"];
        lines.extend_from_slice(args);
        lines.push("0000");
        serve(&dir, on, true, &request(&lines))
    };
    let (oid_blob, oid_tree, oid_unknown) = (
        format!("oid {blob}"),
        format!("oid {tree}"),
        format!("oid {unknown}"),
    );
    let served = ask(&["size", "type", &oid_blob, &oid_tree, &oid_unknown]);
    served.result.unwrap();
    assert_eq!(
        served.packets,
        [
            "size".to_owned(),
            "type".to_owned(),
            format!("{blob} 4 blob"),
            format!("{tree} 95 tree"),
            format!("{unknown} "),
            "0000".to_owned(),
        ]
    );
    assert_eq!(ask(&[&oid_blob]).packets, [blob.clone(), "0000".to_owned()]);
    // A bad request is answered, not fatal.
    let served = ask(&["bogus"]);
    served.result.unwrap();
    assert_eq!(
        served.packets,
        ["ERR object-info: unexpected line: 'bogus'", "0000"]
    );
    // Not a command unless it is advertised.
    let served = serve(&dir, "", true, &request(&["command=object-info", "0000"]));
    assert_eq!(served.fatal(), "invalid command 'object-info'");
}

#[test]
fn negotiation_without_pack() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let dir = fx.work_dir();
    let (main, side, tree) = (rev(&fx, "main"), rev(&fx, "side"), rev(&fx, "main^{tree}"));
    let unknown = "1".repeat(40);
    let fetch = |args: &[String]| {
        let mut lines = vec!["command=fetch", "0001"];
        lines.extend(args.iter().map(String::as_str));
        lines.push("0000");
        serve(&dir, "", true, &request(&lines))
    };

    // Nothing in common.
    let served = fetch(&[format!("want {main}"), format!("have {unknown}")]);
    served.result.unwrap();
    assert_eq!(served.packets, ["acknowledgments\n", "NAK\n", "0000"]);

    // A common commit that the wanted commit does not reach within one
    // step of what the client named: acknowledged, not ready.
    let served = fetch(&[
        format!("want {side}"),
        format!("have {main}"),
        format!("have {main}"),
        format!("have {unknown}"),
    ]);
    served.result.unwrap();
    assert_eq!(
        served.packets,
        [
            "acknowledgments\n".to_owned(),
            format!("ACK {main}\n"),
            "0000".to_owned()
        ]
    );

    // A common tree says nothing about history.
    let served = fetch(&[format!("want {main}"), format!("have {tree}")]);
    assert_eq!(
        served.packets,
        [
            "acknowledgments\n".to_owned(),
            format!("ACK {tree}\n"),
            "0000".to_owned()
        ]
    );

    // `wait-for-done` holds `ready` back.
    let served = fetch(&[
        format!("want {main}"),
        format!("have {side}"),
        "wait-for-done".to_owned(),
    ]);
    assert_eq!(
        served.packets,
        [
            "acknowledgments\n".to_owned(),
            format!("ACK {side}\n"),
            "0000".to_owned()
        ]
    );

    // Without wants there is no answer at all.
    let served = fetch(&[format!("have {main}")]);
    served.result.unwrap();
    assert!(served.packets.is_empty());
}

#[test]
fn request_errors() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let dir = fx.work_dir();
    let main = rev(&fx, "main");
    let unknown = "1".repeat(40);
    let fatal = |config: &str, lines: &[&str]| {
        let served = serve(&dir, config, true, &request(lines));
        (served.fatal().to_owned(), served.packets)
    };
    let none: Vec<String> = Vec::new();

    assert_eq!(
        fatal("", &["foobar", "0000"]),
        ("unknown capability 'foobar'".into(), none.clone())
    );
    assert_eq!(fatal("", &["agent=x", "0000"]).0, "no command requested");
    assert_eq!(
        fatal("", &["command=ls-refs", "command=fetch", "0000"]).0,
        "command 'fetch' requested after already requesting command 'ls-refs'"
    );
    assert_eq!(
        fatal("", &["command=ls-refs", "fetch=shallow", "0000"]).0,
        "unknown capability 'fetch=shallow'"
    );
    assert_eq!(
        fatal("", &["command=ls-refs", "session-id=x", "0000"]).0,
        "unknown capability 'session-id=x'"
    );
    assert_eq!(
        fatal("", &["command=ls-refs", "object-format", "0000"]).0,
        "object-format capability requires an argument"
    );
    assert_eq!(
        fatal("", &["command=fetch", "object-format=sha256", "0000"]).0,
        "mismatched object format: server sha1; client sha256"
    );
    assert_eq!(
        fatal("", &["command=ls-refs", "0001", "no-such-arg", "0000"]).0,
        "unexpected line: 'no-such-arg'"
    );
    assert_eq!(
        fatal("", &["command=ls-refs", "0001", "0001"]).0,
        "expected flush after ls-refs arguments"
    );
    assert_eq!(
        fatal("", &["command=fetch", "0001", "0001"]).0,
        "expected flush after fetch arguments"
    );
    // Features that are switched off are unknown arguments.
    for arg in [
        "filter blob:none",
        "want-ref refs/heads/main",
        "sideband-all",
    ] {
        assert_eq!(
            fatal("", &["command=fetch", "0001", arg, "0000"]).0,
            format!("unexpected line: '{arg}'")
        );
    }
    // The client is told what it asked for wrongly.
    let want_unknown = format!("want {unknown}");
    assert_eq!(
        fatal("", &["command=fetch", "0001", &want_unknown, "0000"]),
        (
            format!("git upload-pack: not our ref {unknown}"),
            vec![format!("ERR upload-pack: not our ref {unknown}")]
        )
    );
    assert_eq!(
        fatal(
            "[uploadpack]\nallowRefInWant = true\n",
            &["command=fetch", "0001", "want-ref refs/heads/nope", "0000"]
        ),
        (
            "unknown ref refs/heads/nope".to_owned(),
            vec!["ERR unknown ref refs/heads/nope".to_owned()]
        )
    );
    let want = format!("want {main}");
    assert_eq!(
        fatal(
            "[uploadpack]\nallowFilter = true\n[uploadpackfilter \"blob:none\"]\nallow = false\n",
            &[
                "command=fetch",
                "0001",
                &want,
                "filter blob:none",
                "done",
                "0000"
            ]
        ),
        (
            "filter 'blob:none' not supported".to_owned(),
            vec!["ERR filter 'blob:none' not supported".to_owned()]
        )
    );
    assert_eq!(
        fatal(
            "[uploadpack]\nallowFilter = true\n[uploadpackfilter \"tree\"]\nmaxDepth = 1\n",
            &[
                "command=fetch",
                "0001",
                &want,
                "filter tree:3",
                "done",
                "0000"
            ]
        )
        .0,
        "tree filter allows max depth 1, but got 3"
    );
    for (arg, message) in [
        (
            "want xyz",
            "git upload-pack: protocol error, expected to get oid, not 'want xyz'",
        ),
        (
            "have xyz",
            "git upload-pack: expected SHA1 object, got 'xyz'",
        ),
        ("shallow xyz", "invalid shallow line: shallow xyz"),
        ("deepen 0", "Invalid deepen: deepen 0"),
        ("deepen-since x", "Invalid deepen-since: deepen-since x"),
        (
            "deepen-not nope",
            "git upload-pack: deepen-not is not a ref: deepen-not nope",
        ),
    ] {
        assert_eq!(
            fatal("", &["command=fetch", "0001", &want, arg, "0000"]).0,
            message
        );
    }
}

#[test]
fn several_requests_on_one_connection() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let dir = fx.work_dir();
    let side = rev(&fx, "side");
    let mut input = request(&["command=ls-refs", "0001", "ref-prefix refs/heads/s", "0000"]);
    input.extend(request(&[
        "command=ls-refs",
        "0001",
        "ref-prefix refs/x",
        "0000",
    ]));
    // A lone flush packet ends the conversation; what follows is not read.
    input.extend(request(&["0000", "command=bogus", "0000"]));
    let served = serve(&dir, "", false, &input);
    served.result.unwrap();
    let after_advertisement = served.packets.iter().position(|p| p == "0000").unwrap() + 1;
    assert_eq!(
        served.packets[after_advertisement..],
        [
            format!("{side} refs/heads/side\n"),
            "0000".to_owned(),
            "0000".to_owned()
        ]
    );
}
