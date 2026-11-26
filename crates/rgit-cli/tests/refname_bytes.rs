// SPDX-License-Identifier: MIT
//! Reference names that are not UTF-8 (#214): git stores and prints them as
//! raw bytes. Loose files, `packed-refs`, reflogs and the output of the
//! reference commands against the reference git.
#![cfg(unix)]

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;

use rgit_difftest::{Check, Fixture, Normalizer};

const CHECKS: [Check; 5] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Refs,
    Check::Reflogs,
];

fn arg(raw: &[u8]) -> OsString {
    OsString::from_vec(raw.to_vec())
}

fn args(list: &[&[u8]]) -> Vec<OsString> {
    list.iter().map(|a| arg(a)).collect()
}

/// A repository with branches and a tag whose names hold a Latin-1 byte,
/// a lone continuation byte and real UTF-8 (sorting between them).
fn fixture(packed: bool) -> Option<Fixture> {
    fixture_in("files", packed)
}

fn fixture_in(format: &str, packed: bool) -> Option<Fixture> {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git([
            "init",
            "-q",
            "-b",
            "main",
            &format!("--ref-format={format}"),
        ])
        .git(["commit", "-q", "--allow-empty", "-m", "a"])
        .git(args(&[b"update-ref", b"refs/heads/caf\xe9", b"HEAD"]))
        .git(args(&[b"branch", b"ab\x80"]))
        .git(args(&[b"branch", b"zz\xc3\xa9"]))
        .git(args(&[b"tag", b"t\xe9"]));
    if packed {
        fx = fx.git(["pack-refs", "--all"]);
    }
    (!fx.is_skipped()).then_some(fx)
}

fn run_cases(fx: &mut Fixture, cases: &[&[&[u8]]]) {
    for case in cases {
        fx.compare(args(case)).checks(CHECKS).run();
    }
}

const LISTING: &[&[&[u8]]] = &[
    &[b"for-each-ref"],
    &[
        b"for-each-ref",
        b"--format=%(refname:short) %(refname:lstrip=-1)",
    ],
    &[b"for-each-ref", b"refs/heads/caf\xe9"],
    &[b"for-each-ref", b"--sort=-refname", b"--format=%(refname)"],
    &[b"show-ref"],
    &[b"show-ref", b"--verify", b"refs/heads/caf\xe9"],
    &[b"show-ref", b"--branches", b"ab\x80"],
    &[b"branch"],
    &[b"branch", b"-v"],
    &[b"branch", b"--list", b"caf*"],
    &[b"branch", b"--list", b"caf?"],
    &[b"tag", b"-l", b"t?"],
    &[b"for-each-ref", b"refs/heads/caf?"],
    &[b"tag"],
    &[b"tag", b"-l", b"t*"],
    &[b"rev-parse", b"caf\xe9", b"refs/tags/t\xe9"],
    &[b"reflog", b"exists", b"refs/heads/caf\xe9"],
];

#[test]
fn listing_loose() {
    let Some(mut fx) = fixture(false) else { return };
    run_cases(&mut fx, LISTING);
}

#[test]
fn listing_packed() {
    let Some(mut fx) = fixture(true) else { return };
    run_cases(&mut fx, LISTING);
}

#[test]
fn listing_reftable() {
    let Some(mut fx) = fixture_in("reftable", false) else {
        return;
    };
    run_cases(&mut fx, LISTING);
}

const WRITING: &[&[&[u8]]] = &[
    &[b"update-ref", b"refs/heads/x\xe9", b"HEAD"],
    &[b"branch", b"n\xff"],
    &[b"tag", b"nt\xe9"],
    &[b"branch", b"-m", b"n\xff", b"m\xfe"],
    &[b"symbolic-ref", b"HEAD", b"refs/heads/caf\xe9"],
    &[b"symbolic-ref", b"HEAD"],
    &[b"symbolic-ref", b"--short", b"HEAD"],
    &[b"for-each-ref", b"--format=%(HEAD)%(refname)"],
    &[b"pack-refs", b"--all"],
    &[b"for-each-ref"],
    &[b"update-ref", b"-d", b"refs/heads/x\xe9"],
    &[b"update-ref", b"refs/heads/y\xe9", b"HEAD"],
    &[b"show-ref"],
];

