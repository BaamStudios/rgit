// SPDX-License-Identifier: MIT
//! `upload-pack` against the reference git.
//!
//! - Wire level: the same protocol v2 requests are sent to `git
//!   upload-pack` and `rgit upload-pack` (v0 and v1: upload_pack_v0.rs); the packets, standard error and
//!   exit code must be equal (apart from `agent=` and the session id), and
//!   packs must hold the same objects.
//! - Clients: the reference git (and rgit) clone and fetch through both
//!   servers, over `file://`, a fake SSH and smart HTTP; the resulting
//!   repositories must be equal and pass `git fsck --strict`.

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

fn commit(fx: Fixture, dir: &str, file: &str, content: &str) -> Fixture {
    fx.write(format!("{dir}/{file}"), content)
        .git(["-C", dir, "add", file])
        .git(["-C", dir, "commit", "-q", "-m", file])
        .tick()
}

/// `srv`: six commits on `main` with files in nested directories, a
/// branch `side` at the first commit with a commit of its own, annotated
/// tags `v1` (third commit) and `v2` (tip), a lightweight tag.
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

/// `git clone <args> file://<work>/<from> <to>` in the work directory.
fn clone_from(fx: Fixture, args: &[&str], from: &str, to: &str) -> Fixture {
    if fx.is_skipped() {
        return fx;
    }
    let url = format!("file://{}/{from}", fx.work_dir().display());
    let mut argv = vec!["clone", "-q"];
    argv.extend_from_slice(args);
    argv.extend([url.as_str(), to]);
    fx.git(argv)
}

fn rev(fx: &Fixture, repo: &str, name: &str) -> String {
    String::from_utf8(fx.git_stdout(["-C", repo, "rev-parse", name]))
        .unwrap()
        .trim()
        .to_owned()
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

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

// ---------------------------------------------------------------- wire level

/// Pack `lines` as pkt-lines; `0000` and `0001` stand for themselves.
fn request(lines: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    for line in lines {
        match line.as_str() {
            "0000" | "0001" => out.extend_from_slice(line.as_bytes()),
            _ => out.extend_from_slice(format!("{:04x}{line}\n", line.len() + 5).as_bytes()),
        }
    }
    out
}

/// What a server answered: the packets as text (the packs and their
/// progress left out, `agent` and session id made equal), the objects of
/// each pack, standard error and the exit code.
#[derive(Debug, PartialEq, Eq)]
struct Reply {
    packets: Vec<String>,
    packs: Vec<BTreeSet<String>>,
    stderr: String,
    code: Option<i32>,
}

fn normalize(line: &str) -> String {
    let mut line = line.to_owned();
    for key in ["agent=", "session-id="] {
        if let Some(at) = line.find(key) {
            line.truncate(at + key.len());
            line.push_str("X\n");
        }
    }
    line
}

fn is_progress(payload: &[u8]) -> bool {
    let text = String::from_utf8_lossy(payload);
    text.split(['\r', '\n']).all(|l| {
        l.trim().is_empty()
            || [
                "Enumerating objects",
                "Counting objects",
                "Compressing objects",
                "Delta compression",
                "Total ",
            ]
            .iter()
            .any(|p| l.trim_start().starts_with(p))
    })
}

fn decode(fx: &Fixture, out: &Output) -> Reply {
    let mut data = out.stdout.as_slice();
    let mut packets = Vec::new();
    let mut pack: Vec<u8> = Vec::new();
    let mut packs = Vec::new();
    let mut progress: Vec<u8> = Vec::new();
    let mut in_pack = false;
    while data.len() >= 4 {
        let Ok(len) = usize::from_str_radix(&text(&data[..4]), 16) else {
            packets.push(format!("GARBAGE {}", text(data)));
            break;
        };
        if len < 4 {
            packets.push(text(&data[..4]));
            if len == 0 {
                in_pack = false;
                if !pack.is_empty() {
                    packs.push(pack_objects(fx, &pack));
                    pack.clear();
                }
                // Progress meters come in pieces of any size; anything
                // else on sideband 2 is part of the answer.
                if !is_progress(&progress) {
                    packets.push(format!("band 2: {}", text(&progress)));
                }
                progress.clear();
            }
            data = &data[4..];
            continue;
        }
        let payload = &data[4..len];
        data = &data[len..];
        if in_pack {
            match payload.first() {
                Some(1) => pack.extend_from_slice(&payload[1..]),
                Some(2) => progress.extend_from_slice(&payload[1..]),
                Some(band) => packets.push(format!("band {band}: {}", text(&payload[1..]))),
                None => packets.push("empty packet".to_owned()),
            }
            continue;
        }
        let line = text(payload);
        // With `sideband-all` the section header is on sideband 1.
        if line == "packfile\n" || line == "\u{1}packfile\n" {
            in_pack = true;
        }
        packets.push(normalize(&line));
    }
    if !pack.is_empty() {
        packets.push(format!("unfinished pack of {} bytes", pack.len()));
    }
    Reply {
        packets,
        packs,
        stderr: text(&out.stderr),
        code: out.status.code(),
    }
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

/// Send `lines` as one request to the server `program` and decode the
/// answer.
fn exchange(
    fx: &Fixture,
    program: &Path,
    config: &[&str],
    repo: &str,
    stateless: bool,
    lines: &[String],
) -> Reply {
    let mut args: Vec<&str> = Vec::new();
    for c in config {
        args.push("-c");
        args.push(c);
    }
    args.push("upload-pack");
    if stateless {
        args.push("--stateless-rpc");
    }
    args.push(repo);
    let out = run(
        fx,
        program,
        &args,
        &[("GIT_PROTOCOL", "version=2")],
        &fx.work_dir(),
        &request(lines),
    );
    decode(fx, &out)
}

/// The request must be answered alike by git and rgit.
#[track_caller]
fn same(fx: &Fixture, config: &[&str], repo: &str, lines: &[&str]) -> Reply {
    let lines: Vec<String> = lines.iter().map(|l| (*l).to_owned()).collect();
    let git = exchange(fx, fx.reference_git(), config, repo, true, &lines);
    let ours = exchange(fx, &rgit(), config, repo, true, &lines);
    assert_eq!(git, ours, "request {lines:?} with {config:?} on {repo}");
    ours
}

fn fetch_request(args: &[String]) -> Vec<String> {
    let mut lines = vec!["command=fetch".to_owned(), "0001".to_owned()];
    lines.extend(args.iter().cloned());
    lines.push("0000".to_owned());
    lines
}

#[track_caller]
fn same_fetch(fx: &Fixture, config: &[&str], repo: &str, args: &[String]) -> Reply {
    let lines = fetch_request(args);
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    same(fx, config, repo, &refs)
}

#[test]
fn advertisement_equals_git() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let configs: &[&[&str]] = &[
        &[],
        &["uploadpack.allowFilter=true"],
        &["uploadpack.allowRefInWant=true"],
        &["uploadpack.allowSidebandAll=true"],
        &["transfer.advertiseSID=true"],
        &["transfer.advertiseObjectInfo=true"],
        &["lsrefs.unborn=allow"],
        &["lsrefs.unborn=ignore"],
        &["uploadpack.advertiseBundleURIs=true"],
        &["promisor.advertise=true"],
        &[
            "promisor.advertise=true",
            "remote.lop.promisor=true",
            "remote.lop.url=file:///a b/l%o,p;q=r",
            "remote.nourl.promisor=true",
            "remote.other.partialCloneFilter=blob:none",
            "remote.other.url=https://o",
            "remote.other.token=t k",
            "promisor.sendFields=token, partialCloneFilter",
            "transfer.advertiseObjectInfo=true",
        ],
        &[
            "promisor.advertise=false",
            "remote.lop.promisor=true",
            "remote.lop.url=https://l",
        ],
        &[
            "uploadpack.allowFilter=true",
            "uploadpack.allowRefInWant=true",
            "uploadpack.allowSidebandAll=true",
            "transfer.advertiseSID=true",
            "transfer.advertiseObjectInfo=true",
            "uploadpack.advertiseBundleURIs=true",
        ],
    ];
    for config in configs {
        for mode in ["--advertise-refs", "--http-backend-info-refs", ""] {
            let serve = |program: &Path| {
                let mut args: Vec<&str> = Vec::new();
                for c in *config {
                    args.extend(["-c", c]);
                }
                args.push("upload-pack");
                if !mode.is_empty() {
                    args.push(mode);
                }
                args.push("srv");
                let env = [("GIT_PROTOCOL", "version=2")];
                decode(&fx, &run(&fx, program, &args, &env, &fx.work_dir(), b""))
            };
            assert_eq!(
                serve(fx.reference_git()),
                serve(&rgit()),
                "{mode} with {config:?}"
            );
        }
    }
}

#[test]
fn ls_refs_equals_git() {
    let fx = fixture()
        .git(["init", "-q", "unborn"])
        .git([
            "-C",
            "srv",
            "symbolic-ref",
            "refs/heads/alias",
            "refs/heads/side",
        ])
        .git([
            "-C",
            "srv",
            "symbolic-ref",
            "refs/heads/alias2",
            "refs/heads/alias",
        ])
        .git([
            "-C",
            "srv",
            "symbolic-ref",
            "refs/heads/dangling",
            "refs/heads/nope",
        ])
        .git([
            "-C",
            "srv",
            "update-ref",
            "refs/remotes/origin/main",
            "HEAD",
        ])
        .git(["-C", "srv", "update-ref", "ORIG_HEAD", "HEAD"])
        .git(["clone", "-q", "srv", "detached"])
        .git(["-C", "detached", "checkout", "-q", "--detach", "HEAD~1"]);
    if fx.is_skipped() {
        return;
    }
    let reply = same(&fx, &[], "srv", &["command=ls-refs", "0000"]);
    assert!(reply.packets.len() > 5, "{reply:?}");
    let cases: &[&[&str]] = &[
        &["symrefs", "peel", "unborn"],
        &["peel", "ref-prefix refs/tags/"],
        &["symrefs", "ref-prefix refs/heads/a", "ref-prefix HEAD"],
        &[
            "ref-prefix refs/heads/",
            "ref-prefix refs/heads/m",
            "ref-prefix refs/he",
        ],
        &["ref-prefix nothing"],
        &["no-such-arg"],
    ];
    for repo in ["srv", "unborn", "detached"] {
        for args in cases {
            let mut lines = vec!["command=ls-refs", "object-format=sha1", "agent=x", "0001"];
            lines.extend_from_slice(args);
            lines.push("0000");
            same(&fx, &[], repo, &lines);
            same(&fx, &["lsrefs.unborn=ignore"], repo, &lines);
        }
    }
    // A client that sends more prefixes than are worth matching gets all.
    let mut lines: Vec<String> = vec!["command=ls-refs".into(), "0001".into()];
    lines.extend((0..65536).map(|i| format!("ref-prefix refs/nothing/{i}")));
    lines.push("0000".into());
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let all = same(&fx, &[], "srv", &refs);
    assert!(
        all.packets
            .iter()
            .any(|p| p.ends_with(" refs/heads/main\n"))
    );
}

#[test]
fn requests_and_errors_equal_git() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let head = rev(&fx, "srv", "HEAD");
    let cases: &[&[&str]] = &[
        &["0000"],
        &["foobar", "0000"],
        &["agent=x", "0000"],
        &["command=ls-refs", "object-format=sha256", "0000"],
        &["command=ls-refs", "command=fetch", "0000"],
        &["command=bogus", "0000"],
        &["command=agent", "0000"],
        &["command=ls-refs=whatever", "0000"],
        &["command=ls-refs", "fetch", "0000"],
        &["command=ls-refs", "session-id=abc", "0000"],
        &["command=ls-refs", "promisor-remote", "0000"],
        &["command=fetch", "object-format", "0001", "0000"],
        &[
            "command=fetch",
            "agent",
            "server-option",
            "server-option=x",
            "0001",
            "0000",
        ],
        &["command=object-info", "0000"],
        &["command=bundle-uri", "0000"],
        &["command=ls-refs", "0001", "0001"],
        &["command=fetch", "0001", "0001"],
        &["command=fetch", "0001", "bogus", "0000"],
        &["command=fetch", "0001", "done", "0000"],
        &["command=fetch", "0001", "want xyz", "0000"],
        &["command=fetch", "0001", "have xyz", "0000"],
        &["command=fetch", "0001", "shallow xyz", "0000"],
        &["command=fetch", "0001", "filter blob:none", "0000"],
        &["command=fetch", "0001", "want-ref refs/heads/main", "0000"],
        &["command=fetch", "0001", "sideband-all", "0000"],
        &["command=fetch", "0001", "packfile-uris https", "0000"],
        &[
            "command=fetch",
            "0001",
            "want 1111111111111111111111111111111111111111",
            "done",
            "0000",
        ],
    ];
    for lines in cases {
        same(&fx, &[], "srv", lines);
    }
    let want = format!("want {head}");
    let with_want: &[(&[&str], &[&str])] = &[
        (&[], &["deepen 0"]),
        (&[], &["deepen x"]),
        (&[], &["deepen-since x"]),
        (&[], &["deepen-not nope"]),
        (&[], &["deepen-not", "done"]),
        (&[], &["deepen 1", "deepen-since 1", "done"]),
        (&[], &["deepen-since 99999999999", "done"]),
        (&[], &["deepen-not main", "done"]),
        (&["uploadpack.allowFilter=true"], &["filter bogus", "done"]),
        (&["uploadpack.allowFilter=true"], &["filter tree:x", "done"]),
        (
            &[
                "uploadpack.allowFilter=true",
                "uploadpackfilter.blob:none.allow=false",
            ],
            &["filter blob:none", "done"],
        ),
        (
            &[
                "uploadpack.allowFilter=true",
                "uploadpackfilter.tree.maxDepth=1",
            ],
            &["filter tree:3", "done"],
        ),
        (
            &[
                "uploadpack.allowFilter=true",
                "uploadpackfilter.allow=false",
                "uploadpackfilter.combine.allow=true",
                "uploadpackfilter.tree.allow=true",
            ],
            &["filter combine:blob:none+tree:1", "done"],
        ),
        (
            &["uploadpack.allowRefInWant=true"],
            &["want-ref refs/heads/nope", "done"],
        ),
    ];
    for (config, args) in with_want {
        let mut lines = vec!["command=fetch", "0001", want.as_str()];
        lines.extend_from_slice(args);
        lines.push("0000");
        same(&fx, config, "srv", &lines);
    }
}

