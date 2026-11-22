// SPDX-License-Identifier: MIT
//! Differential tests of `bisect` (#132) and the bisection helpers of
//! `rev-list`/`rev-parse` against the reference git: the commits visited
//! for the same answers, every message, the state files and refs.

use rgit_difftest::Fixture;

/// The state files compared after every step.
const STATE: &[&str] = &[
    "BISECT_START",
    "BISECT_TERMS",
    "BISECT_LOG",
    "BISECT_NAMES",
    "BISECT_EXPECTED_REV",
    "BISECT_ANCESTORS_OK",
    "BISECT_FIRST_PARENT",
    "BISECT_HEAD",
    "BISECT_RUN",
    "BISECT_RESET_WHEN_FOUND",
];

fn step(fx: &mut Fixture, args: &[&str]) {
    let mut c = fx.compare(args);
    for f in STATE {
        c = c.git_file(*f);
    }
    c.run();
}

fn steps(fx: &mut Fixture, list: &[&[&str]]) {
    for args in list {
        step(fx, args);
    }
}

/// `main`: t1..t12, one commit each (file `n` holds the number); `side`
/// forks at t4 with s5..s7 and is merged into `main` as `m` after t8;
/// `a` follows the merge.
fn fixture() -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"]);
    for i in 1..=8 {
        fx = fx
            .write("n", format!("{i}\n"))
            .git(["add", "n"])
            .git(["commit", "-q", "-m", &format!("c{i}")])
            .git(["tag", &format!("t{i}")])
            .tick();
    }
    fx = fx.git(["checkout", "-q", "-b", "side", "t4"]);
    for i in 5..=7 {
        fx = fx
            .write("s", format!("{i}\n"))
            .git(["add", "s"])
            .git(["commit", "-q", "-m", &format!("s{i}")])
            .git(["tag", &format!("s{i}")])
            .tick();
    }
    fx = fx
        .git(["checkout", "-q", "main"])
        .git(["merge", "-q", "--no-ff", "-m", "merge side", "side"])
        .git(["tag", "m"])
        .tick();
    for i in 9..=12 {
        fx = fx
            .write("n", format!("{i}\n"))
            .git(["add", "n"])
            .git(["commit", "-q", "-m", &format!("c{i}")])
            .git(["tag", &format!("t{i}")])
            .tick();
    }
    fx.git(["tag", "-m", "annotated", "ann", "t6"])
}

#[test]
fn rev_list_bisect_options() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let ranges: &[&[&str]] = &[
        &["t12", "^t1"],
        &["t12", "^t5"],
        &["t12", "^s6"],
        &["t12", "^t7", "^s5"],
        &["m", "^t2"],
        &["s7", "^t1"],
        &["t3", "^t3"],
        &["t12"],
    ];
    for range in ranges {
        for opts in [
            &["--bisect"][..],
            &["--bisect-vars"],
            &["--bisect-all"],
            &["--bisect", "--first-parent"],
            &["--bisect-vars", "--first-parent"],
            &["--bisect-all", "--first-parent"],
            &["--bisect-all", "--bisect-vars"],
            &["--bisect-all", "--oneline"],
            &["--bisect-all", "--reverse"],
            &["--bisect", "--parents"],
            &["--bisect", "-n", "1"],
        ] {
            let mut args = vec!["rev-list"];
            args.extend_from_slice(opts);
            args.extend_from_slice(range);
            fx.compare(&args).run();
        }
    }
    // --bisect uses refs/bisect/, --bisect-vars needs revisions.
    fx.compare(["rev-list", "--bisect"]).run();
    let mut fx = fx.git(["update-ref", "refs/bisect/bad", "t10"]).git([
        "update-ref",
        "refs/bisect/good-x",
        "t3",
    ]);
    for args in [
        &["rev-list", "--bisect"][..],
        &["rev-list", "--bisect", "t11"],
        &["rev-list", "--bisect-vars"],
        &["rev-list", "--bisect-all", "t12", "^t1"],
        &["rev-parse", "--bisect"],
    ] {
        fx.compare(args).run();
    }
}

