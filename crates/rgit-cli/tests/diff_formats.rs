// SPDX-License-Identifier: MIT
//! Differential tests of the general diff output options against the
//! reference git: `--stat` widths (with `COLUMNS`), `--compact-summary`,
//! `--dirstat`, `--relative`, `-O`, `--rotate-to`/`--skip-to`,
//! `--output`, `--max-depth`, `-W`, `--line-prefix`,
//! `--output-indicator-*`, `-a` and option errors.

use rgit_difftest::{Check, Fixture};

fn numbers(n: usize, prefix: &str) -> String {
    (1..=n).map(|i| format!("{prefix}{i}\n")).collect()
}

const FUNCS: &str = "#include <stdio.h>\n\n/*\n * Comment.\n */\nint one(void)\n{\n\tint a = 1;\n\tint b = 2;\n\treturn a + b;\n}\n\nint two(void)\n{\n\tint c = 3;\n\n\treturn c;\n}\n";

fn fixture() -> Fixture {
    let long = "d/".to_string() + &"n".repeat(90);
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("top", numbers(30, ""))
        .write("a/b/c/f", numbers(100, ""))
        .write("a/b/g", numbers(50, ""))
        .write("a/h", numbers(10, ""))
        .write("x/y", numbers(20, ""))
        .write(&long, "short\n")
        .write("bin", "b\0inary")
        .write("prog.c", FUNCS)
        .write("one/two/three/file", "deep\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "first"])
        .tick()
        .write("top", numbers(25, ""))
        .write("a/b/c/f", numbers(90, ""))
        .write("a/b/g", numbers(45, ""))
        .write("a/h", numbers(10, "") + "z\n")
        .write(&long, numbers(300, "line "))
        .write("bin", "b\0inaryyyy")
        .write("prog.c", FUNCS.replace("int b = 2;", "int b = 22;"))
        .write("one/two/three/file", "deeper\n")
        .write("new", "new\n")
        .write("order", "*.c\nx/*\n")
        .git(["rm", "-q", "x/y"])
        .git(["add", "."])
        .git(["commit", "-q", "-m", "second"])
}

#[test]
fn formats_match_git() {
    let mut fx = fixture();
    let mut failures = Vec::new();
    let runs: &[&[&str]] = &[
        &["diff", "--stat", "HEAD^", "HEAD"],
        &["diff", "--stat=60,20", "HEAD^", "HEAD"],
        &["diff", "--stat-graph-width=10", "HEAD^", "HEAD"],
        &["diff", "--stat=30,10,2", "HEAD^", "HEAD"],
        &["diff", "--compact-summary", "HEAD^", "HEAD"],
        &[
            "-c",
            "diff.statNameWidth=20",
            "diff-tree",
            "-r",
            "--stat",
            "HEAD^",
            "HEAD",
        ],
        &["diff", "--dirstat", "HEAD^", "HEAD"],
        &["diff", "--dirstat=files,0,cumulative", "HEAD^", "HEAD"],
        &["diff", "--dirstat=lines,0", "HEAD^", "HEAD"],
        &["diff", "-X10", "--stat", "--summary", "HEAD^", "HEAD"],
        &["diff", "--dirstat=bogus", "HEAD^", "HEAD"],
        &[
            "-c",
            "diff.dirstat=bogus,0",
            "diff",
            "--dirstat",
            "HEAD^",
            "HEAD",
        ],
        &["diff", "--relative=a", "--stat", "HEAD^", "HEAD"],
        &["diff", "--relative=a/", "-p", "HEAD^", "HEAD"],
        &[
            "diff",
            "--relative=a",
            "--stat",
            "--summary",
            "HEAD^",
            "HEAD",
        ],
        &["diff", "-Oorder", "--name-only", "HEAD^", "HEAD"],
        &[
            "-c",
            "diff.orderFile=order",
            "diff",
            "--name-only",
            "HEAD^",
            "HEAD",
        ],
        &["diff", "--rotate-to=new", "--name-only", "HEAD^", "HEAD"],
        &["diff", "--skip-to=new", "--name-only", "HEAD^", "HEAD"],
        &["diff", "--skip-to=nothing", "HEAD^", "HEAD"],
        &["diff-tree", "--max-depth=1", "--name-only", "HEAD^", "HEAD"],
        &["diff-tree", "--max-depth=0", "HEAD^", "HEAD", "--", "one"],
        &["diff", "-W", "HEAD^", "HEAD", "--", "prog.c"],
        &["diff", "-W", "-U0", "HEAD^", "HEAD", "--", "prog.c"],
        &["diff", "--line-prefix=> ", "--stat", "-p", "HEAD^", "HEAD"],
        &[
            "diff",
            "--output-indicator-new=N",
            "--output-indicator-old=O",
            "--output-indicator-context=C",
            "HEAD^",
            "HEAD",
            "--",
            "prog.c",
        ],
        &["diff", "-a", "HEAD^", "HEAD", "--", "bin"],
        &["diff", "-U-1", "HEAD^", "HEAD"],
        &["diff", "--inter-hunk-context=-1", "HEAD^", "HEAD"],
        &["-c", "diff.context=no", "diff", "HEAD^", "HEAD"],
    ];
    for argv in runs {
        if let Err(m) = fx.compare(*argv).checks(Check::OUTPUT).try_run() {
            failures.push(format!("{argv:?}\n{}", m.report()));
        }
    }
    for cols in ["1", "40", "200"] {
        let argv = ["diff", "--stat", "HEAD^", "HEAD"];
        if let Err(m) = fx
            .compare(argv)
            .env("COLUMNS", cols)
            .checks(Check::OUTPUT)
            .try_run()
        {
            failures.push(format!("COLUMNS={cols} {argv:?}\n{}", m.report()));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn no_index_matches_git() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .write("a/1", "1\n")
        .write("a/2", "2\n")
        .write("b/1", "one\n")
        .write("d/sub", "1\n")
        .write("e/sub/file", "2\n");
    let mut failures = Vec::new();
    let runs: &[&[&str]] = &[
        &["diff", "--no-index", "a", "b"],
        &["diff", "--no-index", "--name-status", "a", "b", "1"],
        &["diff", "--no-index", "--name-status", "a/", "b", ":!2"],
        &["diff", "--no-index", "--name-status", "d", "e"],
        &["diff", "--no-index", "/dev/null", "a/1"],
        &["diff", "--no-index", "a/1", "b"],
    ];
    for argv in runs {
        if let Err(m) = fx.compare(*argv).checks(Check::OUTPUT).try_run() {
            failures.push(format!("{argv:?}\n{}", m.report()));
        }
    }
    // Usage errors (git prints its whole option list).
    for argv in [
        &["diff", "--no-index", "--", "a/1", "a/2", "a"][..],
        &["diff", "--no-index", "a"],
    ] {
        if let Err(m) = fx.compare(argv).checks([Check::ExitCode]).try_run() {
            failures.push(format!("{argv:?}\n{}", m.report()));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
