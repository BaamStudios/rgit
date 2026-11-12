// SPDX-License-Identifier: MIT
//! Differential tests of `rgit update-index` against the reference git:
//! output, exit code, the resulting index and the object database.

use rgit_difftest::{Check, Fixture};

const OID: &str = "1234567890123456789012345678901234567890";
const NULL: &str = "0000000000000000000000000000000000000000";

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a", "a\n")
        .write("b", "b\n")
        .write("d/c", "c\n")
        .write("e/new", "n\n")
        .write("untracked", "top\n")
        .symlink("a", "l")
        .git(["update-index", "--add", "a", "b", "d/c", "l"])
        .git(["commit", "-q", "-m", "init"])
        .tick()
        .write("a", "a2\n")
        .write("d/c", "c2\n")
}

/// Index, objects and output; the work tree is not touched.
const STATE: [Check; 5] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Index,
    Check::Objects,
];

fn same(fx: &mut Fixture, dir: &str, args: &[&str]) {
    let mut argv = vec!["update-index"];
    argv.extend_from_slice(args);
    fx.compare(&argv).in_dir(dir).checks(STATE).run();
}

/// Each case starts from the same state (comparisons leave the fixture
/// with the reference git's result, so every case gets a fresh fixture).
fn each(dir: &str, cases: &[&[&str]]) {
    for args in cases {
        let mut fx = fixture();
        same(&mut fx, dir, args);
    }
}

#[test]
fn paths() {
    each(
        "",
        &[
            &["a"],
            &["--verbose", "a", "d/c"],
            &["untracked"],
            &["--add", "--verbose", "untracked", "e/new"],
            &["--add", "untracked", "nosuch"],
            &["--remove", "--verbose", "nosuch"],
            &["--verbose", "--force-remove", "a", "nosuch"],
            &["--add", "d"],
            &["--add", "e"],
            &["--add", "./a", "d/../b", "d//c"],
            &["--add", ".git/config", "d/"],
            &["--", "--add"],
            &["--add", "--chmod=+x", "--verbose", "untracked"],
            &["--chmod=x", "a"],
            &["--verbose", "--chmod=-x", "a", "--chmod=+x", "l"],
            &["--info-only", "--add", "--verbose", "untracked"],
            &[],
            &["--nonsense"],
            &["-h"],
        ],
    );
    each(
        "d",
        &[
            &["--verbose", "c", "../a"],
            &["--add", "../../x"],
            &["--add", "../.git/config"],
        ],
    );
}

#[test]
fn cacheinfo() {
    let new = format!("100644,{OID},x");
    let null = format!("100644,{NULL},y");
    let odd = format!("100600,{OID},z");
    let df = format!("100644,{OID},d/c/x");
    each(
        "",
        &[
            &["--cacheinfo"],
            &["--cacheinfo", "100644"],
            &["--cacheinfo", "100644,1234,x"],
            &["--cacheinfo", &new],
            &["--add", "--verbose", "--cacheinfo", &new],
            &["--add", "--cacheinfo", &null],
            &["--add", "--cacheinfo", &odd],
            &["--add", "--cacheinfo", "100755", OID, "w"],
            &["--add", "--cacheinfo", &df],
            &["--add", "--replace", "--verbose", "--cacheinfo", &df],
        ],
    );
}

#[test]
fn flags_and_refresh() {
    each(
        "",
        &[
            &["--assume-unchanged", "a", "nosuch"],
            &["--verbose", "--skip-worktree", "a"],
            &["--assume-unchanged", "--skip-worktree", "a"],
            &["--refresh"],
            &["-q", "--refresh"],
            &["--really-refresh"],
            &["--unmerged", "--refresh"],
            &["--show-index-version"],
            &["--index-version", "4", "--verbose"],
            &["--index-version", "5"],
            &["--verbose", "a", "--again"],
            &["--verbose", "--again"],
            &["--verbose", "--again", "d/"],
        ],
    );
    let mut fx = fixture().remove("b");
    same(&mut fx, "", &["--refresh"]);
    let mut fx = fixture().remove("b");
    same(&mut fx, "", &["--ignore-missing", "--refresh"]);
    let mut fx = fixture().remove("b");
    same(&mut fx, "", &["--remove", "--verbose", "b"]);
    let mut fx = fixture().git(["update-index", "--assume-unchanged", "a"]);
    same(&mut fx, "", &["--really-refresh"]);
    let mut fx = fixture().git(["update-index", "--skip-worktree", "a"]);
    same(
        &mut fx,
        "",
        &["--no-assume-unchanged", "--no-skip-worktree", "a"],
    );
}

#[test]
fn stdin_and_index_info() {
    let cases: Vec<(&[&str], String)> = vec![
        (&["-z", "--stdin", "--verbose"], "a\0d/c\0".into()),
        (&["--stdin", "--verbose"], "a\nd/c\n".into()),
        (&["--stdin", "--verbose"], "\"d/\\143\"\n".into()),
        (&["--stdin", "--verbose"], "a\nuntracked\n".into()),
        (&["--stdin", "-z"], String::new()),
        (
            &["--index-info", "--verbose"],
            format!("100644 blob {OID}\tq1\n100644 {OID} 2\tq2\n100644 {OID}\tq3\n"),
        ),
        (
            &["--index-info", "--verbose"],
            format!("100644 blob {OID}\tq1\nbogus\n"),
        ),
        (
            &["--index-info", "--verbose"],
            format!("0 {NULL}\ta\n100644 {OID} 3\ta\n"),
        ),
        (
            &["-z", "--index-info", "--verbose"],
            format!("100644 {OID} 0\tz1\0"),
        ),
    ];
    for (args, input) in cases {
        let mut fx = fixture();
        let mut argv = vec!["update-index"];
        argv.extend_from_slice(args);
        fx.compare(&argv).stdin(input).checks(STATE).run();
    }
}

#[test]
fn beyond_symlink_and_submodule() {
    let mut fx = fixture().symlink("d", "lnk").write("d/q", "q\n");
    same(&mut fx, "", &["--add", "lnk/q"]);
    let mut fx = fixture()
        .git(["init", "-q", "sub"])
        .git(["-C", "sub", "commit", "-q", "--allow-empty", "-m", "s"])
        .git(["update-index", "--add", "sub"])
        .tick()
        .git(["-C", "sub", "commit", "-q", "--allow-empty", "-m", "s2"]);
    same(&mut fx, "", &["--refresh"]);
    same(&mut fx, "", &["--ignore-submodules", "--refresh"]);
    same(&mut fx, "", &["--verbose", "sub"]);
}