#[test]
fn object_info_equals_git() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let on: &[&str] = &["transfer.advertiseObjectInfo=true"];
    let commit = format!("oid {}", rev(&fx, "srv", "HEAD"));
    let tree = format!("oid {}", rev(&fx, "srv", "HEAD^{tree}"));
    let blob = format!("oid {}", rev(&fx, "srv", "HEAD:big"));
    let tag = format!("oid {}", rev(&fx, "srv", "v1"));
    let unknown = "oid 1111111111111111111111111111111111111111";
    let cases: &[&[&str]] = &[
        &["size", "type", &commit, &tree, &blob, &tag, unknown],
        &["size", &blob, &blob],
        &["type", &blob, "size"],
        &[&blob],
        &[unknown],
        &["size", unknown],
        &["bogus"],
        &["oid xyz"],
        &[],
    ];
    for args in cases {
        let mut lines = vec!["command=object-info", "0001"];
        lines.extend_from_slice(args);
        lines.push("0000");
        same(&fx, on, "srv", &lines);
    }
    same(&fx, on, "srv", &["command=object-info", "0000"]);
}

/// `wait-for-done` without `want`: a client that only negotiates
/// (`fetch --negotiate-only`, `push.negotiate`) gets its acknowledgments,
/// and an empty pack when it says `done` or names nothing it has. A
/// request without `have` lines is answered with the pack at once.
#[test]
fn negotiation_only_requests_equal_git() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let r = |name: &str| rev(&fx, "srv", name);
    let have = |id: &str| format!("have {id}");
    let unknown = "1".repeat(40);
    let wait = || "wait-for-done".to_owned();
    let cases: Vec<Vec<String>> = vec![
        vec![wait(), have(&r("HEAD~4")), have(&unknown)],
        vec![wait(), have(&unknown)],
        vec![
            wait(),
            have(&r("HEAD~1")),
            have(&r("HEAD~2")),
            have(&r("side")),
        ],
        vec![wait()],
        vec![wait(), have(&r("HEAD~4")), "done".to_owned()],
        // Without it there is nothing to say.
        vec![have(&r("HEAD~4")), have(&unknown)],
        vec![have(&r("HEAD~4")), "done".to_owned()],
        vec![],
    ];
    let head = format!("want {}", r("HEAD"));
    for args in [
        vec![head.clone(), "no-progress".to_owned()],
        vec![head.clone(), wait(), "no-progress".to_owned()],
    ] {
        let reply = same_fetch(&fx, &[], "srv", &args);
        assert_eq!(reply.packets[0], "packfile\n", "{args:?}");
        assert_eq!(reply.packs.len(), 1, "{args:?}");
    }
    for (i, args) in cases.iter().enumerate() {
        let reply = same_fetch(&fx, &[], "srv", args);
        assert_eq!(reply.code, Some(0), "{args:?}");
        assert_eq!(i < 5, !reply.packets.is_empty(), "{args:?}: {reply:?}");
    }
    let acked = same_fetch(&fx, &[], "srv", &cases[0]);
    assert_eq!(
        acked.packets,
        [
            "acknowledgments\n".to_owned(),
            format!("ACK {}\n", r("HEAD~4")),
            "0000".to_owned()
        ]
    );

    // The clients: the common commits come back on standard output.
    let fx = with_old_clone(fx);
    let git = fx.reference_git().to_path_buf();
    let work = fx.work_dir();
    let tip = rev(&fx, "base/c", "HEAD");
    for client in [git.clone(), rgit()] {
        let mut answers = Vec::new();
        for (_, up) in servers(&fx) {
            let args = [
                "fetch",
                "--negotiate-only",
                "--negotiation-tip=HEAD",
                "--upload-pack",
                &up,
                "origin",
            ];
            let out = run(&fx, &client, &args, &[], &work.join("base/c"), b"");
            answers.push((out.status.code(), text(&out.stdout)));
        }
        assert_eq!(answers[0], answers[1], "{}", client.display());
        assert_eq!(answers[1], (Some(0), format!("{tip}\n")));
    }
}

