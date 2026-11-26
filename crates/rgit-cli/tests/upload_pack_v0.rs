// SPDX-License-Identifier: MIT
//! `upload-pack` with protocol v0 and v1 against the reference git.
//!
//! - Wire level: the same requests are sent to `git upload-pack` and
//!   `rgit upload-pack`; the packets, standard error (without progress
//!   meters) and the exit code must be equal (apart from `agent=` and the
//!   session id), and the packs must hold the same objects.
//! - Clients: the reference git clones and fetches with
//!   `protocol.version` 0 and 1 through both servers, over `file://` and
//!   smart HTTP (`--stateless-rpc`); the resulting repositories must be
//!   equal and pass `git fsck --strict`.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use rgit_difftest::Fixture;

mod common;
use common::servers::{isolated, start_http_with};

fn rgit() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rgit"))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn commit(fx: Fixture, dir: &str, file: &str, content: &str) -> Fixture {
    fx.write(format!("{dir}/{file}"), content)
        .git(["-C", dir, "add", file])
        .git(["-C", dir, "commit", "-q", "-m", file])
        .tick()
}

fn rev(fx: &Fixture, repo: &str, name: &str) -> String {
    text(&fx.git_stdout(["-C", repo, "rev-parse", name]))
        .trim()
        .to_owned()
}

/// `srv`: six commits on `main`, a branch `side` at the first commit
/// with a commit of its own, annotated tags `v1` (third commit) and `v2`
/// (tip), a lightweight tag.
fn fixture() -> Fixture {
    let fx = Fixture::new().rgit(rgit()).git(["init", "-q", "srv"]);
    if fx.is_skipped() {
        return fx;
    }
    let fx = commit(fx, "srv", "a/b/c/deep", "deep\n");
    let fx = fx.git(["-C", "srv", "branch", "side"]);
    let fx = commit(fx, "srv", "a/b/mid", "mid\n");
    let fx = commit(fx, "srv", "top", "top\n").git(["-C", "srv", "tag", "-a", "-m", "one", "v1"]);
    let big = "0123456789abcdef\n".repeat(40);
    let fx = commit(fx, "srv", "big", &big).git(["-C", "srv", "tag", "light"]);
    let fx = commit(fx, "srv", "a/other", "other\n");
    let fx = commit(fx, "srv", "last", "last\n").git(["-C", "srv", "tag", "-a", "-m", "two", "v2"]);
    let fx = fx.git(["-C", "srv", "checkout", "-q", "side"]);
    commit(fx, "srv", "s", "side\n").git(["-C", "srv", "checkout", "-q", "main"])
}

/// Run `program` in `dir` with the fixture's isolated environment.
fn run(
    fx: &Fixture,
    program: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    dir: &Path,
    stdin: &[u8],
) -> Output {
    let mut cmd = Command::new(program);
    isolated(fx, &mut cmd);
    cmd.args(args)
        .envs(env.iter().copied())
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let mut pipe = child.stdin.take().unwrap();
    let input = stdin.to_vec();
    let feeder = std::thread::spawn(move || {
        let _ = pipe.write_all(&input);
    });
    let out = child.wait_with_output().unwrap();
    let _ = feeder.join();
    out
}

// ---------------------------------------------------------------- wire level

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

/// What a server answered: the packets as text (`agent` and session id
/// made equal, progress left out), the objects of the pack, standard
/// error without progress, and the exit code.
#[derive(Debug, PartialEq, Eq)]
struct Reply {
    packets: Vec<String>,
    pack: Option<BTreeSet<String>>,
    stderr: String,
    code: Option<i32>,
}

fn normalize(line: &str) -> String {
    let mut out = String::new();
    for (i, word) in line.split(' ').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let newline = if word.ends_with('\n') { "\n" } else { "" };
        match ["agent=", "session-id="]
            .iter()
            .find(|k| word.starts_with(**k))
        {
            Some(key) => out.push_str(&format!("{key}X{newline}")),
            None => out.push_str(word),
        }
    }
    out
}

fn is_progress(line: &str) -> bool {
    let line = line.trim();
    line.is_empty()
        || [
            "Enumerating objects",
            "Counting objects",
            "Compressing objects",
            "Delta compression",
            "Total ",
        ]
        .iter()
        .any(|p| line.starts_with(p))
}

fn without_progress(bytes: &[u8]) -> String {
    text(bytes)
        .split(['\r', '\n'])
        .filter(|l| !is_progress(l))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The ids of the objects in a (non-thin) pack.
fn pack_objects(fx: &Fixture, pack: &[u8]) -> BTreeSet<String> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("p.pack"), pack).unwrap();
    let git = fx.reference_git();
    let indexed = run(fx, git, &["index-pack", "p.pack"], &[], dir.path(), b"");
    assert!(
        indexed.status.success(),
        "the pack is broken: {}",
        text(&indexed.stderr)
    );
    let idx = std::fs::read(dir.path().join("p.idx")).unwrap();
    let shown = run(fx, git, &["show-index"], &[], dir.path(), &idx);
    text(&shown.stdout)
        .lines()
        .filter_map(|l| l.split(' ').nth(1).map(str::to_owned))
        .collect()
}

fn decode(fx: &Fixture, out: &Output) -> Reply {
    let mut data = out.stdout.as_slice();
    let mut packets = Vec::new();
    let mut pack: Vec<u8> = Vec::new();
    let mut progress: Vec<u8> = Vec::new();
    // After the first ACK or NAK the pack may follow, on a sideband or
    // as it is.
    let mut negotiated = false;
    while !data.is_empty() {
        if negotiated && data.starts_with(b"PACK") {
            pack.extend_from_slice(data);
            break;
        }
        let len = data
            .get(..4)
            .and_then(|l| usize::from_str_radix(&text(l), 16).ok())
            .filter(|len| *len <= data.len() && (*len == 0 || *len >= 4));
        let Some(len) = len else {
            packets.push(format!("GARBAGE {}", text(data)));
            break;
        };
        if len == 0 {
            packets.push("0000".to_owned());
            data = &data[4..];
            continue;
        }
        let payload = &data[4..len];
        data = &data[len..];
        match payload.first() {
            Some(1) if negotiated => pack.extend_from_slice(&payload[1..]),
            Some(2) if negotiated => progress.extend_from_slice(&payload[1..]),
            Some(3) if negotiated => packets.push(format!("band 3: {}", text(&payload[1..]))),
            _ => {
                let line = text(payload);
                negotiated |= line.starts_with("ACK ") || line.starts_with("NAK");
                packets.push(normalize(&line));
            }
        }
    }
    let other = without_progress(&progress);
    if !other.is_empty() {
        packets.push(format!("band 2: {other}"));
    }
    Reply {
        packets,
        pack: (!pack.is_empty()).then(|| pack_objects(fx, &pack)),
        stderr: without_progress(&out.stderr),
        code: out.status.code(),
    }
}