#[test]
fn writing() {
    let Some(mut fx) = fixture(false) else { return };
    run_cases(&mut fx, WRITING);
}

#[test]
fn writing_reftable() {
    let Some(mut fx) = fixture_in("reftable", false) else {
        return;
    };
    run_cases(&mut fx, WRITING);
}

#[test]
fn check_ref_format() {
    let Some(mut fx) = fixture(false) else { return };
    run_cases(
        &mut fx,
        &[
            &[b"check-ref-format", b"refs/heads/caf\xe9"],
            &[b"check-ref-format", b"--normalize", b"refs//heads/caf\xe9"],
            &[b"check-ref-format", b"--branch", b"caf\xe9"],
            &[b"check-ref-format", b"refs/heads/a\x7f"],
        ],
    );
}

/// Transport over `file://` (upload-pack and receive-pack are rgit's own
/// on the rgit side), protocol v2 and v0.
fn transport(version: &str) {
    let Some(mut fx) = fixture(false) else { return };
    let v = format!("protocol.version={version}");
    let checks = |fx: &mut Fixture, repo: &str, case: &[&[u8]]| {
        let mut a = args(&[b"-c", v.as_bytes()]);
        a.extend(args(case));
        fx.compare(a).repo(repo).checks(CHECKS).run();
    };
    checks(&mut fx, "", &[b"ls-remote", b"--symref", b"."]);
    checks(&mut fx, "c", &[b"clone", b"-q", b"--no-local", b".", b"c"]);
    checks(&mut fx, "", &[b"branch", b"-D", b"ab\x80"]);
    checks(&mut fx, "c", &[b"-C", b"c", b"fetch", b"--prune"]);
    checks(&mut fx, "c", &[b"-C", b"c", b"branch", b"-a"]);
    checks(&mut fx, "b.git", &[b"init", b"-q", b"--bare", b"b.git"]);
    checks(
        &mut fx,
        "b.git",
        &[
            b"push",
            b"b.git",
            b"refs/heads/*:refs/heads/*",
            b"refs/tags/*:refs/tags/*",
        ],
    );
    checks(
        &mut fx,
        "b.git",
        &[b"push", b"b.git", b"caf\xe9:refs/heads/n\xe9"],
    );
    checks(
        &mut fx,
        "b.git",
        &[b"push", b"b.git", b":refs/heads/caf\xe9"],
    );
    checks(
        &mut fx,
        "b.git",
        &[b"push", b"--porcelain", b"b.git", b"main:refs/heads/m\xe9"],
    );
}

#[test]
fn transport_v2() {
    transport("2");
}

#[test]
fn transport_v0() {
    transport("0");
}

#[test]
fn history_and_status() {
    let Some(mut fx) = fixture(false) else { return };
    run_cases(
        &mut fx,
        &[
            &[b"log", b"--oneline", b"--decorate"],
            &[b"log", b"--oneline", b"--decorate=full"],
            &[b"log", b"-g", b"--oneline", b"caf\xe9"],
            &[b"reflog", b"show", b"caf\xe9"],
            &[b"rev-parse", b"--symbolic-full-name", b"caf\xe9"],
            &[b"describe", b"--all"],
            &[b"name-rev", b"HEAD"],
            &[b"checkout", b"-q", b"caf\xe9"],
            &[b"status"],
            &[b"rev-parse", b"--abbrev-ref", b"HEAD"],
            &[b"checkout", b"-q", b"main"],
            &[b"branch", b"-d", b"caf\xe9"],
            &[b"tag", b"-d", b"t\xe9"],
            &[b"branch", b"-d", b"nothing\xe9"],
        ],
    );
}

/// The merge message names the branch with its bytes; like any message
/// that is not UTF-8 it is warned about and stored as Latin-1 read.
#[test]
fn merge_message() {
    let Some(mut fx) = fixture(false) else { return };
    run_cases(
        &mut fx,
        &[
            &[b"checkout", b"-q", b"caf\xe9"],
            &[b"commit", b"-q", b"--allow-empty", b"-m", b"y"],
            &[b"checkout", b"-q", b"main"],
            &[b"merge", b"--no-ff", b"--no-edit", b"caf\xe9"],
            &[b"stash", b"list"],
        ],
    );
}