#[test]
fn negotiation_and_packs_equal_git() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let r = |name: &str| rev(&fx, "srv", name);
    let (head, parent, old) = (r("HEAD"), r("HEAD~1"), r("HEAD~4"));
    let (side, root, tree, blob) = (r("side"), r("HEAD~5"), r("HEAD^{tree}"), r("HEAD:big"));
    let unknown = "1".repeat(40);
    let want = |id: &str| format!("want {id}");
    let have = |id: &str| format!("have {id}");
    let quiet = || "no-progress".to_owned();
    let done = || "done".to_owned();
    let cases: Vec<Vec<String>> = vec![
        // Clone.
        vec![want(&head), want(&side), done(), quiet()],
        vec![
            want(&head),
            "ofs-delta".into(),
            "include-tag".into(),
            done(),
            quiet(),
        ],
        vec![want(&head), done()],
        // Nothing in common, something in common, ready.
        vec![want(&head), have(&unknown)],
        vec![
            want(&head),
            have(&parent),
            have(&unknown),
            have(&parent),
            quiet(),
        ],
        vec![want(&head), have(&root), quiet()],
        vec![want(&head), want(&side), have(&parent), quiet()],
        vec![want(&head), want(&side), have(&old), quiet()],
        vec![want(&side), have(&head), quiet()],
        vec![want(&root), have(&head), quiet()],
        vec![want(&head), have(&head), quiet()],
        vec![want(&head), have(&tree), have(&tree)],
        vec![want(&tree), have(&parent), quiet()],
        vec![want(&head), have(&parent), "wait-for-done".into()],
        // `done` with haves: no acknowledgments.
        vec![want(&head), have(&parent), have(&tree), done(), quiet()],
        vec![want(&head), have(&unknown), done(), quiet()],
        // Objects that are no commits.
        vec![want(&tree), done(), quiet()],
        vec![want(&blob), done(), quiet()],
        vec![want(&r("v1")), done(), quiet()],
        vec![want(&r("v1")), "include-tag".into(), done(), quiet()],
        vec![want(&parent), "include-tag".into(), done(), quiet()],
        // No wants.
        vec![have(&head)],
        vec![have(&head), done()],
        // `wait-for-done` (`fetch --negotiate-only`): the acknowledgments
        // come without wants too.
        vec![have(&head), "wait-for-done".into()],
        vec![have(&parent), have(&unknown), "wait-for-done".into()],
        vec![have(&unknown), "wait-for-done".into()],
        vec!["wait-for-done".into(), quiet()],
        // No haves: nothing to acknowledge, the pack follows at once.
        vec![want(&head), quiet()],
        vec![want(&head), "wait-for-done".into(), quiet()],
    ];
    for args in &cases {
        same_fetch(&fx, &[], "srv", args);
    }
    // With `sideband-all` every line is on sideband 1.
    let sideband = same_fetch(
        &fx,
        &["uploadpack.allowSidebandAll=true"],
        "srv",
        &[want(&head), have(&root), "sideband-all".into()],
    );
    assert_eq!(sideband.packets[0], "\u{1}acknowledgments\n");
    // `want-ref`.
    let refs = same_fetch(
        &fx,
        &["uploadpack.allowRefInWant=true"],
        "srv",
        &[
            "want-ref refs/heads/main".into(),
            "want-ref refs/tags/v1".into(),
            "want-ref HEAD".into(),
            done(),
            quiet(),
        ],
    );
    assert!(refs.packets.contains(&"wanted-refs\n".to_owned()));
}

#[test]
fn shallow_requests_equal_git() {
    let fx = clone_from(fixture(), &["--depth", "3"], "srv", "shallow");
    if fx.is_skipped() {
        return;
    }
    let r = |name: &str| rev(&fx, "srv", name);
    let (head, p1, p2, p3) = (r("HEAD"), r("HEAD~1"), r("HEAD~2"), r("HEAD~3"));
    let side = r("side");
    let since = text(&fx.git_stdout(["-C", "srv", "log", "-1", "--format=%ct", "HEAD~2"]));
    let since = since.trim();
    let tail = || vec!["done".to_owned(), "no-progress".to_owned()];
    let case = |args: &[&str]| -> Vec<String> {
        args.iter().map(|a| (*a).to_owned()).chain(tail()).collect()
    };
    let want = format!("want {head}");
    let want_side = format!("want {side}");
    let shallow1 = format!("shallow {p1}");
    let shallow3 = format!("shallow {p3}");
    let have1 = format!("have {p1}");
    let cases: Vec<Vec<String>> = vec![
        case(&[&want, "deepen 1"]),
        case(&[&want, "deepen 3"]),
        case(&[&want, "deepen 6"]),
        case(&[&want, "deepen 7"]),
        case(&[&want, "deepen 2", "deepen 4"]),
        case(&[&want, &want_side, "deepen 2"]),
        case(&[&want, "deepen 2147483647"]),
        // A client that is shallow already.
        case(&[&want, &shallow1]),
        case(&[
            &want,
            &shallow1,
            "shallow 1111111111111111111111111111111111111111",
        ]),
        case(&[&want, &shallow1, &have1]),
        case(&[&want, &shallow1, &have1, "deepen 1"]),
        case(&[&want, &shallow1, &have1, "deepen 2"]),
        case(&[&want, &shallow1, &have1, "deepen 3"]),
        case(&[&want, &shallow1, "deepen 4"]),
        case(&[&want, &shallow1, "deepen 2147483647"]),
        case(&[&want, &shallow1, "deepen 1", "deepen-relative"]),
        case(&[&want, &shallow3, &have1, "deepen 2", "deepen-relative"]),
        case(&[&want, "deepen 1", "deepen-relative"]),
        // Cut by time and by reference.
        case(&[&want, &format!("deepen-since {since}")]),
        case(&[&want, "deepen-since 0"]),
        case(&[&want, &shallow3, "deepen-since 1"]),
        case(&[&want, "deepen-not refs/heads/side"]),
        case(&[&want, "deepen-not side"]),
        case(&[&want, "deepen-not v1"]),
        case(&[&want, &shallow3, "deepen-not side"]),
        case(&[&want, "deepen-not v1", "deepen-not side", "deepen-since 0"]),
        case(&[&want, &format!("deepen-not {p2}")]),
        // Tags and trees as wants.
        case(&[&format!("want {}", r("v1")), "deepen 1"]),
        case(&[&format!("want {}", r("HEAD^{tree}")), "deepen 1"]),
        // Negotiation and a shallow request in one.
        vec![
            want.clone(),
            "deepen 1".into(),
            have1.clone(),
            "no-progress".into(),
        ],
    ];
    for args in &cases {
        same_fetch(&fx, &[], "srv", args);
    }
    // A repository that is shallow itself tells where its history ends.
    let s = |name: &str| rev(&fx, "shallow", name);
    let (tip, below) = (s("HEAD"), s("HEAD~1"));
    let boundary = s("HEAD~2");
    let want = format!("want {tip}");
    let cases: Vec<Vec<String>> = vec![
        case(&[&want]),
        case(&[&want, &format!("shallow {below}")]),
        case(&[&want, &format!("shallow {boundary}")]),
        case(&[&format!("want {below}")]),
        case(&[&want, &format!("have {below}")]),
        case(&[&want, "deepen 2"]),
        case(&[&want, "deepen 5"]),
        vec![want.clone(), format!("have {below}"), "no-progress".into()],
        vec![want.clone(), format!("have {p3}")],
    ];
    for args in &cases {
        same_fetch(&fx, &[], "shallow", args);
    }
}

/// `merges`: a history with merges, every commit tagged with its name.
///
/// ```text
/// r - a1 - a2 - m1 - a3 - m2 - a4 - m3 - a5   (main)
///  \\- b1 - b2 -/   \\         /    \\- d1 -/
///   \\- c1 - c2 ------\\-------/
/// ```
fn merged_history() -> Fixture {
    let fx = Fixture::new().rgit(rgit()).git(["init", "-q", "merges"]);
    let commit = |fx: Fixture, name: &str| {
        fx.write(format!("merges/{name}"), format!("{name}\n"))
            .git(["-C", "merges", "add", name])
            .git(["-C", "merges", "commit", "-q", "-m", name])
            .git(["-C", "merges", "tag", name])
            .tick()
    };
    let merge = |fx: Fixture, name: &str, other: &str| {
        fx.git(["-C", "merges", "merge", "-q", "--no-ff", "-m", name, other])
            .git(["-C", "merges", "tag", name])
            .tick()
    };
    let branch = |fx: Fixture, name: &str, from: &str| {
        fx.git(["-C", "merges", "checkout", "-q", "-b", name, from])
    };
    let main = |fx: Fixture| fx.git(["-C", "merges", "checkout", "-q", "main"]);
    let mut fx = commit(fx, "r");
    for name in ["a1", "a2"] {
        fx = commit(fx, name);
    }
    fx = branch(fx, "b", "r");
    for name in ["b1", "b2"] {
        fx = commit(fx, name);
    }
    fx = branch(fx, "c", "r");
    for name in ["c1", "c2"] {
        fx = commit(fx, name);
    }
    fx = merge(main(fx), "m1", "b");
    fx = commit(fx, "a3");
    fx = merge(fx, "m2", "c");
    fx = commit(fx, "a4");
    fx = commit(branch(fx, "d", "m1"), "d1");
    fx = merge(main(fx), "m3", "d");
    commit(fx, "a5")
}

