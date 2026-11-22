// SPDX-License-Identifier: MIT
//! `rgit unpack-objects` against the reference git (output, exit code and
//! the stored objects), and the unpack limit of `rgit fetch`.

use std::path::Path;
use std::process::Command;

use rgit_difftest::{Check, Fixture};

const CHECKS: [Check; 4] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Objects,
];

fn commit(fx: Fixture, dir: &str, file: &str, content: &str) -> Fixture {
    fx.write(format!("{dir}/{file}"), content)
        .git(["-C", dir, "add", file])
        .git(["-C", dir, "commit", "-q", "-m", file])
        .tick()
}

/// `src` with some history, packs of it in the work directory, and an
/// empty repository `dst`.
fn fixture() -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "src"]);
    for i in 1..=4u32 {
        let lines: String = (1..=i * 80).map(|n| format!("{n}\n")).collect();
        fx = commit(fx, "src", "f", &lines);
        fx = commit(fx, "src", &format!("g{i}"), &format!("{i}\n"));
    }
    if fx.is_skipped() {
        return fx;
    }
    let all = fx.git_stdout([
        "-C",
        "src",
        "pack-objects",
        "--stdout",
        "--all",
        "--revs",
        "-q",
    ]);
    let thin = pack(&fx, "HEAD\n^HEAD~2\n", &["--thin"]);
    let head = fx.git_stdout(["-C", "src", "rev-parse", "HEAD"]);
    let tree = fx.git_stdout(["-C", "src", "rev-parse", "HEAD^{tree}"]);
    let trees = pack(&fx, &String::from_utf8_lossy(&tree), &[]);
    let mut bad_commit = b"tree ".to_vec();
    bad_commit.extend_from_slice(&tree[..tree.len() - 1]);
    bad_commit.extend_from_slice(b"\nauthor A <a@b>1 +0000\ncommitter C <c@d> 1 +0000\n\nm\n");
    let fx = fx.write("bad-commit", &bad_commit);
    let bad = fx.git_stdout([
        "-C",
        "src",
        "hash-object",
        "-t",
        "commit",
        "-w",
        "--literally",
        "../bad-commit",
    ]);
    let bad_pack = pack(&fx, &String::from_utf8_lossy(&bad), &["--no-revs"]);
    let _ = head;
    fx.write("all.pack", &all)
        .write("thin.pack", &thin)
        .write("trees.pack", &trees)
        .write("bad.pack", &bad_pack)
        .git(["init", "-q", "dst"])
}

/// A pack of `src` from `input` given to `pack-objects --stdout`.
fn pack(fx: &Fixture, input: &str, extra: &[&str]) -> Vec<u8> {
    let revs = !extra.contains(&"--no-revs");
    let mut cmd = Command::new(fx.reference_git());
    cmd.args(["-C", "src", "pack-objects", "--stdout", "-q"]);
    if revs {
        cmd.arg("--revs");
    }
    cmd.args(extra.iter().filter(|a| **a != "--no-revs"));
    isolate(fx, &mut cmd);
    let mut child = cmd
        .current_dir(fx.work_dir())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("git");
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    out.stdout
}