/// [`CHECKS`] and the files in the git directory (`config`, `HEAD`).
const CONFIG_CHECKS: [Check; 6] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Refs,
    Check::Reflogs,
    Check::GitFiles,
];

fn run_config_cases(fx: &mut Fixture, repo: &str, cases: &[&[&[u8]]]) {
    for case in cases {
        fx.compare(args(case))
            .repo(repo)
            .checks(CONFIG_CHECKS)
            .run();
    }
}

/// The `branch.<name>` sections of a branch whose name is not UTF-8 have
/// its raw bytes in the section header, written and read like git (#738).
#[test]
fn branch_config() {
    let Some(mut fx) = fixture(false) else { return };
    run_config_cases(
        &mut fx,
        "",
        &[
            &[b"branch", b"-u", b"main", b"caf\xe9"],
            &[b"config", b"--get-regexp", b"^branch"],
            &[b"branch", b"-vv"],
            &[b"rev-parse", b"--abbrev-ref", b"caf\xe9@{u}"],
            &[
                b"for-each-ref",
                b"--format=%(refname) %(upstream) %(upstream:short) %(upstream:track)",
                b"refs/heads/",
            ],
            &[b"checkout", b"caf\xe9"],
            &[b"status"],
            &[b"branch", b"--show-current"],
            &[b"rev-parse", b"--symbolic-full-name", b"@{u}"],
            &[b"checkout", b"-q", b"main"],
            &[b"branch", b"-m", b"caf\xe9", b"d\xe9"],
            &[b"branch", b"-c", b"d\xe9", b"e\xe9"],
            &[b"checkout", b"-b", b"n\xe9", b"--track", b"main"],
            &[b"branch", b"--track=inherit", b"i\xe9", b"n\xe9"],
            &[b"branch", b"--unset-upstream", b"n\xe9"],
            &[b"checkout", b"-q", b"main"],
            &[b"branch", b"-D", b"d\xe9"],
            &[b"config", b"--get-regexp", b"^branch"],
        ],
    );
}

/// `push -u`, `pull`, `remote rename` / `remove` and `clone -b` with a
/// branch whose name is not UTF-8.
#[test]
fn branch_config_remote() {
    let Some(mut fx) = fixture(false) else { return };
    run_config_cases(
        &mut fx,
        "",
        &[
            &[b"init", b"-q", b"--bare", b"bare"],
            &[b"remote", b"add", b"origin", b"bare"],
            &[b"push", b"-u", b"origin", b"caf\xe9"],
            &[b"push", b"-q", b"-u", b"origin", b"ab\x80"],
            &[b"checkout", b"caf\xe9"],
            &[b"status"],
            &[b"pull"],
            &[b"pull", b"--rebase"],
            &[b"merge"],
            &[
                b"for-each-ref",
                b"--format=%(push) %(push:short)",
                b"refs/heads/",
            ],
            &[b"checkout", b"-q", b"main"],
            &[b"remote", b"rename", b"origin", b"up"],
            &[b"branch", b"-vv"],
            &[b"remote", b"remove", b"up"],
        ],
    );
    run_config_cases(
        &mut fx,
        "c",
        &[
            &[b"clone", b"-q", b"-b", b"caf\xe9", b"bare", b"c"],
            &[b"-C", b"c", b"branch", b"--show-current"],
            &[
                b"-C",
                b"c",
                b"checkout",
                b"-b",
                b"x\xe9",
                b"--track",
                b"origin/ab\x80",
            ],
            &[b"-C", b"c", b"config", b"--get-regexp", b"^branch"],
        ],
    );
}