/// The packets after the advertisement.
fn answer(reply: &Reply) -> &[String] {
    let end = reply.packets.iter().position(|p| p == "0000");
    &reply.packets[end.map_or(0, |i| i + 1)..]
}

/// How to call `upload-pack`.
#[derive(Clone, Copy, Default)]
struct Call<'a> {
    config: &'a [&'a str],
    flags: &'a [&'a str],
    env: &'a [(&'a str, &'a str)],
    repo: &'a str,
}

fn serve(fx: &Fixture, program: &Path, call: Call<'_>, lines: &[&str]) -> Reply {
    let mut args: Vec<&str> = Vec::new();
    for c in call.config {
        args.extend(["-c", c]);
    }
    args.push("upload-pack");
    args.extend_from_slice(call.flags);
    args.push(call.repo);
    let out = run(
        fx,
        program,
        &args,
        call.env,
        &fx.work_dir(),
        &request(lines),
    );
    decode(fx, &out)
}

/// The request must be answered alike by git and rgit.
#[track_caller]
fn same(fx: &Fixture, call: Call<'_>, lines: &[&str]) -> Reply {
    let git = serve(fx, fx.reference_git(), call, lines);
    let ours = serve(fx, &rgit(), call, lines);
    assert_eq!(
        git, ours,
        "request {lines:?} with {:?} {:?} {:?} on {}",
        call.config, call.flags, call.env, call.repo
    );
    ours
}

/// `srv` and repositories made from it: `empty.git`, `det.git` (detached
/// `HEAD`), `dang.git` (`HEAD` names no branch), `sym.git` (symbolic
/// references), `ns.git` (namespaces `foo` and `foo/bar`), `sh.git`
/// (shallow), `hid.git` (a commit and a tag only `refs/hidden/*` reach).
fn repositories() -> Fixture {
    let fx = fixture();
    if fx.is_skipped() {
        return fx;
    }
    let url = format!("file://{}/srv", fx.work_dir().display());
    let (c1, c3, c4) = (
        rev(&fx, "srv", "main~4"),
        rev(&fx, "srv", "main~2"),
        rev(&fx, "srv", "main~1"),
    );
    let fx = fx
        .git(["init", "-q", "--bare", "empty.git"])
        .git(["clone", "-q", "--bare", "srv", "det.git"])
        .git(["-C", "det.git", "update-ref", "--no-deref", "HEAD", &c4])
        .git(["clone", "-q", "--bare", "srv", "dang.git"])
        .git(["-C", "dang.git", "symbolic-ref", "HEAD", "refs/heads/nope"])
        .git(["clone", "-q", "--bare", "srv", "sym.git"])
        .git([
            "-C",
            "sym.git",
            "symbolic-ref",
            "refs/heads/alias",
            "refs/heads/side",
        ])
        .git(["clone", "-q", "--bare", "srv", "ns.git"])
        .git([
            "-C",
            "ns.git",
            "update-ref",
            "refs/namespaces/foo/refs/heads/nsmain",
            &c3,
        ])
        .git([
            "-C",
            "ns.git",
            "symbolic-ref",
            "refs/namespaces/foo/HEAD",
            "refs/namespaces/foo/refs/heads/nsmain",
        ])
        .git([
            "-C",
            "ns.git",
            "update-ref",
            "refs/namespaces/foo/refs/namespaces/bar/refs/heads/deep",
            &c1,
        ])
        .git(["clone", "-q", "--bare", "--depth", "2", &url, "sh.git"])
        .git(["clone", "-q", "--bare", "srv", "hid.git"]);
    let tree = rev(&fx, "hid.git", "main^{tree}");
    let make = |parent: &str, message: &str| {
        text(&fx.git_stdout([
            "-C",
            "hid.git",
            "commit-tree",
            "-m",
            message,
            "-p",
            parent,
            &tree,
        ]))
        .trim()
        .to_owned()
    };
    let inner = make(&c1, "hidden");
    let tip = make(&inner, "hidden tip");
    fx.git([
        "-C", "hid.git", "tag", "-a", "-m", "hidden", "hidtag", &inner,
    ])
    .git(["-C", "hid.git", "update-ref", "refs/hidden/y", &tip])
    .git([
        "-C",
        "hid.git",
        "update-ref",
        "refs/hidden/t",
        "refs/tags/hidtag",
    ])
    .git(["-C", "hid.git", "update-ref", "-d", "refs/tags/hidtag"])
}

