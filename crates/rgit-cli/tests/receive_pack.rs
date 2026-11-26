// SPDX-License-Identifier: MIT
//! Differential tests of `receive-pack` (#69, #175): the same request
//! (commands, push options and pack bytes, as `send-pack` writes them) fed
//! to `git receive-pack` and `rgit receive-pack`; the answer, the messages
//! and the resulting references and objects must be identical. Also
//! `git push` (the reference client) against `rgit receive-pack`.

use std::path::Path;
use std::process::{Command, Stdio};

use rgit_difftest::{Check, Fixture};

const CHECKS: [Check; 6] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Refs,
    Check::Reflogs,
    Check::Objects,
];

const Z: &str = "0000000000000000000000000000000000000000";

/// Capabilities, commands, push options and pack bytes of one request.
type Case<'a> = (&'a str, Vec<String>, &'a [&'a str], Vec<u8>);

fn pkt(s: &[u8]) -> Vec<u8> {
    let mut v = format!("{:04x}", s.len() + 4).into_bytes();
    v.extend_from_slice(s);
    v
}

/// A request as `send-pack` writes it.
fn request(caps: &str, commands: &[String], options: &[&str], pack: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, c) in commands.iter().enumerate() {
        let mut line = c.clone().into_bytes();
        if i == 0 {
            line.push(0);
            line.extend_from_slice(caps.as_bytes());
        }
        out.extend(pkt(&line));
    }
    out.extend_from_slice(b"0000");
    if caps.split(' ').any(|c| c == "push-options") {
        for o in options {
            out.extend(pkt(o.as_bytes()));
        }
        out.extend_from_slice(b"0000");
    }
    out.extend_from_slice(pack);
    out
}

/// A pack of `revs` (pack-objects `--revs` input) from the repository
/// `src`, made by the reference git.
fn pack(fx: &Fixture, revs: &str) -> Vec<u8> {
    std::fs::write(fx.work_dir().join("revs"), format!("{revs}\n")).unwrap();
    fx.git_stdout([
        "-C",
        "src",
        "-c",
        "alias.mkpack=!git pack-objects --stdout --revs -q <../revs",
        "mkpack",
    ])
}

fn rev(fx: &Fixture, name: &str) -> String {
    String::from_utf8(fx.git_stdout(["-C", "src", "rev-parse", name]))
        .unwrap()
        .trim()
        .to_string()
}

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main", "src"])
        .write("src/a", "1\n")
        .git(["-C", "src", "add", "a"])
        .git(["-C", "src", "commit", "-q", "-m", "one"])
        .tick()
        .write("src/a", "2\n")
        .git(["-C", "src", "commit", "-q", "-am", "two"])
        .tick()
        .write("src/b", "3\n")
        .git(["-C", "src", "add", "b"])
        .git(["-C", "src", "commit", "-q", "-m", "three"])
        .git(["-C", "src", "tag", "-a", "-m", "tag", "v1", "HEAD~1"])
        .git(["init", "-q", "--bare", "srv.git"])
        .git([
            "-C",
            "src",
            "push",
            "-q",
            "../srv.git",
            "HEAD~2:refs/heads/main",
            "HEAD~2:refs/heads/other",
            "HEAD~2:refs/heads/sym-target",
        ])
        .git([
            "-C",
            "srv.git",
            "symbolic-ref",
            "refs/heads/sym",
            "refs/heads/sym-target",
        ])
}