#[test]
fn merged_history_equals_git() {
    let fx = merged_history();
    if fx.is_skipped() {
        return;
    }
    let r = |name: &str| rev(&fx, "merges", name);
    let want = |name: &str| format!("want {}", r(name));
    let have = |name: &str| format!("have {}", r(name));
    let shallow = |name: &str| format!("shallow {}", r(name));
    let quiet = || "no-progress".to_owned();
    let done = || "done".to_owned();
    let unknown = format!("have {}", "1".repeat(40));

    // Which `have`s are acknowledged, and when the server is ready.
    let negotiation: Vec<Vec<String>> = vec![
        vec![want("a5"), have("a4"), have("d1"), quiet()],
        vec![want("a5"), have("d1"), have("a4"), quiet()],
        // m2 is the parent of a4: known already, not acknowledged.
        vec![
            want("a5"),
            unknown.clone(),
            have("c1"),
            have("b1"),
            have("a4"),
            have("d1"),
            have("m2"),
            quiet(),
        ],
        vec![
            want("a5"),
            want("c2"),
            have("a1"),
            have("b1"),
            have("r"),
            have("c1"),
            quiet(),
        ],
        // A chain: every commit makes its parent known.
        vec![
            want("a5"),
            have("a3"),
            have("m1"),
            have("a2"),
            have("a1"),
            have("r"),
            quiet(),
        ],
        vec![
            want("a5"),
            have("a4"),
            have("d1"),
            "wait-for-done".into(),
            quiet(),
        ],
        vec![want("a5"), have("a4"), have("d1"), done(), quiet()],
        vec![
            want("a5"),
            format!("have {}", r("a5^{tree}")),
            have("a4"),
            have("d1"),
            quiet(),
        ],
        vec![want("c2"), have("a5"), quiet()],
        vec![want("a5"), want("c2"), have("a2")],
        vec![want("a5"), want("c2"), have("a1"), quiet()],
    ];
    for args in &negotiation {
        same_fetch(&fx, &[], "merges", args);
    }

    // The order of the `shallow` and `unshallow` lines.
    let time = |name: &str| {
        text(&fx.git_stdout(["-C", "merges", "log", "-1", "--format=%ct", name, "--"]))
            .trim()
            .to_owned()
    };
    let tail = || vec![done(), quiet()];
    let mut shallows: Vec<Vec<String>> = Vec::new();
    for depth in 2..=7 {
        shallows.push(vec![want("a5"), format!("deepen {depth}")]);
    }
    shallows.extend([
        vec![want("a5"), want("d1"), want("c2"), "deepen 2".into()],
        vec![want("c2"), want("d1"), want("a5"), "deepen 2".into()],
        // A wanted commit that an earlier one reaches at the limit.
        vec![want("a5"), want("m3"), want("a4"), "deepen 2".into()],
        vec![
            want("a5"),
            shallow("a3"),
            shallow("d1"),
            shallow("c1"),
            shallow("m1"),
            "deepen 6".into(),
        ],
        vec![
            want("a5"),
            shallow("c1"),
            shallow("m1"),
            shallow("d1"),
            shallow("a3"),
            "deepen 2147483647".into(),
        ],
        vec![
            want("a5"),
            shallow("a4"),
            shallow("d1"),
            "deepen 2".into(),
            "deepen-relative".into(),
        ],
        vec![
            want("a5"),
            shallow("d1"),
            shallow("a4"),
            "deepen 1".into(),
            "deepen-relative".into(),
        ],
        vec![
            want("a5"),
            shallow("a3"),
            shallow("c1"),
            shallow("m1"),
            "deepen 1".into(),
            "deepen-relative".into(),
        ],
        vec![
            want("a5"),
            shallow("m1"),
            shallow("a3"),
            shallow("c2"),
            shallow("d1"),
            "deepen 1".into(),
            "deepen-relative".into(),
        ],
        vec![
            want("a5"),
            shallow("m3"),
            "deepen 3".into(),
            "deepen-relative".into(),
        ],
        vec![
            want("a5"),
            shallow("m2"),
            shallow("d1"),
            shallow("a5"),
            format!("deepen-since {}", time("b1")),
        ],
        vec![
            want("a5"),
            "deepen-not a2".into(),
            "deepen-not b1".into(),
            "deepen-not c1".into(),
        ],
        vec![want("a5"), "deepen-not c2".into(), "deepen-not a3".into()],
    ]);
    for name in ["a1", "b1", "c1", "a3", "m1", "d1"] {
        shallows.push(vec![want("a5"), format!("deepen-since {}", time(name))]);
    }
    for name in ["r", "a1", "b2", "m1"] {
        shallows.push(vec![want("a5"), format!("deepen-not {name}")]);
    }
    for mut args in shallows {
        args.extend(tail());
        same_fetch(&fx, &[], "merges", &args);
    }

    // The same history, shallow on the server at a1, b1 and c2.
    let work = fx.work_dir();
    copy_dir(&work.join("merges"), &work.join("cut"));
    let cut = format!("{}\n{}\n{}\n", r("a1"), r("b1"), r("c2"));
    std::fs::write(work.join("cut/.git/shallow"), cut).unwrap();
    let cases: Vec<Vec<String>> = vec![
        vec![want("a5")],
        vec![want("c2"), want("a5")],
        vec![want("a5"), shallow("b1"), shallow("m2")],
        vec![want("a5"), "deepen 6".into()],
        vec![want("a5"), "deepen 20".into()],
        vec![want("a5"), shallow("m1"), "deepen 2147483647".into()],
        vec![want("a5"), have("a4")],
    ];
    for mut args in cases {
        args.extend(tail());
        same_fetch(&fx, &[], "cut", &args);
    }
}

#[test]
fn filtered_packs_equal_git() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let on: &[&str] = &["uploadpack.allowFilter=true"];
    let r = |name: &str| rev(&fx, "srv", name);
    let (head, old, tree, blob) = (r("HEAD"), r("HEAD~3"), r("HEAD^{tree}"), r("HEAD:big"));
    for filter in [
        "blob:none",
        "blob:limit=0",
        "blob:limit=5",
        "blob:limit=1k",
        "tree:0",
        "tree:1",
        "tree:2",
        "tree:3",
        "tree:4",
        "object:type=blob",
        "object:type=tree",
        "object:type=commit",
        "object:type=tag",
        "combine:blob:none+tree:2",
        "combine:tree:3+blob:limit=5",
    ] {
        let filter = format!("filter {filter}");
        let tail = [filter.clone(), "done".to_owned(), "no-progress".to_owned()];
        let cases: Vec<Vec<String>> = vec![
            vec![format!("want {head}")],
            vec![format!("want {head}"), format!("have {old}")],
            vec![format!("want {}", r("v2")), format!("want {}", r("side"))],
            // What a partial clone asks for later: single objects.
            vec![format!("want {blob}")],
            vec![format!("want {tree}")],
        ];
        for mut args in cases {
            args.extend(tail.iter().cloned());
            same_fetch(&fx, on, "srv", &args);
        }
    }
}

#[test]
fn several_requests_on_one_connection() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let head = rev(&fx, "srv", "HEAD");
    let root = rev(&fx, "srv", "HEAD~5");
    let mut lines: Vec<String> = Vec::new();
    for l in ["command=ls-refs", "0001", "ref-prefix refs/heads/s", "0000"] {
        lines.push(l.into());
    }
    lines.extend(fetch_request(&[
        format!("want {head}"),
        format!("have {root}"),
        "no-progress".into(),
    ]));
    lines.extend(fetch_request(&[
        format!("want {head}"),
        "done".into(),
        "no-progress".into(),
    ]));
    for l in [
        "command=ls-refs",
        "0001",
        "ref-prefix refs/tags/v",
        "0000",
        "0000",
    ] {
        lines.push(l.into());
    }
    // Not read any more.
    for l in ["command=bogus", "0000"] {
        lines.push(l.into());
    }
    let git = exchange(&fx, fx.reference_git(), &[], "srv", false, &lines);
    let ours = exchange(&fx, &rgit(), &[], "srv", false, &lines);
    assert_eq!(git.packets, ours.packets);
    assert_eq!((git.code, git.stderr), (ours.code, ours.stderr));
    assert_eq!(ours.code, Some(0));
}

#[test]
fn command_line_equals_git() {
    let fx = fixture()
        .git(["init", "-q", "--bare", "bare.git"])
        .git(["init", "-q", "plain/sub"])
        .git(["clone", "-q", "--bare", "srv", "both"])
        .git(["clone", "-q", "--bare", "srv", "both.git"])
        .git(["-C", "both.git", "update-ref", "refs/heads/dotgit", "HEAD"])
        .git(["init", "-q", "--bare", "../home/in-home.git"]);
    if fx.is_skipped() {
        return;
    }
    let dir = fx.work_dir();
    let outside = fx.home_dir();
    let both = |args: &[&str], cwd: &Path| {
        let env = [("GIT_PROTOCOL", "version=2")];
        let git = decode(&fx, &run(&fx, fx.reference_git(), args, &env, cwd, b""));
        let ours = decode(&fx, &run(&fx, &rgit(), args, &env, cwd, b""));
        assert_eq!(git, ours, "{args:?}");
    };
    // Usage, outside a repository.
    for args in [
        &["upload-pack", "-h"][..],
        &["upload-pack", "--help-all"],
        &["upload-pack"],
        &["upload-pack", "a", "b"],
        &["upload-pack", "--bogus", "x"],
        &["upload-pack", "--timeout=x", "x"],
        &["upload-pack", "--timeout"],
    ] {
        both(args, &outside);
    }
    // Finding the repository.
    let absolute = dir.join("srv");
    let absolute = absolute.to_str().unwrap();
    for name in [
        "srv",
        "srv/",
        "./srv",
        "srv/.git",
        "bare",
        "bare.git",
        "plain",
        "plain/sub",
        "nonexistent",
        "srv/.git/objects",
        "both",
        absolute,
    ] {
        both(&["upload-pack", "--stateless-rpc", name], &dir);
        both(&["upload-pack", "--stateless-rpc", "--strict", name], &dir);
        both(
            &["upload-pack", "--advertise-refs", "--no-strict", name],
            &dir,
        );
    }
    // `~` is the home directory.
    both(&["upload-pack", "--advertise-refs", "~/in-home"], &dir);
    both(
        &[
            "upload-pack",
            "--advertise-refs",
            "--strict",
            "~/in-home.git",
        ],
        &dir,
    );
    both(
        &[
            "upload-pack",
            "--timeout=30",
            "--no-timeout",
            "--advertise-refs",
            "srv",
        ],
        &dir,
    );
}