#[test]
fn advertisement_equals_git() {
    let fx = repositories();
    if fx.is_skipped() {
        return;
    }
    let configs: &[&[&str]] = &[
        &[],
        &["uploadpack.allowTipSHA1InWant=true"],
        &["uploadpack.allowReachableSHA1InWant=true"],
        &["uploadpack.allowAnySHA1InWant=true"],
        &["uploadpack.allowFilter=true"],
        &["transfer.advertiseSID=true"],
        &[
            "uploadpack.allowFilter=true",
            "transfer.advertiseSID=true",
            "uploadpack.allowAnySHA1InWant=true",
        ],
        &[
            "transfer.hideRefs=refs/tags",
            "uploadpack.hideRefs=refs/heads/side",
        ],
        &["transfer.hideRefs=HEAD"],
        &["transfer.hideRefs=refs", "transfer.hideRefs=HEAD"],
        &[
            "transfer.hideRefs=refs/tags",
            "transfer.hideRefs=!refs/tags/v1",
        ],
        // Not upload-pack's business.
        &["receive.hideRefs=refs/heads"],
    ];
    let flags: &[&[&str]] = &[
        &[],
        &["--advertise-refs"],
        &["--http-backend-info-refs"],
        &["--stateless-rpc", "--advertise-refs"],
    ];
    let protocols: &[&[(&str, &str)]] = &[
        &[],
        &[("GIT_PROTOCOL", "version=1")],
        &[("GIT_PROTOCOL", "version=1:foo=bar")],
        &[("GIT_PROTOCOL", "version=7")],
        &[("GIT_PROTOCOL", "version=0:version=1")],
    ];
    for config in configs {
        for flags in flags {
            for env in protocols {
                let call = Call {
                    config,
                    flags,
                    env,
                    repo: "srv",
                };
                let reply = same(&fx, call, &["0000"]);
                assert_eq!(reply.code, Some(0));
            }
        }
    }
    for repo in [
        "empty.git",
        "det.git",
        "dang.git",
        "sym.git",
        "sh.git",
        "hid.git",
    ] {
        for flags in flags {
            for env in &protocols[..2] {
                let call = Call {
                    config: &[],
                    flags,
                    env,
                    repo,
                };
                same(&fx, call, &["0000"]);
            }
        }
    }
    for namespace in ["foo", "foo/bar", "nope"] {
        let call = Call {
            env: &[("GIT_NAMESPACE", namespace)],
            repo: "ns.git",
            ..Call::default()
        };
        same(&fx, call, &["0000"]);
    }
    // Without a namespace its references are ordinary ones.
    let all = same(
        &fx,
        Call {
            repo: "ns.git",
            ..Call::default()
        },
        &["0000"],
    );
    assert!(
        all.packets
            .iter()
            .any(|p| p.contains("refs/namespaces/foo/HEAD"))
    );
    let first = same(
        &fx,
        Call {
            repo: "srv",
            ..Call::default()
        },
        &["0000"],
    );
    assert!(
        first.packets[0].contains(" HEAD\0multi_ack thin-pack side-band side-band-64k ofs-delta")
            && first.packets[0].contains(" symref=HEAD:refs/heads/main "),
        "{:?}",
        first.packets[0]
    );
}

#[test]
fn negotiation_equals_git() {
    let fx = repositories();
    if fx.is_skipped() {
        return;
    }
    let id = |name: &str| rev(&fx, "srv", name);
    let (h, h1, h2, h3, h4) = (
        id("main"),
        id("main~1"),
        id("main~2"),
        id("main~3"),
        id("main~4"),
    );
    let (side, tree, blob, v1) = (id("side"), id("main^{tree}"), id("main:top"), id("v1"));
    let z = "0".repeat(40);
    let y = "1".repeat(40);
    let want = |caps: &str| format!("want {h} {caps}");
    let have = |id: &str| format!("have {id}");
    let srv = Call {
        repo: "srv",
        ..Call::default()
    };
    let check = |call: Call<'_>, lines: &[String]| {
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        same(&fx, call, &lines)
    };
    let s = |l: &str| l.to_owned();
    let flush = || s("0000");
    let done = || s("done");

    // The client is done before it wanted anything.
    assert_eq!(check(srv, &[flush()]).code, Some(0));
    assert_eq!(check(srv, &[]).code, Some(128));
    assert_eq!(check(srv, &[want("")]).code, Some(128));
    assert_eq!(check(srv, &[want(""), flush()]).code, Some(128));

    // A clone, with and without sidebands.
    for caps in [
        "",
        "side-band-64k",
        "side-band",
        "side-band-64k no-progress",
        "side-band no-progress ofs-delta",
        "side-band-64k include-tag agent=foo/1.0 session-id=abc",
        "no-progress",
        "multi_ack",
        "multi_ack_detailed side-band-64k",
        "foo-bar object-format=sha256",
    ] {
        let reply = check(srv, &[want(caps), flush(), done()]);
        assert_eq!(reply.code, Some(0), "{caps}");
        assert_eq!(reply.pack.map(|p| p.len() > 10), Some(true), "{caps}");
    }
    // Capabilities count on any `want` line, also right behind the id.
    check(
        srv,
        &[
            want(""),
            format!("want {v1} side-band-64k no-progress"),
            flush(),
            done(),
        ],
    );
    check(
        srv,
        &[
            format!("want {h}side-band-64k no-progress"),
            flush(),
            done(),
        ],
    );
    check(srv, &[want(""), want(""), flush(), done()]);

    // Acknowledgments in the three modes.
    for mode in ["", "multi_ack", "multi_ack_detailed"] {
        let w = || want(mode);
        // Common commits, one of them the parent of the other.
        check(srv, &[w(), flush(), have(&h3), have(&h4), flush(), done()]);
        check(
            srv,
            &[
                w(),
                flush(),
                have(&h3),
                have(&h4),
                have(&h3),
                flush(),
                done(),
            ],
        );
        // Unknown first, then common ones in later rounds.
        check(
            srv,
            &[
                w(),
                flush(),
                have(&z),
                flush(),
                have(&h3),
                flush(),
                have(&h2),
                flush(),
                done(),
            ],
        );
        check(
            srv,
            &[
                w(),
                flush(),
                have(&z),
                have(&h3),
                have(&h4),
                flush(),
                have(&h2),
                flush(),
                done(),
            ],
        );
        // Unknown objects once the pack can be made.
        check(
            srv,
            &[
                w(),
                flush(),
                have(&h3),
                have(&z),
                have(&y),
                flush(),
                have(&y),
                flush(),
                have(&h2),
                flush(),
                done(),
            ],
        );
        // Nothing in common.
        check(srv, &[w(), flush(), have(&z), flush(), done()]);
        check(srv, &[w(), flush(), done()]);
        // `done` right behind the `have` lines; text after the id.
        check(srv, &[w(), flush(), have(&h3), done()]);
        check(srv, &[w(), flush(), format!("have {h3} junk"), done()]);
        // Two wanted branches, a common commit that only one reaches at
        // first.
        check(
            srv,
            &[
                w(),
                format!("want {side}"),
                flush(),
                have(&h2),
                flush(),
                have(&h4),
                flush(),
                done(),
            ],
        );
        // Objects that are no commits.
        check(
            srv,
            &[
                w(),
                flush(),
                have(&tree),
                flush(),
                have(&blob),
                flush(),
                done(),
            ],
        );
        // Everything is there already.
        check(srv, &[w(), flush(), have(&h), flush(), done()]);
        check(srv, &[w(), flush(), have(&h1), have(&h), done()]);
        // The client goes away.
        check(srv, &[w(), flush(), have(&h3), flush()]);
    }

    // `no-done`: the pack follows `ready` at once, only with
    // `multi_ack_detailed`.
    let caps = "side-band-64k no-progress";
    for mode in ["multi_ack_detailed", "multi_ack", ""] {
        let w = || want(&format!("{mode} no-done {caps}"));
        check(srv, &[w(), flush(), have(&h3), flush(), done()]);
        check(srv, &[w(), flush(), have(&h3), flush()]);
        check(srv, &[w(), flush(), have(&z), flush(), done()]);
        check(
            srv,
            &[
                w(),
                flush(),
                have(&z),
                have(&h3),
                flush(),
                have(&h2),
                flush(),
            ],
        );
    }

    // `--stateless-rpc`: no advertisement, one round per request.
    let stateless = Call {
        flags: &["--stateless-rpc"],
        ..srv
    };
    check(stateless, &[]);
    check(stateless, &[flush()]);
    check(stateless, &[want(""), flush(), done()]);
    for mode in [
        "multi_ack_detailed no-done",
        "multi_ack_detailed",
        "multi_ack",
        "",
    ] {
        let w = || want(&format!("{mode} {caps}"));
        check(
            stateless,
            &[w(), flush(), have(&z), flush(), have(&h3), flush(), done()],
        );
        check(stateless, &[w(), flush(), have(&h3), flush(), done()]);
        check(stateless, &[w(), flush(), have(&h3), flush()]);
        check(
            stateless,
            &[w(), flush(), have(&z), have(&h3), have(&h2), done()],
        );
    }
    // Protocol v1 is v0 after the advertisement.
    let v1_call = Call {
        env: &[("GIT_PROTOCOL", "version=1")],
        ..srv
    };
    check(v1_call, &[want(caps), flush(), have(&h3), flush(), done()]);
    check(
        Call {
            flags: &["--stateless-rpc"],
            ..v1_call
        },
        &[want(caps), flush(), done()],
    );
}