#[test]
fn receive_pack_requests_same_as_git() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let (c1, c2, c3) = (rev(&fx, "HEAD~2"), rev(&fx, "HEAD~1"), rev(&fx, "HEAD"));
    let tag = rev(&fx, "v1");
    let p23 = pack(&fx, &format!("{c3}\n^{c1}"));
    let p2 = pack(&fx, &format!("{c2}\n^{c1}"));
    let ptag = pack(&fx, &format!("{tag}\n^{c1}"));
    let cmd = |o: &str, n: &str, r: &str| format!("{o} {n} {r}");
    let cases: Vec<Case<'_>> = vec![
        // Fast-forward with a sideband.
        (
            "report-status side-band-64k quiet",
            vec![cmd(&c1, &c3, "refs/heads/main")],
            &[],
            p23.clone(),
        ),
        // Wrong old value, a creation, a duplicate.
        (
            "report-status",
            vec![
                cmd(&c2, &c3, "refs/heads/other"),
                cmd(Z, &c2, "refs/heads/new"),
            ],
            &[],
            p23.clone(),
        ),
        (
            "report-status-v2",
            vec![cmd(Z, &c2, "refs/heads/d"), cmd(Z, &c2, "refs/heads/d")],
            &[],
            p2.clone(),
        ),
        // Funny names, deletions of missing and existing refs.
        (
            "report-status",
            vec![
                cmd(Z, &c2, "HEAD"),
                cmd(Z, &c2, "refs/x"),
                cmd(Z, &c2, "refs/heads/a..b"),
                cmd(Z, Z, "refs/heads/nothere"),
                cmd(&c1, Z, "refs/heads/nothere2"),
                cmd(&c1, Z, "refs/heads/other"),
            ],
            &[],
            p2.clone(),
        ),
        // Atomic: one bad update fails all.
        (
            "report-status atomic",
            vec![
                cmd(Z, &c3, "refs/heads/at"),
                cmd(&c1, &c3, "refs/heads/main"),
            ],
            &[],
            p23.clone(),
        ),
        // A symbolic ref and its target with different changes.
        (
            "report-status",
            vec![
                cmd(&c1, &c2, "refs/heads/sym"),
                cmd(&c1, &c3, "refs/heads/sym-target"),
            ],
            &[],
            p23.clone(),
        ),
        // An annotated tag; a missing object.
        (
            "report-status",
            vec![
                cmd(Z, &tag, "refs/tags/v1"),
                cmd(
                    Z,
                    "1234567890123456789012345678901234567890",
                    "refs/heads/m",
                ),
            ],
            &[],
            ptag.clone(),
        ),
        // No report requested; a bad pack header; garbage.
        ("", vec![cmd(Z, &c2, "refs/heads/q")], &[], p2.clone()),
        (
            "report-status",
            vec![cmd(Z, &c2, "refs/heads/q2")],
            &[],
            b"PACX".to_vec(),
        ),
        (
            "report-status",
            vec!["garbage".to_string()],
            &[],
            Vec::new(),
        ),
        (
            "report-status object-format=sha256",
            vec![cmd(Z, &c2, "refs/heads/q3")],
            &[],
            p2.clone(),
        ),
    ];
    for (caps, commands, options, data) in cases {
        let input = request(caps, &commands, options, &data);
        fx.compare(["receive-pack", "srv.git"])
            .stdin(input)
            .env("GIT_USER_AGENT", "test-agent")
            .repo("srv.git")
            .checks(CHECKS)
            .run();
    }
    // Advertisement only, plain and stateless.
    for argv in [
        &["receive-pack", "--advertise-refs", "srv.git"][..],
        &[
            "receive-pack",
            "--stateless-rpc",
            "--http-backend-info-refs",
            "srv.git",
        ],
        &["receive-pack", "srv"],
        &["receive-pack", "nonexistent"],
        &["receive-pack"],
        &["receive-pack", "a", "b"],
        &["receive-pack", "--bogus", "srv.git"],
        &["receive-pack", "-h"],
        &["receive-pack", "--help-all"],
    ] {
        fx.compare(argv)
            .env("GIT_USER_AGENT", "test-agent")
            .checks(Check::OUTPUT)
            .run();
    }
    // Protocol v1 adds a version line; v2 is not spoken by receive-pack.
    for protocol in ["version=1", "version=2"] {
        fx.compare(["receive-pack", "--advertise-refs", "srv.git"])
            .env("GIT_USER_AGENT", "test-agent")
            .env("GIT_PROTOCOL", protocol)
            .checks(Check::OUTPUT)
            .run();
    }
}