fn isolate(fx: &Fixture, cmd: &mut Command) {
    cmd.env("HOME", fx.home_dir())
        .env("XDG_CONFIG_HOME", fx.home_dir().join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
}

fn read(fx: &Fixture, name: &str) -> Vec<u8> {
    std::fs::read(fx.work_dir().join(name)).unwrap()
}

#[test]
fn unpack() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let all = read(&fx, "all.pack");
    let mut trailing = all.clone();
    trailing.extend_from_slice(b"TRAILER\n");
    let mut bad_sum = all.clone();
    let n = bad_sum.len();
    bad_sum[n - 1] ^= 1;
    let mut bad_version = all.clone();
    bad_version[7] = 4;
    let mut bad_sig = all.clone();
    bad_sig[0] = b'X';
    let cases: Vec<(Vec<&str>, Vec<u8>)> = vec![
        (vec!["unpack-objects"], all.clone()),
        (vec!["unpack-objects", "-n"], all.clone()),
        (vec!["unpack-objects", "-q"], trailing),
        (vec!["unpack-objects"], read(&fx, "thin.pack")),
        (vec!["unpack-objects"], all[..all.len() / 2].to_vec()),
        (vec!["unpack-objects"], Vec::new()),
        (vec!["unpack-objects"], bad_sum),
        (vec!["unpack-objects"], bad_version),
        (vec!["unpack-objects"], bad_sig),
        (vec!["unpack-objects", "--max-input-size=100"], all.clone()),
        (vec!["unpack-objects", "--strict"], all.clone()),
        (vec!["unpack-objects", "--strict"], read(&fx, "trees.pack")),
        (vec!["unpack-objects", "--strict"], read(&fx, "bad.pack")),
        (
            vec!["unpack-objects", "--strict=missingSpaceBeforeDate=warn"],
            read(&fx, "bad.pack"),
        ),
        (vec!["unpack-objects"], read(&fx, "bad.pack")),
        (vec!["unpack-objects", "--bogus"], all.clone()),
        (vec!["unpack-objects", "extra"], all.clone()),
        (vec!["unpack-objects", "-h"], Vec::new()),
    ];
    let mut failures = Vec::new();
    for (args, input) in &cases {
        let r = fx
            .compare(args)
            .in_dir("dst")
            .repo("dst")
            .checks(CHECKS)
            .stdin(input.clone())
            .try_run();
        if let Err(m) = r {
            failures.push(m.to_string());
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn strict_into_populated_repository() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let all = read(&fx, "all.pack");
    let mut fx = fx.git(["-C", "dst", "fetch", "-q", "../src", "HEAD"]);
    for input in [all, read(&fx, "trees.pack")] {
        fx.compare(["unpack-objects", "--strict"])
            .in_dir("dst")
            .repo("dst")
            .checks(CHECKS)
            .stdin(input)
            .run();
    }
}

fn loose_and_packs(git_dir: &Path) -> (usize, usize) {
    let objects = git_dir.join("objects");
    let mut loose = 0;
    for e in std::fs::read_dir(&objects).unwrap().filter_map(Result::ok) {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.len() == 2 && e.path().is_dir() {
            loose += std::fs::read_dir(e.path()).unwrap().count();
        }
    }
    let packs = std::fs::read_dir(objects.join("pack"))
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter(|e| e.file_name().to_string_lossy().ends_with(".pack"))
                .count()
        })
        .unwrap_or(0);
    (loose, packs)
}

/// `rgit fetch` stores fewer objects than `fetch.unpackLimit` /
/// `transfer.unpackLimit` (default 100) as loose objects, like git.
#[test]
fn fetch_unpack_limit() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let rgit = env!("CARGO_BIN_EXE_rgit");
    let fx = fx
        .git(["clone", "-q", "--no-local", "src", "a"])
        .git(["clone", "-q", "--no-local", "src", "b"])
        .git(["clone", "-q", "--no-local", "src", "c"])
        .git(["clone", "-q", "--no-local", "src", "d"]);
    let fx = commit(fx, "src", "new", "new\n");
    for (dir, config, want_pack) in [
        ("a", None, false),
        ("b", Some("transfer.unpackLimit=3"), true),
        ("c", Some("fetch.unpackLimit=4"), false),
        ("d", Some("fetch.unpackLimit=0"), false),
    ] {
        let git_dir = fx.work_dir().join(dir).join(".git");
        let before = loose_and_packs(&git_dir);
        let mut cmd = Command::new(rgit);
        if let Some(c) = config {
            cmd.args(["-c", c]);
        }
        cmd.args(["fetch", "-q", "origin"])
            .current_dir(fx.work_dir().join(dir));
        isolate(&fx, &mut cmd);
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let after = loose_and_packs(&git_dir);
        if want_pack {
            assert_eq!((after.0, after.1), (before.0, before.1 + 1), "{dir}");
        } else {
            assert_eq!((after.0, after.1), (before.0 + 3, before.1), "{dir}");
        }
    }
    // `--keep` keeps the pack.
    let fx = fx.git(["clone", "-q", "--no-local", "src", "e"]);
    let fx = commit(fx, "src", "newer", "newer\n");
    let git_dir = fx.work_dir().join("e/.git");
    let before = loose_and_packs(&git_dir);
    let mut cmd = Command::new(rgit);
    cmd.args(["fetch", "-q", "-k", "origin"])
        .current_dir(fx.work_dir().join("e"));
    isolate(&fx, &mut cmd);
    assert!(cmd.output().unwrap().status.success());
    assert_eq!(loose_and_packs(&git_dir).1, before.1 + 1);
}