#[test]
fn dashed_names_and_help_outside_a_repository() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let dir = fx.work_dir();
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let env = [("GIT_PROTOCOL", "version=2")];
    let direct = run(
        &fx,
        &rgit(),
        &["upload-pack", "--advertise-refs", "srv"],
        &env,
        &dir,
        b"",
    );
    assert!(direct.status.success());
    for name in ["git-upload-pack", "rgit-upload-pack"] {
        let link = bin.join(name);
        #[cfg(unix)]
        std::os::unix::fs::symlink(rgit(), &link).unwrap();
        #[cfg(not(unix))]
        std::fs::copy(rgit(), &link).unwrap();
        let out = run(&fx, &link, &["--advertise-refs", "srv"], &env, &dir, b"");
        assert_eq!(text(&out.stdout), text(&direct.stdout), "{name}");
        assert!(out.status.success(), "{name}: {}", text(&out.stderr));
        let help = run(&fx, &link, &["-h"], &[], &fx.home_dir(), b"");
        assert!(
            text(&help.stdout).starts_with("usage: git-upload-pack "),
            "{name}"
        );
    }
}

/// Run rgit with only `path` as `PATH` (no other git in sight).
fn run_with_path(fx: &Fixture, path: &Path, args: &[&str], protocol: Option<&str>) -> Output {
    let mut cmd = Command::new(rgit());
    isolated(fx, &mut cmd);
    cmd.env("PATH", path).args(args).current_dir(fx.work_dir());
    if let Some(p) = protocol {
        cmd.env("GIT_PROTOCOL", p);
    }
    cmd.stdin(Stdio::null()).output().unwrap()
}

#[test]
fn older_protocol_versions() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    // Protocol v0 and v1 are served here as well (tests/upload_pack_v0.rs),
    // also when another git-upload-pack is on PATH.
    let other = fx.work_dir().join("other-git");
    std::fs::create_dir_all(&other).unwrap();
    rgit_difftest::write_executable(
        other.join("git-upload-pack"),
        "#!/bin/sh\necho \"other upload-pack: $* ($GIT_PROTOCOL)\"\n",
    );
    for protocol in [None, Some("version=0"), Some("version=7")] {
        let out = run_with_path(
            &fx,
            &other,
            &["upload-pack", "--advertise-refs", "srv"],
            protocol,
        );
        assert_eq!(out.status.code(), Some(0));
        assert!(
            text(&out.stdout).contains(" HEAD\0multi_ack "),
            "{protocol:?}"
        );
        assert!(!text(&out.stdout).contains("version "), "{protocol:?}");
    }
    let out = run_with_path(
        &fx,
        &other,
        &["upload-pack", "--advertise-refs", "srv"],
        Some("version=1"),
    );
    assert!(text(&out.stdout).starts_with("000eversion 1\n"));
    // The highest version asked for counts.
    let out = run_with_path(
        &fx,
        &other,
        &["upload-pack", "--advertise-refs", "srv"],
        Some("version=1:version=2:other=x"),
    );
    assert!(text(&out.stdout).starts_with("000eversion 2\n"));
}

#[test]
fn namespaces_and_hidden_refs_equal_git() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx;
    for (name, target) in [
        ("refs/namespaces/ns/refs/heads/in-ns", "HEAD"),
        ("refs/namespaces/ns/refs/heads/hidden", "HEAD~1"),
        ("refs/namespaces/ns/refs/tags/t", "v1"),
        (
            "refs/namespaces/ns/refs/namespaces/inner/refs/heads/deep",
            "HEAD~2",
        ),
        ("refs/heads/outside", "HEAD"),
        ("refs/heads/hidden", "HEAD"),
        ("refs/hidden/a", "HEAD"),
        ("refs/hidden/b/c", "HEAD"),
        ("refs/hiddenx", "HEAD"),
    ] {
        fx = fx.git(["-C", "srv", "update-ref", name, target]);
    }
    for (name, target) in [
        (
            "refs/namespaces/ns/HEAD",
            "refs/namespaces/ns/refs/heads/in-ns",
        ),
        (
            "refs/namespaces/ns/refs/heads/sym",
            "refs/namespaces/ns/refs/heads/in-ns",
        ),
        (
            "refs/namespaces/ns/refs/namespaces/inner/HEAD",
            "refs/namespaces/ns/refs/namespaces/inner/refs/heads/unborn",
        ),
    ] {
        fx = fx.git(["-C", "srv", "symbolic-ref", name, target]);
    }
    let both = |namespace: Option<&str>, config: &[&str], lines: &[&str]| {
        let lines: Vec<String> = lines.iter().map(|l| (*l).to_owned()).collect();
        let serve = |program: &Path| {
            let mut args: Vec<&str> = Vec::new();
            for c in config {
                args.extend(["-c", c]);
            }
            args.extend(["upload-pack", "--stateless-rpc", "srv"]);
            let mut env = vec![("GIT_PROTOCOL", "version=2")];
            if let Some(ns) = namespace {
                env.push(("GIT_NAMESPACE", ns));
            }
            decode(
                &fx,
                &run(&fx, program, &args, &env, &fx.work_dir(), &request(&lines)),
            )
        };
        let (git, ours) = (serve(fx.reference_git()), serve(&rgit()));
        assert_eq!(git, ours, "{lines:?} in {namespace:?} with {config:?}");
        ours
    };
    let list = [
        "command=ls-refs",
        "0001",
        "symrefs",
        "peel",
        "unborn",
        "0000",
    ];
    // Namespaces. (git lists the HEAD of a namespace twice.)
    let plain = both(None, &[], &list);
    let ns = both(Some("ns"), &[], &list);
    assert!(ns.packets.len() < plain.packets.len());
    assert!(ns.packets.iter().any(|p| p.contains(" refs/heads/in-ns\n")));
    for namespace in ["ns/inner", "/ns//inner/", "nope"] {
        both(Some(namespace), &[], &list);
    }
    // Hidden references.
    let cases: &[(Option<&str>, &[&str])] = &[
        (None, &["transfer.hideRefs=refs/hidden"]),
        (None, &["transfer.hideRefs=refs/hidden/"]),
        (
            None,
            &[
                "transfer.hideRefs=refs/hidden",
                "transfer.hideRefs=!refs/hidden/b",
            ],
        ),
        (
            None,
            &[
                "transfer.hideRefs=!refs/hidden/b",
                "transfer.hideRefs=refs/hidden",
            ],
        ),
        (
            None,
            &[
                "uploadpack.hideRefs=refs/heads",
                "transfer.hideRefs=!refs/heads/main",
                "receive.hideRefs=refs/tags",
            ],
        ),
        (None, &["transfer.hideRefs=HEAD"]),
        (None, &["transfer.hideRefs=refs"]),
        (None, &["transfer.hideRefs=refs/heads/main"]),
        (None, &["transfer.hideRefs=^refs/heads/hidden"]),
        (Some("ns"), &["transfer.hideRefs=refs/heads/hidden"]),
        (
            Some("ns"),
            &["transfer.hideRefs=^refs/namespaces/ns/refs/heads/in-ns"],
        ),
        (
            Some("ns"),
            &["transfer.hideRefs=refs/namespaces/ns/refs/heads/in-ns"],
        ),
        (
            Some("ns"),
            &[
                "transfer.hideRefs=refs/heads",
                "transfer.hideRefs=!^refs/namespaces/ns/refs/heads/sym",
            ],
        ),
    ];
    for (namespace, config) in cases {
        both(*namespace, config, &list);
    }
    // `want-ref` sees the same references.
    let want = |name: &str| -> Vec<String> {
        fetch_request(&[
            format!("want-ref {name}"),
            "done".into(),
            "no-progress".into(),
        ])
    };
    let on = "uploadpack.allowRefInWant=true";
    let hide = "transfer.hideRefs=refs/heads/hidden";
    let cases: &[(Option<&str>, &[&str], &str)] = &[
        (Some("ns"), &[on], "refs/heads/in-ns"),
        (Some("ns"), &[on], "refs/heads/outside"),
        (Some("ns"), &[on], "HEAD"),
        (None, &[on], "refs/namespaces/ns/refs/heads/in-ns"),
        (Some("ns"), &[on, hide], "refs/heads/hidden"),
        (None, &[on, hide], "refs/heads/hidden"),
        (None, &[on, hide], "refs/namespaces/ns/refs/heads/hidden"),
    ];
    for (namespace, config, name) in cases {
        let lines = want(name);
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        both(*namespace, config, &refs);
    }
    // An object can be asked for by id whatever hides its reference, and
    // `deepen-not` names references of the repository as they are.
    let hidden = rev(&fx, "srv", "refs/heads/hidden");
    let lines = fetch_request(&[
        format!("want {hidden}"),
        "done".into(),
        "no-progress".into(),
    ]);
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    both(None, &["transfer.hideRefs=refs/heads"], &refs);
    for (namespace, config, not) in [
        (Some("ns"), &[][..], "refs/heads/hidden"),
        (Some("ns"), &[][..], "refs/tags/v1"),
        (None, &[hide][..], "side"),
    ] {
        let lines = fetch_request(&[
            format!("want {hidden}"),
            format!("deepen-not {not}"),
            "done".into(),
            "no-progress".into(),
        ]);
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        both(namespace, config, &refs);
    }
}