#[test]
fn receive_pack_policies_same_as_git() {
    let mut fx = fixture()
        .git([
            "-C",
            "srv.git",
            "config",
            "receive.denyNonFastForwards",
            "true",
        ])
        .git(["-C", "srv.git", "config", "receive.denyDeletes", "true"])
        .git([
            "-C",
            "srv.git",
            "config",
            "receive.hideRefs",
            "refs/heads/sym",
        ])
        .git(["-C", "srv.git", "config", "receive.unpackLimit", "1"]);
    if fx.is_skipped() {
        return;
    }
    let (c1, c2, c3) = (rev(&fx, "HEAD~2"), rev(&fx, "HEAD~1"), rev(&fx, "HEAD"));
    let p23 = pack(&fx, &format!("{c3}\n^{c1}"));
    let cmd = |o: &str, n: &str, r: &str| format!("{o} {n} {r}");
    let input = request(
        "report-status side-band-64k",
        &[
            cmd(&c1, &c3, "refs/heads/main"),
            cmd(&c1, Z, "refs/heads/other"),
            cmd(Z, &c3, "refs/heads/sym-target/x"),
            cmd(Z, &c3, "refs/heads/sym"),
            cmd(Z, &c2, "refs/heads/kept"),
        ],
        &[],
        &p23,
    );
    fx.compare(["receive-pack", "srv.git"])
        .stdin(input)
        .env("GIT_USER_AGENT", "test-agent")
        .repo("srv.git")
        .checks(CHECKS)
        .run();
    // Not a fast-forward any more.
    let p1 = pack(&fx, &c1);
    let input = request(
        "report-status",
        &[cmd(&c3, &c1, "refs/heads/main")],
        &[],
        &p1,
    );
    fx.compare(["receive-pack", "srv.git"])
        .stdin(input)
        .env("GIT_USER_AGENT", "test-agent")
        .repo("srv.git")
        .checks(CHECKS)
        .run();
}

#[test]
fn receive_pack_hooks_same_as_git() {
    let hook = |name: &str| {
        format!(
            "#!/bin/sh\necho \"{name} $*\" >&2\nwhile read -r o n r; do echo \"{name}< $o $n $r\" >&2; done\n\
             echo \"{name} options=${{GIT_PUSH_OPTION_COUNT-unset}} ${{GIT_PUSH_OPTION_0-}}\" >&2\n\
             test -z \"$GIT_QUARANTINE_PATH\" || echo \"{name} quarantined\" >&2\n\
             exit ${{FAIL_{}:-0}}\n",
            name.replace('-', "_").to_uppercase()
        )
    };
    let mut fx = fixture()
        .git([
            "-C",
            "srv.git",
            "config",
            "receive.advertisePushOptions",
            "true",
        ])
        .write_executable("srv.git/hooks/pre-receive", hook("pre-receive"))
        .write_executable("srv.git/hooks/update", hook("update"))
        .write_executable("srv.git/hooks/post-receive", hook("post-receive"))
        .write_executable("srv.git/hooks/post-update", hook("post-update"));
    if fx.is_skipped() {
        return;
    }
    let (c1, c2, c3) = (rev(&fx, "HEAD~2"), rev(&fx, "HEAD~1"), rev(&fx, "HEAD"));
    let p23 = pack(&fx, &format!("{c3}\n^{c1}"));
    let cmd = |o: &str, n: &str, r: &str| format!("{o} {n} {r}");
    for (caps, fail) in [
        ("report-status push-options", ""),
        ("report-status push-options", "FAIL_UPDATE"),
        ("report-status", "FAIL_PRE_RECEIVE"),
    ] {
        let input = request(
            caps,
            &[
                cmd(&c1, &c3, "refs/heads/main"),
                cmd(Z, &c2, "refs/heads/n"),
            ],
            &["opt=1"],
            &p23,
        );
        let mut cmp = fx
            .compare(["receive-pack", "srv.git"])
            .stdin(input)
            .env("GIT_USER_AGENT", "test-agent")
            .repo("srv.git")
            .checks(CHECKS);
        if !fail.is_empty() {
            cmp = cmp.env(fail, "1");
        }
        cmp.run();
    }
}

