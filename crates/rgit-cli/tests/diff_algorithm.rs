// SPDX-License-Identifier: MIT
//! Differential tests of the diff algorithm options (`--minimal`,
//! `--patience`, `--histogram`, `--anchored`, `--diff-algorithm`,
//! `diff.algorithm`) against the reference git.

use rgit_difftest::{Check, Fixture};

const OLD: &str = "#include <stdio.h>

int fib(int n)
{
\tif (n < 2)
\t\treturn n;
\treturn fib(n - 1) + fib(n - 2);
}

int fact(int n)
{
\tif (n < 2)
\t\treturn 1;
\treturn n * fact(n - 1);
}

int main(void)
{
\tprintf(\"%d\\n\", fib(10));
\treturn 0;
}
";

const NEW: &str = "#include <stdio.h>

int fact(int n)
{
\tif (n < 2)
\t\treturn 1;
\treturn n * fact(n - 1);
}

int fib(int n)
{
\tif (n < 2)
\t\treturn n;
\treturn fib(n - 1) + fib(n - 2);
}

int main(void)
{
\tprintf(\"%d\\n\", fib(10));
\tprintf(\"%d\\n\", fact(5));
\treturn 0;
}
";

/// Repeated lines and a moved block: the algorithms disagree here.
const OLD2: &str = "a\nb\nc\nd\ne\nf\ng\n}\n}\n}\nx\ny\n";
const NEW2: &str = "x\ny\nb\nc\n}\nd\ne\nf\ng\n}\n}\na\n";

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("prog.c", OLD)
        .write("lines", OLD2)
        .git(["add", "."])
        .git(["commit", "-q", "-m", "first"])
        .tick()
        .write("prog.c", NEW)
        .write("lines", NEW2)
        .git(["commit", "-q", "-a", "-m", "second"])
}

fn compare_all(fx: &mut Fixture, cmds: &[Vec<&str>]) {
    let mut failures = Vec::new();
    for argv in cmds {
        if let Err(m) = fx.compare(argv).checks(Check::OUTPUT).try_run() {
            failures.push(format!("{argv:?}\n{}", m.report()));
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn algorithm_options_match_git() {
    let mut fx = fixture();
    let base = ["diff-tree", "-p", "HEAD^", "HEAD"];
    let variants: &[&[&str]] = &[
        &[],
        &["--minimal"],
        &["--patience"],
        &["--histogram"],
        &["--anchored=int fib"],
        &["--anchored", "int fib"],
        &["--anchored=}", "--anchored=a"],
        &["--diff-algorithm=patience"],
        &["--diff-algorithm", "histogram"],
        &["--diff-algorithm=minimal"],
        &["--diff-algorithm=Myers"],
        &["--diff-algorithm=default"],
        &["--minimal", "--patience"],
        &["--patience", "--minimal"],
        &["--anchored=int fib", "--histogram"],
        &["--histogram", "--anchored=int fib"],
        &["--anchored=int fib", "--patience"],
        &["--anchored=int fib", "--minimal"],
        &["--anchored=int fib", "--diff-algorithm=patience"],
        &["--anchored=int fib", "--diff-algorithm=histogram"],
        &[
            "--anchored=int fib",
            "--diff-algorithm=histogram",
            "--patience",
        ],
        &["--patience", "--no-indent-heuristic"],
        &["--patience", "-U1"],
        &["--patience", "--stat"],
        &["--patience", "--numstat"],
        &["--diff-algorithm=bogus"],
        &["--diff-algorithm"],
        &["--anchored"],
    ];
    let cmds: Vec<Vec<&str>> = variants
        .iter()
        .map(|v| base.iter().chain(v.iter()).copied().collect())
        .collect();
    compare_all(&mut fx, &cmds);
}

#[test]
fn diff_algorithm_config_matches_git() {
    let mut fx = fixture();
    let cmds: Vec<Vec<&str>> = vec![
        // Porcelain diff follows diff.algorithm, the plumbing does not.
        vec!["-c", "diff.algorithm=patience", "diff", "HEAD^", "HEAD"],
        vec!["-c", "diff.algorithm=histogram", "diff", "HEAD^", "HEAD"],
        vec!["-c", "diff.algorithm=minimal", "diff", "HEAD^", "HEAD"],
        vec![
            "-c",
            "diff.algorithm=patience",
            "diff-tree",
            "-p",
            "HEAD^",
            "HEAD",
        ],
        vec![
            "-c",
            "diff.algorithm=patience",
            "diff",
            "--diff-algorithm=default",
            "HEAD^",
            "HEAD",
        ],
        vec![
            "-c",
            "diff.algorithm=patience",
            "diff",
            "--minimal",
            "HEAD^",
            "HEAD",
        ],
        vec!["-c", "diff.algorithm=bogus", "diff", "HEAD^", "HEAD"],
        vec![
            "-c",
            "diff.algorithm=bogus",
            "diff-tree",
            "-p",
            "HEAD^",
            "HEAD",
        ],
    ];
    compare_all(&mut fx, &cmds);
}
