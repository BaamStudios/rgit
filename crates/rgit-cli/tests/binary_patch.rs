// SPDX-License-Identifier: MIT
//! Binary patches against the reference git: `diff --binary` writes
//! literal hunks byte for byte like git (git may choose delta hunks,
//! which rgit does not write), and `apply` applies binary patches of
//! either program, with git's checks of the preimage and the result.

/// Stat checks that survive the copy of the fixture (`--index` compares
/// stat data).
const STAT: [&str; 4] = [
    "-c",
    "core.checkStat=minimal",
    "-c",
    "core.trustCtime=false",
];

/// `args` after [`STAT`].
fn with_stat(args: &[&str]) -> Vec<String> {
    STAT.iter().chain(args).map(|s| s.to_string()).collect()
}

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    let big: Vec<u8> = (0..3000u32).map(|i| (i * 7919 % 251) as u8).collect();
    let mut bigger = big.clone();
    bigger.splice(0..0, big[..100].iter().copied());
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("bin", b"one\n\0two\n".as_slice())
        .write("big", big)
        .write("text", "a\nb\n")
        .write("gone", b"x\0".as_slice())
        .git(["add", "."])
        .git(["commit", "-q", "-m", "first"])
        .tick()
        .write("bin", b"one\n\0three\n".as_slice())
        .write("big", bigger)
        .write("text", "a\nc\n")
        .write("new", b"a\0".as_slice())
        .remove("gone")
        .git(["add", "-A"])
        .git(["commit", "-q", "-m", "second"])
}

#[test]
fn literal_patches_match_git() {
    let mut fx = fixture();
    for args in [
        &[
            "diff", "--binary", "HEAD^", "HEAD", "--", "bin", "new", "gone", "text",
        ][..],
        &[
            "diff", "--binary", "HEAD", "HEAD^", "--", "bin", "new", "gone",
        ],
        &["diff-tree", "-p", "--binary", "HEAD^", "HEAD", "--", "bin"],
        &["diff", "--binary", "--stat", "HEAD^", "HEAD", "--", "bin"],
        &[
            "format-patch",
            "-1",
            "--stdout",
            "--no-signature",
            "--no-binary",
            "HEAD",
        ],
        &[
            "format-patch",
            "-1",
            "--stdout",
            "--no-signature",
            "HEAD",
            "--",
            "bin",
        ],
    ] {
        fx.compare(with_stat(args)).checks(Check::OUTPUT).run();
    }
}

/// The output of `rgit <args>` in the fixture's work tree.
fn rgit_stdout(fx: &Fixture, args: &[&str]) -> Vec<u8> {
    std::process::Command::new(env!("CARGO_BIN_EXE_rgit"))
        .current_dir(fx.work_dir())
        .env("HOME", fx.home_dir())
        .env("XDG_CONFIG_HOME", fx.home_dir())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .expect("rgit runs")
        .stdout
}

#[test]
fn apply_binary_patches() {
    let fx = fixture();
    let git_patch = fx.git_stdout(["diff", "--binary", "HEAD^", "HEAD"]);
    let plain = fx.git_stdout(["diff", "HEAD^", "HEAD"]);
    let full = fx.git_stdout(["diff", "--full-index", "HEAD^", "HEAD"]);
    let ours = rgit_stdout(&fx, &["diff", "--binary", "HEAD^", "HEAD"]);
    let mut fx = fx
        .write("git.patch", git_patch)
        .write("plain.patch", plain)
        .write("full.patch", full)
        .write("ours.patch", ours)
        .git(["checkout", "-q", "HEAD^"]);
    for args in [
        &["apply", "--index", "git.patch"][..],
        &["apply", "--index", "ours.patch"],
        &["apply", "--check", "git.patch"],
        &["apply", "--stat", "--numstat", "--summary", "git.patch"],
        &["apply", "-R", "git.patch"],
        &["apply", "--index", "plain.patch"],
        &["apply", "--index", "full.patch"],
        &["apply", "--binary", "--cached", "git.patch"],
    ] {
        fx.compare(with_stat(args)).run();
    }
}