/// Runs the reference git with an isolated configuration.
fn git(reference: &Path, home: &Path, dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(reference)
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env_remove("GIT_DIR")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// `git push` (the reference client) into a non-bare repository through
/// `git receive-pack` and through `rgit receive-pack`: same output, same
/// references, same work tree (`updateInstead`).
#[test]
fn push_from_git_to_rgit_receive_pack() {
    push_to_rgit_receive_pack("sha1");
}

/// The same in SHA-256 repositories.
#[test]
fn push_from_git_to_rgit_receive_pack_sha256() {
    push_to_rgit_receive_pack("sha256");
}

fn push_to_rgit_receive_pack(format: &str) {
    let Some(reference) = rgit_difftest::env::reference_git() else {
        return;
    };
    let rgit = env!("CARGO_BIN_EXE_rgit");
    let tmp = tempfile::tempdir().unwrap();
    let mut results = Vec::new();
    for (side, server) in [
        ("git", format!("{} receive-pack", reference.display())),
        ("rgit", format!("{rgit} receive-pack")),
    ] {
        let root = tmp.path().join(side);
        std::fs::create_dir_all(&root).unwrap();
        let run = |dir: &Path, args: &[&str]| git(&reference, tmp.path(), dir, args);
        let object_format = format!("--object-format={format}");
        run(&root, &["init", "-q", &object_format, "-b", "main", "srv"]);
        std::fs::write(root.join("srv/f"), "1\n").unwrap();
        run(&root.join("srv"), &["add", "f"]);
        run(&root.join("srv"), &["commit", "-q", "-m", "one"]);
        run(&root, &["clone", "-q", "srv", "wk"]);
        let wk = root.join("wk");
        std::fs::write(wk.join("f"), "2\n").unwrap();
        run(&wk, &["commit", "-q", "-am", "two"]);
        let rp = format!("--receive-pack={server}");
        let mut log = String::new();
        let mut push = |args: &[&str]| {
            let mut a = vec!["push", rp.as_str()];
            a.extend_from_slice(args);
            let out = run(&wk, &a);
            log.push_str(&format!(
                "$ {}\n{}{}exit {}\n",
                args.join(" "),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
                out.status.code().unwrap_or(-1)
            ));
        };
        push(&["origin", "main"]);
        push(&["origin", "main:side", "main:refs/tags/t"]);
        push(&["--atomic", "origin", "main:a", "main:main"]);
        push(&["origin", ":side", ":nothere"]);
        run(
            &root.join("srv"),
            &["config", "receive.denyCurrentBranch", "updateInstead"],
        );
        push(&["origin", "main"]);
        std::fs::write(root.join("srv/f"), "dirty\n").unwrap();
        std::fs::write(wk.join("f"), "3\n").unwrap();
        run(&wk, &["commit", "-q", "-am", "three"]);
        push(&["origin", "main"]);
        let refs = run(&root.join("srv"), &["for-each-ref"]);
        log.push_str(&String::from_utf8_lossy(&refs.stdout));
        log.push_str(&std::fs::read_to_string(root.join("srv/f")).unwrap());
        // Paths differ between the two sides.
        results.push(log.replace(&root.display().to_string(), "<root>"));
    }
    rgit_difftest::assert_same_text("push to receive-pack", &results[0], &results[1]);
    // The pushes really went through (not failing the same way).
    assert!(
        results[1].contains(" * [new branch]      main -> side"),
        "{}",
        results[1]
    );
}