#[test]
fn session_good_bad_skip() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    steps(
        &mut fx,
        &[
            &["bisect", "start"],
            &["bisect", "bad", "t8"],
            &["bisect", "good", "t1"],
            &["bisect", "good"],
            &["bisect", "skip"],
            &["bisect", "bad"],
            &["bisect", "good"],
            &["bisect", "good"],
            &["bisect", "bad"],
            &["bisect", "next"],
            &["bisect", "log"],
            &["bisect", "terms"],
            &["bisect", "visualize", "--oneline"],
            &["status"],
            &["bisect", "reset"],
            &["bisect", "reset"],
        ],
    );
}

#[test]
fn session_skips_and_ranges() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    steps(
        &mut fx,
        &[
            &["bisect", "start", "t8", "t1", "t2"],
            &["bisect", "skip", "t3..t5", "t6"],
            &["bisect", "good", "t4", "t5"],
            &["bisect", "skip"],
            &["bisect", "log"],
            &["bisect", "reset"],
            &["bisect", "start", "t8", "t1"],
            &["bisect", "skip"],
            &["bisect", "skip"],
            &["bisect", "skip"],
            &["bisect", "skip"],
            &["bisect", "skip"],
            &["bisect", "skip"],
            &["bisect", "log"],
            &["bisect", "reset"],
            // A commit both good and bad.
            &["bisect", "start", "t8", "t7"],
            &["bisect", "good", "t8"],
            &["bisect", "reset"],
        ],
    );
}

#[test]
fn terms() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    steps(
        &mut fx,
        &[
            &["bisect", "terms"],
            &["bisect", "start", "--term-new=broken", "--term-old=fine"],
            &["bisect", "terms"],
            &["bisect", "terms", "--term-good"],
            &["bisect", "terms", "--term-bad"],
            &["bisect", "terms", "--term-old"],
            &["bisect", "terms", "--term-new"],
            &["bisect", "terms", "--foo"],
            &["bisect", "terms", "a", "b"],
            &["bisect", "bad"],
            &["bisect", "good"],
            &["bisect", "broken"],
            &["bisect", "fine", "t2"],
            &["bisect", "broken", "t5"],
            &["bisect", "fine", "t3"],
            &["bisect", "broken"],
            &["bisect", "log"],
            &["bisect", "reset"],
            &["bisect", "start"],
            &["bisect", "old", "t1"],
            &["bisect", "new", "t6"],
            &["bisect", "bad"],
            &["bisect", "new"],
            &["bisect", "new"],
            &["bisect", "reset"],
            &["bisect", "start", "--term-good"],
            &["bisect", "reset"],
            &["bisect", "start", "--term-good", "invalid..term"],
            &["bisect", "reset"],
            &["bisect", "start", "--term-old", "bad", "--term-new", "good"],
            &["bisect", "start", "--term-old=start", "--term-new=x"],
            &["bisect", "start", "--term-old=a", "--term-new=a"],
            &["bisect", "reset"],
            &[
                "bisect",
                "start",
                "--term-good",
                "one",
                "t6",
                "--term-good",
                "two",
                "--term-bad",
                "bad-term",
                "t1",
                "--",
            ],
            &["bisect", "terms"],
            &["bisect", "reset"],
        ],
    );
}

#[test]
fn merge_bases() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    steps(
        &mut fx,
        &[
            // good and bad are siblings: the merge base comes first.
            &["bisect", "start", "t8", "s7"],
            &["bisect", "good"],
            &["bisect", "bad"],
            &["bisect", "reset"],
            &["bisect", "start", "t8", "s7"],
            &["bisect", "bad"],
            &["bisect", "reset"],
            &[
                "bisect",
                "start",
                "--term-new=new",
                "--term-old=old",
                "t8",
                "s7",
            ],
            &["bisect", "new"],
            &["bisect", "reset"],
            &[
                "bisect",
                "start",
                "--term-new=x1",
                "--term-old=y1",
                "t8",
                "s7",
            ],
            &["bisect", "x1"],
            &["bisect", "reset"],
            &["bisect", "start", "t8", "s7"],
            &["bisect", "skip"],
            &["bisect", "good"],
            &["bisect", "reset"],
            // Mistaken good and bad.
            &["bisect", "start", "t4", "s7"],
            &["bisect", "reset"],
            &["bisect", "start", "t3", "t7"],
            &["bisect", "reset"],
            &["bisect", "start", "t8", "t2"],
            &["bisect", "bad", "t1"],
            &["bisect", "reset"],
            // A good merge base needs no test.
            &["bisect", "start", "t8", "t6", "s7"],
            &["bisect", "reset"],
        ],
    );
}