/// Messages of `checkout`, `switch`, `stash` and `worktree` print the
/// name's raw bytes.
#[test]
fn messages() {
    let Some(fx) = fixture(false) else { return };
    let mut fx = fx.write("f", "x\n");
    run_cases(
        &mut fx,
        &[
            &[b"checkout", b"caf\xe9"],
            &[b"switch", b"-c", b"s\xe9"],
            &[b"switch", b"main"],
            &[b"checkout", b"--detach", b"caf\xe9"],
            &[b"checkout", b"-"],
            &[b"checkout", b"-"],
            &[b"add", b"f"],
            &[b"stash"],
            &[b"stash", b"list"],
            &[b"stash", b"pop"],
            &[b"stash", b"-q"],
            &[b"worktree", b"add", b"w", b"caf\xe9"],
            &[b"worktree", b"add", b"-b", b"w\xe9", b"w2"],
            &[b"worktree", b"add", b"--orphan", b"-b", b"o\xe9", b"w3"],
            &[b"branch", b"-vv"],
            &[b"worktree", b"remove", b"w"],
        ],
    );
    for porcelain in [false, true] {
        let mut a = args(&[b"worktree", b"list"]);
        if porcelain {
            a.push("--porcelain".into());
        }
        fx.compare(a)
            .checks(CHECKS)
            .normalize_stdout(Normalizer::regex(r"(?m)^(worktree )?/\S+ *", "<path> "))
            .run();
    }
    let in_worktrees: [(&str, &[&[u8]]); 2] = [
        ("w2", &[b"-C", b"w2", b"branch", b"--show-current"]),
        ("w3", &[b"-C", b"w3", b"symbolic-ref", b"HEAD"]),
    ];
    for (repo, case) in in_worktrees {
        fx.compare(args(case)).repo(repo).checks(CHECKS).run();
    }
}

/// `rebase`: the state files, the todo list (labels of
/// `--rebase-merges`) and the messages hold the raw bytes.
#[test]
fn rebase() {
    let Some(mut fx) = fixture(false) else { return };
    let cases: &[&[&[u8]]] = &[
        &[b"commit", b"-q", b"--allow-empty", b"-m", b"b"],
        &[b"checkout", b"-q", b"caf\xe9"],
        &[b"commit", b"-q", b"--allow-empty", b"-m", b"c"],
        &[b"checkout", b"-q", b"-b", b"m\xe9"],
        &[b"commit", b"-q", b"--allow-empty", b"-m", b"d"],
        &[b"checkout", b"-q", b"caf\xe9"],
        &[b"merge", b"-q", b"--no-ff", b"-m", b"merge", b"m\xe9"],
        &[b"rebase", b"-i", b"--rebase-merges", b"main"],
        &[b"rebase", b"-i", b"--update-refs", b"HEAD~2"],
        &[b"rebase", b"main", b"m\xe9"],
        &[b"rebase", b"main", b"caf\xe9"],
        &[b"rebase", b"--apply", b"main", b"m\xe9"],
    ];
    for case in cases {
        fx.compare(args(case))
            .env("GIT_SEQUENCE_EDITOR", "cat")
            .checks(CHECKS)
            .run();
    }
    // A conflict stops the rebase: `head-name` names the branch.
    let mut fx = fx.write("f", "x\n");
    run_cases(
        &mut fx,
        &[
            &[b"checkout", b"-q", b"main"],
            &[b"add", b"f"],
            &[b"commit", b"-q", b"-m", b"f"],
            &[b"checkout", b"-q", b"m\xe9"],
        ],
    );
    let mut fx = fx.write("f", "y\n");
    let conflict: &[&[&[u8]]] = &[
        &[b"add", b"f"],
        &[b"commit", b"-q", b"-m", b"g"],
        &[b"rebase", b"main"],
        &[b"status"],
        &[b"rebase", b"--abort"],
    ];
    for case in conflict {
        fx.compare(args(case))
            .checks(CONFIG_CHECKS)
            .git_file("rebase-merge/head-name")
            .run();
    }
}

/// A symbolic ref that points nowhere is skipped with a warning when a
/// name is looked up (t1430 "rev-parse skips symref pointing to broken
/// name").
#[test]
fn dangling_symref() {
    let Some(mut fx) = fixture(false) else { return };
    run_cases(
        &mut fx,
        &[
            &[b"symbolic-ref", b"refs/heads/dang", b"refs/heads/nothere"],
            &[b"rev-parse", b"dang"],
            &[b"rev-parse", b"--verify", b"dang"],
            &[b"rev-parse", b"--verify", b"-q", b"dang"],
            &[b"symbolic-ref", b"refs/heads/d\xe9", b"refs/heads/no\xe9"],
            &[b"rev-parse", b"d\xe9"],
            &[b"rev-parse", b"--verify", b"-q", b"d\xe9"],
            &[b"symbolic-ref", b"refs/tags/main", b"refs/heads/nothere"],
            &[b"rev-parse", b"--verify", b"main"],
            &[b"branch", b"-D", b"dang", b"d\xe9"],
        ],
    );
}
