// SPDX-License-Identifier: MIT
//! Differential tests of `rgit rm` against the reference git: output,
//! exit code, the resulting index and the work tree.

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a", "a\n")
        .write("b", "b\n")
        .write("c", "c\n")
        .write("d/x", "x\n")
        .write("d/e/y", "y\n")
        .git(["add", "a", "b", "c", "d"])
        .git(["commit", "-q", "-m", "init"])
        .tick()
        .write("b", "b2\n")
        .write("c", "c2\n")
        .git(["add", "c"])
        .write("n", "n\n")
        .git(["add", "n"])
}

const STATE: [Check; 5] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Index,
    Check::Worktree,
];

fn each(dir: &str, cases: &[&[&str]]) {
    for args in cases {
        let mut fx = fixture();
        let mut argv = vec!["rm"];
        argv.extend_from_slice(args);
        fx.compare(&argv).in_dir(dir).checks(STATE).run();
    }
}

#[test]
fn paths() {
    each(
        "",
        &[
            &["a"],
            &["--cached", "a"],
            &["-n", "a"],
            &["-q", "a"],
            &["b"],
            &["--cached", "b"],
            &["-f", "b"],
            &["c"],
            &["--cached", "c"],
            &["n"],
            &["--cached", "n"],
            &["b", "c", "n"],
            &["d"],
            &["-r", "d"],
            &["-r", "--cached", "d"],
            &["d/*"],
            &["nosuch"],
            &["--ignore-unmatch", "nosuch"],
            &[],
        ],
    );
}

#[test]
fn subdirectory() {
    each("d", &[&["x"], &["-r", "e"], &["-r", "."]]);
}