#[test]
fn first_parent_and_merges() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    steps(
        &mut fx,
        &[
            &["bisect", "start", "--first-parent", "t12", "t2"],
            &["bisect", "bad"],
            &["bisect", "bad"],
            &["bisect", "good"],
            &["bisect", "good"],
            &["bisect", "log"],
            &["bisect", "reset"],
            // The merge is the first bad commit.
            &["bisect", "start", "t9", "t8", "s7"],
            &["bisect", "bad", "m"],
            &["bisect", "reset"],
            &["bisect", "start", "t12", "t1"],
            &["bisect", "bad"],
            &["bisect", "good"],
            &["bisect", "bad"],
            &["bisect", "good"],
            &["bisect", "reset"],
        ],
    );
}

#[test]
fn no_checkout_and_detached() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    steps(
        &mut fx,
        &[
            &["bisect", "start", "--no-checkout", "t8", "t1"],
            &["bisect", "bad"],
            &["bisect", "good", "BISECT_HEAD"],
            &["bisect", "log"],
            &["bisect", "reset"],
            &["checkout", "-q", "t7"],
            &["bisect", "start", "t7", "t1"],
            &["status"],
            &["bisect", "good"],
            &["bisect", "reset"],
            &["bisect", "start", "t8", "t1"],
            &["bisect", "reset", "t3"],
            &["checkout", "-q", "main"],
            &["bisect", "start"],
            &["bisect", "good", "t2"],
            &["bisect", "bad", "ann"],
            &["bisect", "reset"],
        ],
    );
}

#[test]
fn start_arguments_and_errors() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    steps(
        &mut fx,
        &[
            &["bisect"],
            &["bisect", "-h"],
            &["bisect", "help"],
            &["bisect", "foo"],
            &["bisect", "log"],
            &["bisect", "next"],
            &["bisect", "skip"],
            &["bisect", "reset", "a", "b"],
            &["bisect", "next", "a"],
            &["bisect", "replay"],
            &["bisect", "run"],
            &["bisect", "start", "--foo"],
            &["bisect", "start", "t7", "foo", "--"],
            &["bisect", "start", "foo", "bar"],
            &["bisect", "good", "foo"],
            &["bisect", "bad", "t1", "t2"],
            &["bisect", "good", "t1", "bar"],
            &["bisect", "skip", "t1", "foo"],
            &["bisect", "log"],
            &["bisect", "reset"],
            &["bisect", "start", "t7", "t2", "foo"],
            &["bisect", "reset"],
            &["bisect", "start", "--", "n"],
            &["bisect", "reset"],
            &["bisect", "start", "t8", "t1"],
            &["bisect", "start", "t8", "t3"],
            &["bisect", "reset", "nonexist"],
            &["bisect", "reset"],
            &["bisect", "start"],
            &["bisect", "good", "t1"],
            &["bisect", "next"],
            &["bisect", "reset"],
            &["bisect", "start"],
            &["bisect", "bad", "t6"],
            &["bisect", "next"],
            &["bisect", "reset"],
        ],
    );
}

#[test]
fn reset_when_found() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    steps(
        &mut fx,
        &[
            &["bisect", "start", "--reset-when-found=unknown", "t7", "t1"],
            &[
                "bisect",
                "start",
                "--reset-when-found",
                "--no-checkout",
                "t7",
                "t1",
            ],
            &["bisect", "reset"],
            &["bisect", "start", "--reset-when-found", "t7", "t5"],
            &["bisect", "bad"],
            &["bisect", "start", "--reset-when-found=found", "t7", "t5"],
            &["bisect", "bad"],
            &["checkout", "-q", "main"],
            &["bisect", "start", "t7", "t5"],
            &["bisect", "run", "--reset-when-found=foo", "true"],
            &["bisect", "run", "--reset-when-found=found", "false"],
            &["bisect", "reset"],
        ],
    );
}