/// The trace2 events of a server besides the ones every command writes,
/// without what differs from run to run.
fn server_events(fx: &Fixture, program: &Path, config: &[&str], lines: &[String]) -> Vec<String> {
    let trace = fx.work_dir().join("trace2.events");
    let _ = std::fs::remove_file(&trace);
    let mut args: Vec<&str> = Vec::new();
    for c in config {
        args.extend(["-c", c]);
    }
    args.extend(["upload-pack", "--stateless-rpc", "srv"]);
    let env = [
        ("GIT_PROTOCOL", "version=2"),
        ("GIT_TRACE2_EVENT", trace.to_str().unwrap()),
    ];
    run(fx, program, &args, &env, &fx.work_dir(), &request(lines));
    std::fs::read_to_string(&trace)
        .unwrap_or_default()
        .lines()
        // The events of pack-objects have a session id with a slash.
        .filter(|l| l.contains("\"event\":\"data") && !l.contains("/20"))
        .map(|l| {
            let from = l.find("\"category\"").unwrap_or(0);
            l[from..].to_owned()
        })
        .collect()
}

#[test]
fn trace2_events_equal_git() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let r = |name: &str| rev(&fx, "srv", name);
    let (head, parent, root) = (r("HEAD"), r("HEAD~1"), r("HEAD~5"));
    let tail = || vec!["done".to_owned(), "no-progress".to_owned()];
    let cases: Vec<(&[&str], Vec<String>)> = vec![
        (&[], [vec![format!("want {head}")], tail()].concat()),
        (
            &[],
            vec![
                format!("want {head}"),
                format!("want {parent}"),
                format!("have {root}"),
                "have 1111111111111111111111111111111111111111".into(),
                "no-progress".into(),
            ],
        ),
        (
            &[],
            [
                vec![
                    format!("want {head}"),
                    "deepen 2".into(),
                    format!("shallow {parent}"),
                ],
                tail(),
            ]
            .concat(),
        ),
        (
            &[],
            [
                vec![
                    format!("want {head}"),
                    "deepen-since 1".into(),
                    "deepen-not side".into(),
                ],
                tail(),
            ]
            .concat(),
        ),
        (
            &[],
            [
                vec![
                    format!("want {head}"),
                    "deepen 1".into(),
                    "deepen-relative".into(),
                ],
                tail(),
            ]
            .concat(),
        ),
        (
            &["uploadpack.allowFilter=true"],
            [
                vec![format!("want {head}"), "filter blob:limit=1k".into()],
                tail(),
            ]
            .concat(),
        ),
        (
            &["uploadpack.allowRefInWant=true"],
            [vec!["want-ref refs/heads/main".to_owned()], tail()].concat(),
        ),
        (&[], vec![format!("have {head}")]),
    ];
    for (config, args) in cases {
        let lines = fetch_request(&args);
        let git = server_events(&fx, fx.reference_git(), config, &lines);
        let ours = server_events(&fx, &rgit(), config, &lines);
        assert_eq!(git, ours, "{args:?}");
        assert!(
            ours.iter().any(|l| l.contains("\"key\":\"fetch-info\"")),
            "{ours:?}"
        );
        assert!(ours[0].contains("\"key\":\"negotiated-version\",\"value\":\"2\""));
    }
    // The client's session id, when the server advertises its own.
    let lines: Vec<String> = ["command=ls-refs", "session-id=client-1", "0000"]
        .iter()
        .map(|l| (*l).to_owned())
        .collect();
    let on: &[&str] = &["transfer.advertiseSID=true"];
    let git = server_events(&fx, fx.reference_git(), on, &lines);
    let ours = server_events(&fx, &rgit(), on, &lines);
    assert_eq!(git, ours);
    assert!(
        ours.iter()
            .any(|l| l.contains("\"key\":\"client-sid\",\"value\":\"client-1\""))
    );
}

#[test]
fn keep_alive_while_pack_objects_is_silent() {
    let fx = fixture().write_executable("slow", "#!/bin/sh\nsleep 3\nexec \"$@\"\n");
    if fx.is_skipped() {
        return;
    }
    let dir = fx.work_dir();
    let head = rev(&fx, "srv", "HEAD");
    let hook = format!("uploadpack.packObjectsHook={}", dir.join("slow").display());
    let lines = fetch_request(&[format!("want {head}"), "done".into(), "no-progress".into()]);
    let serve = |keep_alive: &str| {
        let args = [
            "-c",
            hook.as_str(),
            "-c",
            keep_alive,
            "upload-pack",
            "--stateless-rpc",
            "srv",
        ];
        let env = [("GIT_PROTOCOL", "version=2")];
        run(&fx, &rgit(), &args, &env, &dir, &request(&lines))
    };
    // An empty packet on sideband 1 while nothing else is to send.
    let out = serve("uploadpack.keepAlive=1");
    let raw = out.stdout.clone();
    let keep_alives = raw.windows(5).filter(|w| w == b"0005\x01").count();
    assert!(
        keep_alives >= 1,
        "no keep-alive packet in {} bytes",
        raw.len()
    );
    let reply = decode(&fx, &out);
    assert_eq!(reply.code, Some(0));
    assert_eq!(reply.packs.len(), 1, "{:?}", reply.packets);
    // None when switched off.
    let out = serve("uploadpack.keepAlive=0");
    assert!(!out.stdout.windows(9).any(|w| w == b"0005\x010005"));
    assert_eq!(decode(&fx, &out).packs.len(), 1);
}

#[cfg(unix)]
#[test]
fn timeout_ends_an_idle_connection() {
    use std::os::unix::process::ExitStatusExt;
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    // A client that says nothing.
    let silent = |program: &Path| {
        let mut cmd = Command::new(program);
        isolated(&fx, &mut cmd);
        let mut child = cmd
            .args(["upload-pack", "--timeout=1", "srv"])
            .env("GIT_PROTOCOL", "version=2")
            .current_dir(fx.work_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let started = std::time::Instant::now();
        let status = child.wait().unwrap();
        (status.signal(), status.code(), started.elapsed())
    };
    let (git_signal, git_code, _) = silent(fx.reference_git());
    let (signal, code, elapsed) = silent(&rgit());
    assert_eq!((signal, code), (git_signal, git_code));
    assert!(elapsed < std::time::Duration::from_secs(20), "{elapsed:?}");
}

#[test]
fn failing_pack_objects_is_reported() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    // Remove a blob the pack needs.
    let blob = rev(&fx, "srv", "HEAD:last");
    let path = fx
        .work_dir()
        .join("srv/.git/objects")
        .join(&blob[..2])
        .join(&blob[2..]);
    std::fs::remove_file(path).unwrap();
    let head = rev(&fx, "srv", "HEAD");
    let lines = fetch_request(&[format!("want {head}"), "done".into(), "no-progress".into()]);
    let git = exchange(&fx, fx.reference_git(), &[], "srv", true, &lines);
    let ours = exchange(&fx, &rgit(), &[], "srv", true, &lines);
    assert_eq!(ours.code, Some(128));
    assert_eq!(git.stderr, ours.stderr);
    assert_eq!(git.packets, ours.packets);
    assert!(
        ours.packets
            .iter()
            .any(|p| p
                == "band 3: aborting due to possible repository corruption on the remote side."),
        "{ours:?}"
    );
}

