// SPDX-License-Identifier: MIT
//! `reset --soft`, `--mixed` and `reset [<tree-ish>] -- <pathspec>`
//! against the reference git: refs, `ORIG_HEAD`, reflogs, the index, the
//! "Unstaged changes after reset:" list, errors and exit codes.

use rgit_difftest::{Check, Fixture};

fn rgit() -> Fixture {
    Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"))
}

fn fixture() -> Fixture {
    rgit()
        .git(["init", "-q"])
        .write("f", "1\n")
        .git(["add", "f"])
        .git(["commit", "-q", "-m", "one"])
        .tick()
        .write("f", "2\n")
        .git(["add", "f"])
        .git(["commit", "-q", "-m", "two"])
        .tick()
}

/// Two commits, then staged and unstaged changes of every kind.
fn busy() -> Fixture {
    rgit()
        .git(["init", "-q"])
        .write("a", "a\n")
        .write("c", "c\n")
        .write("d/c", "dc\n")
        .write("d/e/b", "b\n")
        .write("sp ace", "s\n")
        .write("x", "x\n")
        .write("list", "a\nd\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .tick()
        .write("a", "two\n")
        .write("d/c", "dc2\n")
        .git(["commit", "-q", "-a", "-m", "two"])
        .tick()
        .write("a", "three\n")
        .write("new", "n\n")
        .git(["add", "a", "new"])
        .git(["rm", "-q", "c"])
        .write("d/e/b", "b2\n")
        .write("sp ace", "s2\n")
        .git(["update-index", "--chmod=+x", "x"])
        .write_executable("x", "x\n")
        .remove("d/c")
        .symlink("target", "d/c")
}

fn run(make: fn() -> Fixture, cases: &[&[&str]]) {
    for args in cases {
        let mut fx = make();
        if fx.is_skipped() {
            return;
        }
        fx.compare(*args).run();
    }
}

#[test]
fn soft() {
    let cases: &[&[&str]] = &[
        &["reset", "--soft", "HEAD~1"],
        &["reset", "--soft"],
        &["reset", "-q", "--soft", "HEAD"],
        &["reset", "--soft", "HEAD", "--"],
        &["reset", "--soft", "HEAD", "--", "f"],
        &["reset", "--soft", "nope"],
        &["reset", "--soft", "HEAD^{tree}"],
    ];
    for args in cases {
        let mut fx = fixture();
        if fx.is_skipped() {
            return;
        }
        fx.compare(*args)
            .checks([
                Check::ExitCode,
                Check::Stdout,
                Check::Stderr,
                Check::Refs,
                Check::Reflogs,
                Check::Index,
            ])
            .run();
    }
}

#[test]
fn mixed_whole() {
    run(
        busy,
        &[
            &["reset"],
            &["reset", "-q"],
            &["reset", "--mixed"],
            &["reset", "HEAD~1"],
            &["reset", "--mixed", "-q", "HEAD~1"],
            &["reset", "HEAD~1", "--"],
            &["reset", "-N"],
            &["reset", "-N", "HEAD~1"],
            &["reset", "--no-refresh"],
            &["reset", "--no-refresh", "--refresh", "HEAD~1"],
            &["reset", "--soft", "--mixed"],
            &["reset", "--hard", "--soft"],
        ],
    );
}

#[test]
fn mixed_paths() {
    run(
        busy,
        &[
            &["reset", "--", "d"],
            &["reset", "--", "nonexist"],
            &["reset", "HEAD~1", "--", "a"],
            &["reset", "HEAD~1", "a", "d"],
            &["reset", "a"],
            &["reset", "a", "zzz"],
            &["reset", "-q", "--", "a", "new"],
            &["reset", "-N", "--", "new"],
            &["reset", "--mixed", "--", "a"],
            &["reset", "HEAD^{tree}", "a"],
            &["reset", "HEAD~1:", "--", "a"],
            &["reset", "--", "*a*"],
            &["reset", "--", ":(glob)d/**"],
            &["reset", "--pathspec-from-file=list"],
            &["reset", "--pathspec-from-file=list", "HEAD~1", "--"],
        ],
    );
}

#[test]
fn mixed_in_subdirectory() {
    for args in [
        &["reset", "--", "c"][..],
        &["reset", "HEAD~1", "../a"],
        &["reset", "--", "../../x"],
        &["reset", "c"],
    ] {
        let mut fx = busy();
        if fx.is_skipped() {
            return;
        }
        fx.compare(args).in_dir("d").run();
    }
}

#[test]
fn errors() {
    run(
        busy,
        &[
            &["reset", "nope"],
            &["reset", "nope", "--"],
            &["reset", "nope", "a"],
            &["reset", "zzz", "a"],
            &["reset", "HEAD:a", "--", "a"],
            &["reset", "HEAD^{tree}"],
            &["reset", "--soft", "-N"],
            &["reset", "--soft", "--", "a"],
            &["reset", "--hard", "--", "a"],
            &["reset", "--keep", "a"],
            &["reset", "--merge", "--", "a"],
            &["reset", "-p", "--soft"],
            &["reset", "--pathspec-file-nul"],
            &["reset", "--pathspec-from-file=list", "a"],
            &["reset", "--pathspec-from-file=list", "-p"],
            &["reset", "--pathspec-from-file=nonexist"],
            &["reset", "--bogus"],
            &["reset", "-h"],
        ],
    );
}

#[test]
fn revision_and_file() {
    let make = || {
        rgit()
            .git(["init", "-q"])
            .write("main", "m\n")
            .write("a", "a\n")
            .git(["add", "."])
            .git(["commit", "-q", "-m", "one"])
            .tick()
            .write("a", "a2\n")
            .git(["add", "a"])
    };
    run(
        make,
        &[
            &["reset", "main"],
            &["reset", "main", "a"],
            &["reset", "main", "--"],
        ],
    );
}

#[test]
fn unborn() {
    let make = || {
        rgit()
            .git(["init", "-q"])
            .write("a", "a\n")
            .write("b", "b\n")
            .git(["add", "."])
    };
    run(
        make,
        &[
            &["reset"],
            &["reset", "-N"],
            &["reset", "--", "a"],
            &["reset", "HEAD", "--", "a"],
            &["reset", "HEAD", "--"],
            &["reset", "HEAD"],
            &["reset", "--soft"],
            &["reset", "main", "--"],
        ],
    );
}

#[test]
fn bare() {
    let make = || {
        rgit().git(["init", "-q", "--bare"]).git([
            "commit-tree",
            "-m",
            "x",
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
        ])
    };
    run(
        make,
        &[&["reset"], &["reset", "--soft"], &["reset", "--", "a"]],
    );
}

#[test]
fn conflicts_and_flags() {
    let make = || {
        let fx = rgit()
            .git(["init", "-q"])
            .write("a", "a\n")
            .write("b", "b\n")
            .write("d/c", "c\n")
            .git(["add", "."])
            .git(["commit", "-q", "-m", "one"])
            .tick()
            .git(["branch", "side"])
            .write("a", "ours\n")
            .git(["commit", "-q", "-a", "-m", "ours"])
            .tick()
            .git(["switch", "-q", "side"])
            .write("a", "theirs\n")
            .git(["commit", "-q", "-a", "-m", "theirs"])
            .tick()
            .git(["switch", "-q", "main"]);
        // Conflicts in `a`; the exit code of the failed merge is expected.
        let _ = fx.git_output(["merge", "-q", "side"]);
        fx.tick().write("d/c", "c2\n").write("b", "b2\n").git([
            "update-index",
            "--assume-unchanged",
            "b",
        ])
    };
    run(
        make,
        &[
            &["reset"],
            &["reset", "--", "d"],
            &["reset", "--", "a"],
            &["reset", "-N", "HEAD~1"],
        ],
    );
}

fn busy_clean() -> Fixture {
    busy().git(["reset", "-q", "--hard"])
}

#[test]
fn hard_merge_keep() {
    run(
        busy,
        &[
            &["reset", "--hard"],
            &["reset", "--hard", "HEAD~1"],
            &["reset", "--hard", "-q", "HEAD~1"],
            &["reset", "--merge"],
            &["reset", "--merge", "HEAD~1"],
            &["reset", "--keep"],
            &["reset", "--keep", "HEAD~1"],
        ],
    );
    // The fixture copies the work tree for each side, so every file looks
    // touched; with minimal stat checks the copies match their entries.
    const MIN: [&str; 4] = [
        "-c",
        "core.checkStat=minimal",
        "-c",
        "core.trustCtime=false",
    ];
    let cases: &[&[&str]] = &[
        &["reset", "--merge", "HEAD~1"],
        &["reset", "--keep", "HEAD~1"],
        &["reset", "--hard", "HEAD~1"],
        &[MIN[0], MIN[1], MIN[2], MIN[3], "reset", "--merge", "HEAD~1"],
        &[MIN[0], MIN[1], MIN[2], MIN[3], "reset", "--keep", "HEAD~1"],
        &[MIN[0], MIN[1], MIN[2], MIN[3], "reset", "--merge"],
        &[MIN[0], MIN[1], MIN[2], MIN[3], "reset", "--keep"],
    ];
    // A staged change of a file the reset does not touch, an unstaged one,
    // and an unstaged change of a file it does touch.
    run(|| busy_clean().write("x", "s\n").git(["add", "x"]), cases);
    run(|| busy_clean().write("x", "u\n"), cases);
    run(|| busy_clean().write("a", "u\n"), cases);
    run(|| busy_clean().write("a", "u\n").git(["add", "a"]), cases);
    // A directory replaced by a file, with an untracked file in it; an
    // untracked file where the target has a tracked one.
    run(
        || {
            busy_clean()
                .git(["rm", "-q", "-r", "d"])
                .write("d", "file\n")
                .git(["add", "d"])
                .git(["commit", "-q", "-m", "three"])
                .tick()
                .write("u", "untracked\n")
        },
        cases,
    );
    run(
        || {
            busy_clean()
                .git(["rm", "-q", "c"])
                .git(["commit", "-q", "-m", "three"])
                .tick()
                .write("c", "untracked\n")
        },
        cases,
    );
}

#[test]
fn hard_with_conflicts() {
    let make = || {
        let fx = rgit()
            .git(["init", "-q"])
            .write("a", "a\n")
            .git(["add", "."])
            .git(["commit", "-q", "-m", "one"])
            .tick()
            .git(["branch", "side"])
            .write("a", "ours\n")
            .git(["commit", "-q", "-a", "-m", "ours"])
            .tick()
            .git(["switch", "-q", "side"])
            .write("a", "theirs\n")
            .git(["commit", "-q", "-a", "-m", "theirs"])
            .tick()
            .git(["switch", "-q", "main"]);
        let _ = fx.git_output(["merge", "-q", "side"]);
        fx.tick()
    };
    run(
        make,
        &[
            &["reset", "--hard"],
            &["reset", "--hard", "HEAD~1"],
            // git-reset(1): unmerged entries take the target's version.
            &["reset", "--merge"],
            &["reset", "--merge", "HEAD~1"],
            &["reset", "--keep"],
        ],
    );
}
