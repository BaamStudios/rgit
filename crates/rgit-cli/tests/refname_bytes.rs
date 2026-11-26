// SPDX-License-Identifier: MIT
//! Reference names that are not UTF-8 (#214): git stores and prints them as
//! raw bytes. Loose files, `packed-refs`, reflogs and the output of the
//! reference commands against the reference git.
#![cfg(unix)]

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;

use rgit_difftest::{Check, Fixture};

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