#[test]
fn pack_objects_hook() {
    let fx = fixture().write_executable(
        "hook",
        "#!/bin/sh\necho \"$*\" >\"$HOOK_LOG\"\ncat >\"$HOOK_LOG.in\"\n\"$@\" <\"$HOOK_LOG.in\"\n",
    );
    if fx.is_skipped() {
        return;
    }
    let dir = fx.work_dir();
    let hook = dir.join("hook");
    let hook = hook.to_str().unwrap();
    let log = dir.join("hook.log");
    let head = rev(&fx, "srv", "HEAD");
    let parent = rev(&fx, "srv", "HEAD~1");
    let serve = |program: &Path, config: &[String], args: &[String]| {
        let _ = std::fs::remove_file(&log);
        let mut argv: Vec<&str> = Vec::new();
        for c in config {
            argv.extend(["-c", c.as_str()]);
        }
        argv.extend(["upload-pack", "--stateless-rpc", "srv"]);
        let env = [
            ("GIT_PROTOCOL", "version=2"),
            ("HOOK_LOG", log.to_str().unwrap()),
        ];
        let out = run(
            &fx,
            program,
            &argv,
            &env,
            &dir,
            &request(&fetch_request(args)),
        );
        let reply = decode(&fx, &out);
        let logged = std::fs::read_to_string(&log).ok();
        let input = std::fs::read_to_string(dir.join("hook.log.in")).ok();
        (reply, logged, input)
    };
    let hooked = vec![format!("uploadpack.packObjectsHook={hook}")];
    let requests: Vec<Vec<String>> = vec![
        vec![format!("want {head}"), "done".into(), "no-progress".into()],
        vec![
            format!("want {head}"),
            format!("have {parent}"),
            "thin-pack".into(),
            "ofs-delta".into(),
            "include-tag".into(),
            "done".into(),
        ],
        vec![
            format!("want {head}"),
            format!("shallow {parent}"),
            "deepen 3".into(),
            "done".into(),
            "no-progress".into(),
        ],
    ];
    for args in &requests {
        // The hook gets the command line and input git would give it.
        let git = serve(fx.reference_git(), &hooked, args);
        let ours = serve(&rgit(), &hooked, args);
        assert_eq!(git.1, ours.1, "arguments for {args:?}");
        assert_eq!(git.2, ours.2, "input for {args:?}");
        assert!(
            ours.1.as_deref().is_some_and(|l| l.starts_with("git ")),
            "{ours:?}"
        );
        if !args.contains(&"thin-pack".to_owned()) {
            assert_eq!(git.0, ours.0);
        }
    }
    // The repository's own configuration cannot name a hook.
    let set = ["-C", "srv", "config", "uploadpack.packObjectsHook", hook];
    assert!(
        run(&fx, fx.reference_git(), &set, &[], &dir, b"")
            .status
            .success()
    );
    let (reply, logged, _) = serve(&rgit(), &[], &requests[0]);
    assert_eq!(reply.code, Some(0));
    assert_eq!(logged, None);
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

/// The quoted `--upload-pack` commands of the two servers.
fn servers(fx: &Fixture) -> [(&'static str, String); 2] {
    [
        (
            "git",
            format!("'{}' upload-pack", fx.reference_git().display()),
        ),
        ("rgit", format!("'{}' upload-pack", rgit().display())),
    ]
}

/// Run `client <args>` with each server in a copy of `base` (a directory
/// below the work directory, or none) and require the same outcome and
/// state of `repo` afterwards. In the arguments `{UP}` stands for the
/// server command and `{WORK}` for the fixture's work directory; `env`
/// is added to the client's environment.
#[track_caller]
fn flow_with(
    fx: &Fixture,
    client: &Path,
    base: Option<&str>,
    cwd: &str,
    repo: &str,
    args: &[&str],
    env: &[(&str, &str)],
) -> String {
    let work = fx.work_dir();
    let mut results = Vec::new();
    for (name, up) in servers(fx) {
        let dir = work.join(format!("flow-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        match base {
            Some(b) => copy_dir(&work.join(b), &dir),
            None => std::fs::create_dir_all(&dir).unwrap(),
        }
        let args: Vec<String> = args
            .iter()
            .map(|a| {
                a.replace("{UP}", &up)
                    .replace("{WORK}", work.to_str().unwrap())
            })
            .collect();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = run(fx, client, &args, env, &dir.join(cwd), b"");
        let st = state(fx, &dir.join(repo));
        results.push((out.status.code(), st, text(&out.stderr)));
    }
    let (git, ours) = (&results[0], &results[1]);
    assert_eq!(
        (git.0, &git.1),
        (ours.0, &ours.1),
        "{args:?}\ngit stderr: {}\nrgit stderr: {}",
        git.2,
        ours.2
    );
    assert_eq!(ours.0, Some(0), "{args:?} failed: {}", ours.2);
    assert!(ours.1.contains("fsck: Some(0) []"), "{args:?}: {}", ours.1);
    ours.1.clone()
}

#[track_caller]
fn flow(
    fx: &Fixture,
    client: &Path,
    base: Option<&str>,
    cwd: &str,
    repo: &str,
    args: &[&str],
) -> String {
    flow_with(fx, client, base, cwd, repo, args, &[])
}

fn global(fx: Fixture, key: &str, value: &str) -> Fixture {
    fx.git(["config", "--global", key, value])
}

#[test]
fn reference_client_clones() {
    let fx = fixture()
        .git(["init", "-q", "unborn"])
        .git(["clone", "-q", "srv", "detached"])
        .git(["-C", "detached", "checkout", "-q", "--detach", "HEAD~1"]);
    let fx = clone_from(fx, &["--depth", "3"], "srv", "shallow");
    if fx.is_skipped() {
        return;
    }
    let git = fx.reference_git().to_path_buf();
    let url = "file://{WORK}/srv";
    let clone = |extra: &[&str], from: &str| {
        let mut args = vec!["clone", "-q", "--upload-pack", "{UP}"];
        args.extend_from_slice(extra);
        args.extend([from, "c"]);
        flow(&fx, &git, None, "", "c", &args)
    };
    let full = clone(&[], url);
    assert!(full.contains("refs/tags/v2 "));
    clone(&["--bare"], url);
    clone(&["--mirror"], url);
    clone(&["-b", "side", "--single-branch"], url);
    clone(&["--no-tags"], url);
    // Shallow.
    let one = clone(&["--depth", "1"], url);
    assert!(!one.contains("shallow: []"));
    clone(&["--depth", "2", "--no-single-branch"], url);
    clone(&["--depth", "100"], url);
    let since = text(&fx.git_stdout(["-C", "srv", "log", "-1", "--format=%ct", "HEAD~2"]));
    clone(&[&format!("--shallow-since={}", since.trim())], url);
    clone(&["--shallow-exclude=side"], url);
    clone(&["--shallow-exclude=v1", "--no-single-branch"], url);
    // Other repositories.
    clone(&[], "file://{WORK}/unborn");
    clone(&[], "file://{WORK}/detached");
    let from_shallow = clone(&[], "file://{WORK}/shallow");
    assert!(!from_shallow.contains("shallow: []"));
    // A local path, with the transport forced.
    clone(&["--no-local"], "{WORK}/srv");
}

#[test]
fn reference_client_partial_clones() {
    let fx = global(fixture(), "uploadpack.allowFilter", "true");
    if fx.is_skipped() {
        return;
    }
    let git = fx.reference_git().to_path_buf();
    let url = "file://{WORK}/srv";
    for filter in [
        "blob:none",
        "blob:limit=5",
        "tree:0",
        "tree:2",
        "object:type=commit",
        "combine:tree:3+blob:none",
    ] {
        let filter = format!("--filter={filter}");
        let st = flow(
            &fx,
            &git,
            None,
            "",
            "c",
            &[
                "clone",
                "-q",
                "--no-checkout",
                "--upload-pack",
                "{UP}",
                &filter,
                url,
                "c",
            ],
        );
        // Something was left out.
        assert!(st.contains('?'), "{filter}: {st}");
    }
    flow(
        &fx,
        &git,
        None,
        "",
        "c",
        &[
            "clone",
            "-q",
            "--no-checkout",
            "--upload-pack",
            "{UP}",
            "--filter=blob:none",
            "--depth",
            "2",
            url,
            "c",
        ],
    );
    // Checking out fetches the blobs that are missing, through the same
    // server.
    let st = flow(
        &fx,
        &git,
        None,
        "",
        "c",
        &[
            "-c",
            "remote.origin.uploadpack={UP}",
            "clone",
            "-q",
            "--upload-pack",
            "{UP}",
            "--filter=blob:none",
            url,
            "c",
        ],
    );
    assert!(st.contains("refs/heads/main "));
}

/// `base/c`: a clone of `srv` from before its last three commits on
/// `main`, without the tag `v2`.
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
fn reference_client_fetches() {
    let fx = with_old_clone(fixture());
    if fx.is_skipped() {
        return;
    }
    let git = fx.reference_git().to_path_buf();
    let fetch_with = |config: &[&str], args: &[&str]| {
        let mut argv: Vec<&str> = Vec::new();
        for c in config {
            argv.extend(["-c", c]);
        }
        argv.extend(["fetch", "-q", "--upload-pack", "{UP}"]);
        argv.extend_from_slice(args);
        flow(&fx, &git, Some("base"), "c", "c", &argv)
    };
    let fetch = |args: &[&str]| fetch_with(&[], args);
    let all = fetch(&["origin"]);
    assert!(all.contains("refs/tags/v2 "));
    fetch(&["--no-tags", "origin"]);
    fetch(&["--tags", "origin"]);
    fetch(&["origin", "tag", "v2"]);
    fetch(&["origin", "side:refs/heads/side2"]);
    let sha = rev(&fx, "srv", "HEAD~1");
    fetch(&["origin", &sha]);
    fetch_with(&["fetch.negotiationAlgorithm=noop"], &["origin"]);
    fetch_with(&["fetch.negotiationAlgorithm=skipping"], &["origin"]);
    fetch(&["--depth=1", "origin"]);
    let pulled = flow(
        &fx,
        &git,
        Some("base"),
        "c",
        "c",
        &[
            "pull",
            "-q",
            "--ff-only",
            "--upload-pack",
            "{UP}",
            "origin",
            "main",
        ],
    );
    assert!(pulled.contains(&format!("refs/heads/main {}", rev(&fx, "srv", "main"))));
}

#[test]
fn reference_client_deepens() {
    let fx = clone_from(fixture(), &["--depth", "2"], "srv", "base/c");
    if fx.is_skipped() {
        return;
    }
    // New history on the server after the shallow clone.
    let fx = commit(fx, "srv", "newer", "newer\n");
    let fx = commit(fx, "srv", "newest", "newest\n");
    let git = fx.reference_git().to_path_buf();
    let fetch = |args: &[&str]| {
        let mut argv = vec!["fetch", "-q", "--upload-pack", "{UP}"];
        argv.extend_from_slice(args);
        flow(&fx, &git, Some("base"), "c", "c", &argv)
    };
    let plain = fetch(&["origin"]);
    assert!(!plain.contains("shallow: []"));
    fetch(&["--deepen=1", "origin"]);
    fetch(&["--deepen=3", "origin"]);
    fetch(&["--depth=1", "origin"]);
    fetch(&["--depth=3", "origin"]);
    fetch(&["--depth=5", "origin"]);
    let complete = fetch(&["--unshallow", "origin"]);
    assert!(complete.contains("shallow: []"), "{complete}");
    let since = text(&fx.git_stdout(["-C", "srv", "log", "-1", "--format=%ct", "HEAD~5"]));
    fetch(&[&format!("--shallow-since={}", since.trim()), "origin"]);
    fetch(&["--shallow-exclude=v1", "origin"]);
    fetch(&["--update-shallow", "origin", "side:refs/heads/side2"]);
}

#[test]
fn reference_client_wants_refs() {
    let fx = with_old_clone(global(fixture(), "uploadpack.allowRefInWant", "true"));
    if fx.is_skipped() {
        return;
    }
    let git = fx.reference_git().to_path_buf();
    let url = "file://{WORK}/srv";
    flow(
        &fx,
        &git,
        None,
        "",
        "c",
        &["clone", "-q", "--upload-pack", "{UP}", url, "c"],
    );
    flow(
        &fx,
        &git,
        None,
        "",
        "c",
        &[
            "clone",
            "-q",
            "--upload-pack",
            "{UP}",
            "-b",
            "side",
            "--single-branch",
            url,
            "c",
        ],
    );
    flow(
        &fx,
        &git,
        Some("base"),
        "c",
        "c",
        &[
            "fetch",
            "-q",
            "--upload-pack",
            "{UP}",
            "origin",
            "refs/heads/main:refs/x/main",
            "refs/tags/v1:refs/x/v1",
        ],
    );
    // The `want-ref` lines are really sent.
    let work = fx.work_dir();
    let up = &servers(&fx)[1].1;
    let trace = run(
        &fx,
        &git,
        &["fetch", "--upload-pack", up, "origin", "main"],
        &[("GIT_TRACE_PACKET", "1")],
        &work.join("base/c"),
        b"",
    );
    let trace = text(&trace.stderr);
    assert!(trace.contains("fetch> want-ref refs/heads/main"), "{trace}");
    assert!(trace.contains("fetch< wanted-refs"), "{trace}");
    // A shallow fetch by reference name: the `shallow-info` section comes
    // before `wanted-refs`, as gitprotocol-v2 has it. (git 2.56 writes them
    // the other way round and its own client then gives up, so there is
    // nothing to compare with.)
    let dst = work.join("shallow-by-ref");
    let cloned = run(
        &fx,
        &git,
        &[
            "clone",
            "-q",
            "--depth",
            "1",
            "--upload-pack",
            up,
            &format!("file://{}/srv", work.display()),
            dst.to_str().unwrap(),
        ],
        &[],
        &work,
        b"",
    );
    assert!(cloned.status.success(), "{}", text(&cloned.stderr));
    let st = state(&fx, &dst);
    assert!(
        st.contains("fsck: Some(0) []") && !st.contains("shallow: []"),
        "{st}"
    );
}

#[test]
fn rgit_client_against_both_servers() {
    let fx = with_old_clone(fixture());
    if fx.is_skipped() {
        return;
    }
    let client = rgit();
    let url = "file://{WORK}/srv";
    let clone = |extra: &[&str]| {
        let mut args = vec!["clone", "-q", "--upload-pack", "{UP}"];
        args.extend_from_slice(extra);
        args.extend([url, "c"]);
        flow(&fx, &client, None, "", "c", &args)
    };
    clone(&[]);
    clone(&["--bare"]);
    clone(&["-b", "side", "--single-branch"]);
    clone(&["--no-tags"]);
    for args in [
        &["fetch", "-q", "--upload-pack", "{UP}", "origin"][..],
        &[
            "fetch",
            "-q",
            "--upload-pack",
            "{UP}",
            "--no-tags",
            "origin",
        ],
        &[
            "fetch",
            "-q",
            "--upload-pack",
            "{UP}",
            "origin",
            "tag",
            "v2",
        ],
        &[
            "pull",
            "-q",
            "--ff-only",
            "--upload-pack",
            "{UP}",
            "origin",
            "main",
        ],
    ] {
        flow(&fx, &client, Some("base"), "c", "c", args);
    }
    // ls-remote prints the same through both servers.
    let listed: Vec<String> = servers(&fx)
        .iter()
        .map(|(_, up)| {
            let url = format!("file://{}/srv", fx.work_dir().display());
            let args = ["ls-remote", "--upload-pack", up.as_str(), url.as_str()];
            let out = run(&fx, &client, &args, &[], &fx.work_dir(), b"");
            assert!(out.status.success(), "{}", text(&out.stderr));
            text(&out.stdout)
        })
        .collect();
    assert_eq!(listed[0], listed[1]);
    assert!(listed[1].contains("refs/tags/v2^{}"), "{}", listed[1]);
}

#[test]
fn over_ssh() {
    // The fake ssh runs the command it is given on this machine.
    let fx = with_old_clone(fixture()).write_executable(
        "ssh",
        "#!/bin/sh\nwhile test $# -gt 2; do shift; done\nshift\nexec sh -c \"$1\"\n",
    );
    if fx.is_skipped() {
        return;
    }
    let git = fx.reference_git().to_path_buf();
    let ssh = fx.work_dir().join("ssh");
    let env = [
        ("GIT_SSH", ssh.to_str().unwrap()),
        ("GIT_SSH_VARIANT", "ssh"),
    ];
    let url = "ssh://localhost{WORK}/srv";
    flow_with(
        &fx,
        &git,
        None,
        "",
        "c",
        &["clone", "-q", "--upload-pack", "{UP}", url, "c"],
        &env,
    );
    flow_with(
        &fx,
        &git,
        None,
        "",
        "c",
        &[
            "clone",
            "-q",
            "--depth",
            "2",
            "--upload-pack",
            "{UP}",
            url,
            "c",
        ],
        &env,
    );
    flow_with(
        &fx,
        &git,
        Some("base"),
        "c",
        "c",
        &[
            "fetch",
            "-q",
            "--upload-pack",
            "{UP}",
            url,
            "refs/heads/*:refs/remotes/ssh/*",
        ],
        &env,
    );
}

#[test]
fn over_http() {
    let fx = with_old_clone(global(fixture(), "uploadpack.allowFilter", "true"));
    if fx.is_skipped() {
        return;
    }
    let git = fx.reference_git().to_path_buf();
    let work = fx.work_dir();
    // `git http-backend` runs `git upload-pack` from its exec path: one
    // exec path per server, with `git` being the reference git or rgit.
    let exec_path = PathBuf::from(text(&fx.git_stdout(["--exec-path"])).trim());
    let backend = ["git-http-backend", "git-http-backend.exe"]
        .iter()
        .map(|n| exec_path.join(n))
        .find(|p| p.is_file());
    let Some(backend) = backend else {
        eprintln!("skipping: no git-http-backend in {}", exec_path.display());
        return;
    };
    let mut results = Vec::new();
    for (name, program) in [("git", git.clone()), ("rgit", rgit())] {
        let bin = work.join(format!("exec-{name}"));
        std::fs::create_dir_all(&bin).unwrap();
        let log = work.join(format!("exec-{name}.log"));
        rgit_difftest::write_executable(
            bin.join("git"),
            format!(
                "#!/bin/sh\necho \"$*\" >>'{}'\nexec '{}' \"$@\"\n",
                log.display(),
                program.display()
            ),
        );
        #[cfg(unix)]
        std::os::unix::fs::symlink(&backend, bin.join("git-http-backend")).unwrap();
        #[cfg(not(unix))]
        std::fs::copy(&backend, bin.join("git-http-backend")).unwrap();
        let base = start_http_with(&fx, vec![("GIT_EXEC_PATH".into(), bin.into_os_string())]);
        let dir = work.join(format!("http-{name}"));
        copy_dir(&work.join("base"), &dir);
        let mut states = Vec::new();
        let url = format!("{base}/srv");
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
        ];
        for (args, cwd, repo) in steps {
            let out = run(&fx, &git, &args, &[], &dir.join(cwd), b"");
            assert!(
                out.status.success(),
                "{name}: {args:?}: {}",
                text(&out.stderr)
            );
            let st = state(&fx, &dir.join(repo));
            assert!(st.contains("fsck: Some(0) []"), "{name}: {args:?}: {st}");
            states.push(st);
        }
        let log = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            log.contains("upload-pack --stateless-rpc"),
            "{name} was not asked to serve: {log}"
        );
        results.push(states);
    }
    assert_eq!(results[0], results[1]);
}