#[test]
fn run_on_generated_history() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx.git(["checkout", "-q", "-b", "long", "t1"]);
    for i in 1..=40 {
        fx = fx
            .write("v", format!("{i}\n"))
            .git(["add", "v"])
            .git(["commit", "-q", "-m", &format!("v{i}")])
            .tick();
    }
    let mut fx = fx.write_executable("../check.sh", "#!/bin/sh\ntest $(cat v) -lt 27\n");
    steps(
        &mut fx,
        &[
            &["bisect", "start", "long", "t1"],
            &["bisect", "run", "../check.sh"],
            &["bisect", "log"],
            &["bisect", "reset"],
            // Skips (exit 125) on the way.
            &["bisect", "start", "long", "t1"],
            &[
                "bisect",
                "run",
                "sh",
                "-c",
                "v=$(cat v); test $v -ge 20 && test $v -le 24 && exit 125; test $v -lt 27",
            ],
            &["bisect", "reset"],
            // Only skipped commits are left.
            &["bisect", "start", "long", "long~6"],
            &["bisect", "run", "sh", "-c", "exit 125"],
            &["bisect", "log"],
            &["bisect", "reset"],
            // Without checkout: the script looks at BISECT_HEAD.
            &["bisect", "start", "--no-checkout", "long", "t1"],
            &[
                "bisect",
                "run",
                "sh",
                "-c",
                "test $(git cat-file -p BISECT_HEAD:v) -lt 13",
            ],
            &["bisect", "reset"],
            // Exit codes that stop the run, and 126/127 checked on a good
            // commit.
            &["bisect", "start", "long", "t1"],
            &["bisect", "run", "sh", "-c", "exit 128"],
            &["bisect", "run", "sh", "-c", "exit 255"],
            &["bisect", "run", "./does-not-exist"],
            &["bisect", "run", "sh", "-c", "rm .git/BISECT_START"],
            &["bisect", "reset"],
            &[
                "bisect",
                "start",
                "--term-new=term1",
                "--term-old=term2",
                "long",
                "t1",
            ],
            &["bisect", "run", "false"],
            &["bisect", "reset"],
        ],
    );
}

#[test]
fn replay() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    steps(
        &mut fx,
        &[
            &["bisect", "start", "t8", "t1"],
            &["bisect", "good"],
            &["bisect", "skip"],
            &["bisect", "bad"],
        ],
    );
    let log = fx.git_output(["bisect", "log"]).stdout;
    let crlf: Vec<u8> = log
        .split(|&b| b == b'\n')
        .flat_map(|l| l.iter().copied().chain(*b"\r\n"))
        .collect();
    let mut fx = fx
        .git(["bisect", "reset"])
        .write("../replay.log", log)
        .write("../replay-crlf.log", crlf)
        .write(
            "../odd.log",
            "garbage\ngit bisect start t7 t1\ngit bisect foo\n",
        )
        .write(
            "../unquoted.log",
            "git bisect start t7 t1\ngit bisect good t3\ngit bisect skip t4\ngit bisect bad t6\n",
        );
    steps(
        &mut fx,
        &[
            &["bisect", "replay", "../replay.log"],
            &["bisect", "log"],
            &["bisect", "reset"],
            &["bisect", "replay", "../replay-crlf.log"],
            &["bisect", "reset"],
            &["bisect", "replay", "../nonexistent.log"],
            &["bisect", "replay", "../odd.log"],
            &["bisect", "reset"],
            &["bisect", "replay", "../unquoted.log"],
            &["bisect", "reset"],
        ],
    );
}

#[test]
fn linked_worktree() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx.git(["worktree", "add", "-q", "--detach", "../wt", "t10"]);
    let mut c = fx.compare(["-C", "../wt", "bisect", "start", "t10", "t2"]);
    for f in STATE {
        c = c.git_file(*f);
    }
    c.run();
    fx.compare(["-C", "../wt", "bisect", "bad"]).run();
    fx.compare(["-C", "../wt", "for-each-ref", "refs/bisect/"])
        .run();
    fx.compare(["for-each-ref", "refs/bisect/"]).run();
    fx.compare(["bisect", "log"]).run();
    fx.compare(["-C", "../wt", "bisect", "log"]).run();
    fx.compare(["-C", "../wt", "bisect", "reset"]).run();
}