#[test]
fn bad_requests_equal_git() {
    let fx = repositories();
    if fx.is_skipped() {
        return;
    }
    let h = rev(&fx, "srv", "main");
    let tree = rev(&fx, "srv", "main^{tree}");
    let z = "0".repeat(40);
    let srv = Call {
        repo: "srv",
        ..Call::default()
    };
    let filter = Call {
        config: &["uploadpack.allowFilter=true"],
        ..srv
    };
    let want = format!("want {h} side-band-64k no-progress");
    let filter_want = format!("want {h} side-band-64k no-progress filter");
    let shallow_tree = format!("shallow {tree}");
    let unknown_want = format!("want {z}");
    let cases: &[(Call<'_>, &[&str])] = &[
        (srv, &[&format!("foo {h}"), "0000"]),
        (srv, &["want xyz", "0000"]),
        (srv, &["want", "0000"]),
        (srv, &[&unknown_want, "junk", "0000", "done"]),
        (srv, &[&want, "junk", "0000"]),
        (srv, &[&want, "0000", "have xyz", "0000"]),
        (srv, &[&want, "0000", "have", "0000"]),
        (srv, &[&want, "0000", "foo", "0000"]),
        (srv, &[&want, "0000", &want, "0000"]),
        (srv, &[&want, "shallow xyz", "0000"]),
        (srv, &[&want, &shallow_tree, "0000", "done"]),
        (srv, &[&want, "deepen xyz", "0000"]),
        (srv, &[&want, "deepen 0", "0000", "done"]),
        (srv, &[&want, "deepen -1", "0000", "done"]),
        (srv, &[&want, "deepen-since xyz", "0000", "done"]),
        // The keywords need their argument.
        (srv, &[&want, "deepen-since", "0000", "done"]),
        (srv, &[&want, "deepen", "0000", "done"]),
        (srv, &[&want, "shallow", "0000", "done"]),
        (srv, &[&want, "filter", "0000", "done"]),
        (srv, &[&want, "deepen-since 4102444800", "0000", "done"]),
        (srv, &[&want, "deepen-not nope", "0000", "done"]),
        (srv, &[&want, "deepen 1", "deepen-since 1", "0000", "done"]),
        (srv, &[&want, "deepen 1", "deepen-not side", "0000", "done"]),
        (srv, &[&want, "deepen 1", "0000", "junk"]),
        // Filters need the capability, on both sides.
        (srv, &[&filter_want, "filter blob:none", "0000", "done"]),
        (filter, &[&want, "filter blob:none", "0000", "done"]),
        (filter, &[&filter_want, "filter bogus", "0000", "done"]),
        (
            filter,
            &[
                &filter_want,
                "filter blob:none",
                "filter tree:0",
                "0000",
                "done",
            ],
        ),
    ];
    for (call, lines) in cases {
        let reply = same(&fx, *call, lines);
        assert_eq!(reply.code, Some(128), "{lines:?}: {reply:?}");
        assert!(reply.pack.is_none(), "{lines:?}");
    }
    // Filters `uploadpackfilter.*` does not allow: the client is told.
    for config in [
        &[
            "uploadpack.allowFilter=true",
            "uploadpackfilter.allow=false",
        ][..],
        &[
            "uploadpack.allowFilter=true",
            "uploadpackfilter.tree.maxDepth=1",
        ],
        &[
            "uploadpack.allowFilter=true",
            "uploadpackfilter.blob:none.allow=false",
        ],
    ] {
        for spec in ["filter blob:none", "filter tree:3", "filter blob:limit=10"] {
            let call = Call { config, ..srv };
            same(&fx, call, &[&filter_want, spec, "junk", "0000", "done"]);
        }
    }
}

/// `--timeout`: a client that says nothing is dropped, in the `want`
/// list and in the negotiation.
#[cfg(unix)]
#[test]
fn timeout_ends_an_idle_connection() {
    use std::os::unix::process::ExitStatusExt;
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let want = format!("want {}", rev(&fx, "srv", "main"));
    for lines in [&[][..], &[&want[..], "0000"]] {
        let mut cmd = Command::new(rgit());
        isolated(&fx, &mut cmd);
        let mut child = cmd
            .args(["upload-pack", "--timeout=1", "srv"])
            .current_dir(fx.work_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        // The connection stays open.
        let mut pipe = child.stdin.take().unwrap();
        pipe.write_all(&request(lines)).unwrap();
        let started = std::time::Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if started.elapsed().as_secs() > 30 {
                let _ = child.kill();
                panic!("no time-out after {lines:?}");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        assert_eq!(status.signal(), Some(14), "{lines:?}: {status:?}");
        drop(pipe);
    }
}

#[test]
fn wanted_objects_equal_git() {
    let fx = repositories();
    if fx.is_skipped() {
        return;
    }
    let id = |repo: &str, name: &str| rev(&fx, repo, name);
    let caps = "side-band-64k no-progress";
    let inner = id("srv", "main~1");
    let h2 = id("srv", "main~2");
    let side = id("srv", "side");
    let tree = id("srv", "main^{tree}");
    let blob = id("srv", "main:top");
    let v1 = id("srv", "v1");
    let v1_commit = id("srv", "v1^{commit}");
    // Objects nothing refers to.
    let hashed = run(
        &fx,
        fx.reference_git(),
        &["-C", "srv", "hash-object", "-w", "--stdin"],
        &[],
        &fx.work_dir(),
        b"lost\n",
    );
    let lost_blob = text(&hashed.stdout).trim().to_owned();
    let lost_commit = text(&fx.git_stdout([
        "-C",
        "srv",
        "commit-tree",
        "-m",
        "lost",
        "-p",
        &id("srv", "main"),
        &tree,
    ]))
    .trim()
    .to_owned();
    let hidden_tip = id("hid.git", "refs/hidden/y");
    let hidden_inner = id("hid.git", "refs/hidden/y~1");
    let hidden_tag = id("hid.git", "refs/hidden/t");

    let policies: &[&[&str]] = &[
        &[],
        &["uploadpack.allowTipSHA1InWant=true"],
        &["uploadpack.allowReachableSHA1InWant=true"],
        &["uploadpack.allowAnySHA1InWant=true"],
    ];
    let modes: &[&[&str]] = &[&[], &["--stateless-rpc"]];
    let mut allowed = Vec::new();
    for policy in policies {
        for flags in modes {
            let mut check = |repo: &str, hide: &[&str], what: &str, id: &str| {
                let mut config: Vec<&str> = policy.to_vec();
                config.extend_from_slice(hide);
                let call = Call {
                    config: &config,
                    flags,
                    env: &[],
                    repo,
                };
                let reply = same(&fx, call, &[&format!("want {id} {caps}"), "0000", "done"]);
                if reply.code == Some(0) {
                    allowed.push(format!("{policy:?} {flags:?} {what}"));
                }
            };
            check("srv", &[], "inner commit", &inner);
            check("srv", &[], "peeled tag", &v1_commit);
            check("srv", &[], "tree", &tree);
            check("srv", &[], "blob", &blob);
            check("srv", &[], "lost blob", &lost_blob);
            check("srv", &[], "lost commit", &lost_commit);
            check(
                "srv",
                &["uploadpack.hideRefs=refs/heads/side"],
                "hidden branch",
                &side,
            );
            check("srv", &["uploadpack.hideRefs=refs/tags"], "hidden tag", &v1);
            let hide = &["uploadpack.hideRefs=refs/hidden"][..];
            check("hid.git", hide, "hidden-only tip", &hidden_tip);
            check("hid.git", hide, "hidden-only commit", &hidden_inner);
            check("hid.git", hide, "hidden-only tag", &hidden_tag);
        }
    }
    // The expectations themselves, so that a common failure of both
    // servers does not pass for agreement.
    let has = |s: &str| allowed.iter().any(|a| a == s);
    assert!(!has("[] [] inner commit") && !has("[] [] tree") && !has("[] [] hidden branch"));
    assert!(has("[] [\"--stateless-rpc\"] inner commit"));
    assert!(!has("[] [\"--stateless-rpc\"] lost commit"));
    assert!(has(
        "[\"uploadpack.allowTipSHA1InWant=true\"] [] hidden branch"
    ));
    assert!(!has(
        "[\"uploadpack.allowTipSHA1InWant=true\"] [] hidden-only commit"
    ));
    assert!(has(
        "[\"uploadpack.allowReachableSHA1InWant=true\"] [] hidden-only commit"
    ));
    assert!(!has(
        "[\"uploadpack.allowReachableSHA1InWant=true\"] [] lost commit"
    ));
    assert!(has(
        "[\"uploadpack.allowAnySHA1InWant=true\"] [] lost commit"
    ));

    // Several wanted objects: the first refused one is named.
    let srv = Call {
        repo: "srv",
        ..Call::default()
    };
    let h3 = id("srv", "main~3");
    same(
        &fx,
        srv,
        &[
            &format!("want {inner}"),
            &format!("want {h3}"),
            "0000",
            "done",
        ],
    );
    // In a namespace only its references count.
    let ns = Call {
        env: &[("GIT_NAMESPACE", "foo")],
        repo: "ns.git",
        ..Call::default()
    };
    let main = id("srv", "main");
    let outside = same(&fx, ns, &[&format!("want {main} {caps}"), "0000", "done"]);
    assert_eq!(outside.code, Some(128));
    let inside = same(&fx, ns, &[&format!("want {h2} {caps}"), "0000", "done"]);
    assert_eq!(inside.code, Some(0));
}

#[test]
fn shallow_and_filter_requests_equal_git() {
    let fx = repositories();
    if fx.is_skipped() {
        return;
    }
    let id = |name: &str| rev(&fx, "srv", name);
    let (h, h2, h3) = (id("main"), id("main~2"), id("main~3"));
    let side = id("side");
    let caps = "side-band-64k no-progress";
    let want = format!("want {h} {caps}");
    let want_side = format!("want {side}");
    let relative = format!("want {h} {caps} deepen-relative");
    let shallow = format!("shallow {h2}");
    let have = format!("have {h2}");
    let since = text(&fx.git_stdout(["-C", "srv", "log", "-1", "--format=%ct", "main~3"]));
    let since = format!("deepen-since {}", since.trim());
    let srv = Call {
        repo: "srv",
        ..Call::default()
    };
    let cases: &[&[&str]] = &[
        &[&want, "deepen 1", "0000", "done"],
        &[&want, "deepen 2", "0000", "done"],
        &[&want, &want_side, "deepen 2", "0000", "done"],
        &[&want, "deepen 100", "0000", "done"],
        // The client goes away after the shallow lines.
        &[&want, "deepen 2", "0000"],
        // A shallow client: deeper, complete, as it is.
        &[&want, &shallow, "deepen 4", "0000", &have, "done"],
        &[&want, &shallow, "deepen 2147483647", "0000", &have, "done"],
        &[&want, &shallow, "0000", &have, "done"],
        &[&shallow, &want, "0000", "done"],
        // `deepen-relative` is a capability.
        &[&relative, &shallow, "deepen 1", "0000", &have, "done"],
        &[&want, &shallow, "deepen 1", "0000", &have, "done"],
        &[&relative, "deepen 1", "0000", "done"],
        &[&want, &since, "0000", "done"],
        // A commit this repository does not have cannot be shallow.
        &[
            &want,
            &format!("shallow {}", "0".repeat(40)),
            "0000",
            "done",
        ],
        &[&want, "deepen-not refs/heads/side", "0000", "done"],
        &[&want, "deepen-not side", "0000", "done"],
        &[&want, "deepen-not v1", "0000", "done"],
        &[&want, &since, "deepen-not side", "0000", "done"],
        &[&want, &shallow, &since, "0000", &have, "done"],
    ];
    for lines in cases {
        let reply = same(&fx, srv, lines);
        if lines.last() == Some(&"done") {
            assert_eq!(reply.code, Some(0), "{lines:?}: {reply:?}");
        }
    }
    let one = same(&fx, srv, &[&want, "deepen 1", "0000", "done"]);
    assert_eq!(
        answer(&one)[..3],
        [
            format!("shallow {h}"),
            "0000".to_owned(),
            "NAK\n".to_owned()
        ]
    );
    let deeper = same(
        &fx,
        srv,
        &[&want, &shallow, "deepen 4", "0000", &have, "done"],
    );
    assert_eq!(
        answer(&deeper)[..2],
        [format!("shallow {h3}"), format!("unshallow {h2}")]
    );

    // A repository that is shallow itself.
    let sh = Call {
        repo: "sh.git",
        ..Call::default()
    };
    for lines in [
        &[&want[..], "0000", "done"][..],
        &[&want, "deepen 1", "0000", "done"],
        &[&want, "deepen 5", "0000", "done"],
        &[&want, "deepen 2147483647", "0000", "done"],
    ] {
        assert_eq!(same(&fx, sh, lines).code, Some(0), "{lines:?}");
    }

    // Filters.
    let filter = Call {
        config: &["uploadpack.allowFilter=true"],
        ..srv
    };
    let filter_want = format!("want {h} {caps} filter");
    let full = same(&fx, srv, &[&want, "0000", "done"]).pack.unwrap();
    for spec in [
        "blob:none",
        "blob:limit=100",
        "tree:0",
        "tree:2",
        "object:type=commit",
        "combine:blob:none+tree:2",
    ] {
        let line = format!("filter {spec}");
        let reply = same(&fx, filter, &[&filter_want, &line, "0000", "done"]);
        assert_eq!(reply.code, Some(0), "{spec}");
        assert!(reply.pack.unwrap().len() < full.len(), "{spec}");
        same(
            &fx,
            filter,
            &[&filter_want, &line, "deepen 2", "0000", "done"],
        );
    }
}

// ------------------------------------------------------------------- clients

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// What a repository holds: references, shallow commits, the objects
/// reachable from the references (missing ones marked), and the verdict
/// of `git fsck --strict`.
fn state(fx: &Fixture, repo: &Path) -> String {
    if !repo.exists() {
        return "no repository".to_owned();
    }
    let git = fx.reference_git();
    let get = |args: &[&str]| run(fx, git, args, &[], repo, b"");
    let mut out = String::new();
    let refs = get(&["for-each-ref", "--format=%(refname) %(objectname)"]);
    out.push_str(&text(&refs.stdout));
    let git_dir = text(&get(&["rev-parse", "--absolute-git-dir"]).stdout);
    let mut shallow: Vec<String> =
        std::fs::read_to_string(Path::new(git_dir.trim()).join("shallow"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect();
    shallow.sort();
    out.push_str(&format!("shallow: {shallow:?}\n"));
    let listed = get(&["rev-list", "--objects", "--all", "--missing=print"]);
    let mut objects: Vec<String> = text(&listed.stdout)
        .lines()
        .map(|l| l.split(' ').next().unwrap_or("").to_owned())
        .collect();
    objects.sort();
    out.push_str(&format!("objects: {}\n", objects.join(" ")));
    let fsck = get(&["fsck", "--strict"]);
    let problems: Vec<String> = text(&fsck.stderr)
        .lines()
        .chain(text(&fsck.stdout).lines())
        .filter(|l| {
            !["Checking", "dangling", "notice: "]
                .iter()
                .any(|p| l.starts_with(p))
        })
        .map(str::to_owned)
        .collect();
    out.push_str(&format!("fsck: {:?} {problems:?}\n", fsck.status.code()));
    out
}

/// Run `git -c protocol.version=<version> <args>` with each server in a
/// copy of `base` and require the same outcome and state of `repo`
/// afterwards. `{UP}` in the arguments stands for the server command,
/// `{WORK}` for the fixture's work directory. The client's packet trace
/// must show the old protocol.
#[track_caller]
fn flow(
    fx: &Fixture,
    version: &str,
    base: Option<&str>,
    cwd: &str,
    repo: &str,
    args: &[&str],
) -> String {
    let work = fx.work_dir();
    let git = fx.reference_git().to_path_buf();
    let mut results = Vec::new();
    for (name, program) in [("git", git.clone()), ("rgit", rgit())] {
        let up = format!("'{}' upload-pack", program.display());
        let dir = work.join(format!("flow-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        match base {
            Some(b) => copy_dir(&work.join(b), &dir),
            None => std::fs::create_dir_all(&dir).unwrap(),
        }
        let trace = work.join(format!("trace-{name}"));
        let _ = std::fs::remove_file(&trace);
        let mut argv = vec!["-c".to_owned(), format!("protocol.version={version}")];
        argv.extend(args.iter().map(|a| {
            a.replace("{UP}", &up)
                .replace("{WORK}", work.to_str().unwrap())
        }));
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let env = [("GIT_TRACE_PACKET", trace.to_str().unwrap())];
        let out = run(fx, &git, &argv, &env, &dir.join(cwd), b"");
        let st = state(fx, &dir.join(repo));
        let trace = std::fs::read_to_string(&trace).unwrap_or_default();
        assert!(
            !trace.contains("version 2") && trace.contains("multi_ack"),
            "{name}: not protocol v{version}: {trace}"
        );
        assert_eq!(
            version == "1",
            trace.contains("< version 1"),
            "{name}: {trace}"
        );
        results.push((out.status.code(), st, text(&out.stderr)));
    }
    let (git, ours) = (&results[0], &results[1]);
    assert_eq!(
        (git.0, &git.1),
        (ours.0, &ours.1),
        "v{version} {args:?}\ngit stderr: {}\nrgit stderr: {}",
        git.2,
        ours.2
    );
    assert_eq!(ours.0, Some(0), "v{version} {args:?} failed: {}", ours.2);
    assert!(ours.1.contains("fsck: Some(0) []"), "{args:?}: {}", ours.1);
    ours.1.clone()
}

/// `base/c`: a clone of `srv` that is three commits behind and lacks two
/// tags.
fn with_old_clone(fx: Fixture) -> Fixture {
    fx.git(["clone", "-q", "--no-local", "srv", "base/c"])
        .git(["-C", "base/c", "reset", "-q", "--hard", "HEAD~3"])
        .git([
            "-C",
            "base/c",
            "update-ref",
            "refs/remotes/origin/main",
            "HEAD",
        ])
        .git(["-C", "base/c", "tag", "-d", "v2", "light"])
        .git(["-C", "base/c", "reflog", "expire", "--expire=now", "--all"])
        .git(["-C", "base/c", "gc", "-q", "--prune=now"])
}

#[test]
fn reference_client_clones() {
    let fx = fixture()
        .git(["config", "--global", "uploadpack.allowFilter", "true"])
        .git(["init", "-q", "unborn"])
        .git(["clone", "-q", "srv", "detached"])
        .git(["-C", "detached", "checkout", "-q", "--detach", "HEAD~1"]);
    if fx.is_skipped() {
        return;
    }
    let url = format!("file://{}/srv", fx.work_dir().display());
    let fx = fx.git(["clone", "-q", "--depth", "3", &url, "shallow"]);
    let since = text(&fx.git_stdout(["-C", "srv", "log", "-1", "--format=%ct", "HEAD~2"]));
    let since = format!("--shallow-since={}", since.trim());
    let url = "file://{WORK}/srv";
    for version in ["0", "1"] {
        let clone = |extra: &[&str], from: &str| {
            let mut args = vec!["clone", "-q", "--upload-pack", "{UP}"];
            args.extend_from_slice(extra);
            args.extend([from, "c"]);
            flow(&fx, version, None, "", "c", &args)
        };
        let full = clone(&[], url);
        assert!(full.contains("refs/tags/v2 ") && full.contains("shallow: []"));
        clone(&["--bare"], url);
        clone(&["--mirror"], url);
        clone(&["-b", "side", "--single-branch"], url);
        clone(&["--no-tags"], url);
        let one = clone(&["--depth", "1"], url);
        assert!(!one.contains("shallow: []"));
        clone(&["--depth", "2", "--no-single-branch"], url);
        clone(&["--depth", "100"], url);
        clone(&[&since], url);
        clone(&["--shallow-exclude=side"], url);
        clone(&["--shallow-exclude=v1", "--no-single-branch"], url);
        let partial = clone(&["--no-checkout", "--filter=blob:none"], url);
        assert!(partial.contains('?'), "nothing missing: {partial}");
        clone(&["--no-checkout", "--filter=tree:0"], url);
        clone(
            &["--no-checkout", "--filter=blob:limit=100", "--depth", "2"],
            url,
        );
        clone(&[], "file://{WORK}/unborn");
        clone(&[], "file://{WORK}/detached");
        let from_shallow = clone(&[], "file://{WORK}/shallow");
        assert!(!from_shallow.contains("shallow: []"));
        clone(&["--no-local"], "{WORK}/srv");
    }
}

#[test]
fn reference_client_fetches() {
    let fx = with_old_clone(fixture().git([
        "config",
        "--global",
        "uploadpack.allowReachableSHA1InWant",
        "true",
    ]));
    if fx.is_skipped() {
        return;
    }
    let url = format!("file://{}/srv", fx.work_dir().display());
    let fx = fx.git(["clone", "-q", "--depth", "2", &url, "base/s"]);
    // History the shallow clone has not seen.
    let fx = commit(fx, "srv", "newer", "newer\n");
    let fx = commit(fx, "srv", "newest", "newest\n");
    let sha = rev(&fx, "srv", "HEAD~1");
    let since = text(&fx.git_stdout(["-C", "srv", "log", "-1", "--format=%ct", "HEAD~5"]));
    let since = format!("--shallow-since={}", since.trim());
    for version in ["0", "1"] {
        let fetch_with = |config: &[&str], repo: &str, args: &[&str]| {
            let mut argv: Vec<&str> = Vec::new();
            for c in config {
                argv.extend(["-c", c]);
            }
            argv.extend(["fetch", "-q", "--upload-pack", "{UP}"]);
            argv.extend_from_slice(args);
            flow(&fx, version, Some("base"), repo, repo, &argv)
        };
        let fetch = |args: &[&str]| fetch_with(&[], "c", args);
        let all = fetch(&["origin"]);
        assert!(all.contains("refs/tags/v2 "));
        fetch(&["--no-tags", "origin"]);
        fetch(&["--tags", "origin"]);
        fetch(&["origin", "tag", "v2"]);
        fetch(&["origin", "side:refs/heads/side2"]);
        fetch(&["--depth=1", "origin"]);
        // Many rounds of `have` lines, and none at all.
        fetch_with(&["fetch.negotiationAlgorithm=skipping"], "c", &["origin"]);
        fetch_with(&["fetch.negotiationAlgorithm=noop"], "c", &["origin"]);
        // A commit that is no tip: only where the server allows it.
        fetch(&["origin", &sha]);

        let deepen = |args: &[&str]| fetch_with(&[], "s", args);
        let plain = deepen(&["origin"]);
        assert!(!plain.contains("shallow: []"));
        deepen(&["--deepen=1", "origin"]);
        deepen(&["--deepen=3", "origin"]);
        deepen(&["--depth=1", "origin"]);
        deepen(&["--depth=5", "origin"]);
        let complete = deepen(&["--unshallow", "origin"]);
        assert!(complete.contains("shallow: []"), "{complete}");
        deepen(&[&since, "origin"]);
        deepen(&["--shallow-exclude=v1", "origin"]);
        deepen(&["--update-shallow", "origin", "side:refs/heads/side2"]);
    }
}

/// Smart HTTP: `git http-backend` runs `upload-pack --stateless-rpc`.
#[test]
fn over_http() {
    let fx =
        with_old_clone(fixture().git(["config", "--global", "uploadpack.allowFilter", "true"]));
    if fx.is_skipped() {
        return;
    }
    let git = fx.reference_git().to_path_buf();
    let work = fx.work_dir();
    let exec_path = PathBuf::from(text(&fx.git_stdout(["--exec-path"])).trim());
    let backend = ["git-http-backend", "git-http-backend.exe"]
        .iter()
        .map(|n| exec_path.join(n))
        .find(|p| p.is_file());
    let Some(backend) = backend else {
        eprintln!("skipping: no git-http-backend in {}", exec_path.display());
        return;
    };
    for version in ["0", "1"] {
        let mut results = Vec::new();
        for (name, program) in [("git", git.clone()), ("rgit", rgit())] {
            // One exec path per server, with `git` being the reference
            // git or rgit.
            let bin = work.join(format!("exec-{name}-{version}"));
            std::fs::create_dir_all(&bin).unwrap();
            let log = work.join(format!("exec-{name}-{version}.log"));
            rgit_difftest::write_executable(
                bin.join("git"),
                format!(
                    "#!/bin/sh\necho \"$GIT_PROTOCOL: $*\" >>'{}'\nexec '{}' \"$@\"\n",
                    log.display(),
                    program.display()
                ),
            );
            #[cfg(unix)]
            std::os::unix::fs::symlink(&backend, bin.join("git-http-backend")).unwrap();
            #[cfg(not(unix))]
            std::fs::copy(&backend, bin.join("git-http-backend")).unwrap();
            let base = start_http_with(&fx, vec![("GIT_EXEC_PATH".into(), bin.into_os_string())]);
            let dir = work.join(format!("http-{name}-{version}"));
            copy_dir(&work.join("base"), &dir);
            let url = format!("{base}/srv");
            let config = format!("protocol.version={version}");
            let steps: Vec<(Vec<&str>, &str, &str)> = vec![
                (vec!["clone", "-q", &url, "full"], "", "full"),
                (
                    vec!["clone", "-q", "--depth", "2", &url, "shallow"],
                    "",
                    "shallow",
                ),
                (
                    vec![
                        "clone",
                        "-q",
                        "--no-checkout",
                        "--filter=blob:none",
                        &url,
                        "partial",
                    ],
                    "",
                    "partial",
                ),
                (
                    vec!["fetch", "-q", &url, "refs/heads/*:refs/remotes/http/*"],
                    "c",
                    "c",
                ),
                (
                    vec!["fetch", "-q", "--deepen=2", "origin"],
                    "shallow",
                    "shallow",
                ),
                (
                    vec!["fetch", "-q", "--unshallow", "origin"],
                    "shallow",
                    "shallow",
                ),
            ];
            let mut states = Vec::new();
            for (args, cwd, repo) in steps {
                let mut argv = vec!["-c", config.as_str()];
                argv.extend(args);
                let out = run(&fx, &git, &argv, &[], &dir.join(cwd), b"");
                assert!(
                    out.status.success(),
                    "{name} v{version}: {argv:?}: {}",
                    text(&out.stderr)
                );
                let st = state(&fx, &dir.join(repo));
                assert!(st.contains("fsck: Some(0) []"), "{name}: {argv:?}: {st}");
                states.push(st);
            }
            let log = std::fs::read_to_string(&log).unwrap_or_default();
            assert!(
                log.contains("upload-pack --stateless-rpc") && !log.contains("version=2"),
                "{name} was not asked to serve v{version}: {log}"
            );
            results.push(states);
        }
        assert_eq!(results[0], results[1], "v{version}");
    }
}
