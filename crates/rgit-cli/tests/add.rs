// SPDX-License-Identifier: MIT
//! Differential tests of `rgit add` against the reference git: output,
//! exit code, the resulting index and the object database.

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a", "a\n")
        .write("b", "b\n")
        .write("d/c", "c\n")
        .write(".gitignore", "*.log\nbuild/\n")
        .git(["add", "a", "b", "d/c", ".gitignore"])
        .git(["commit", "-q", "-m", "init"])
        .tick()
        .write("a", "a2\n")
        .write("d/c", "c2\n")
        .write("d/new", "n\n")
        .write("e/f/g", "g\n")
        .write("top", "top\n")
        .write("x.log", "log\n")
        .write("build/out", "o\n")
        .remove("b")
}

const STATE: [Check; 5] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Index,
    Check::Objects,
];

fn each(dir: &str, cases: &[&[&str]]) {
    for args in cases {
        let mut fx = fixture();
        let mut argv = vec!["add"];
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
            &["-v", "a", "d"],
            &["-v", "."],
            &["-n", "."],
            &["-v", "-A"],
            &["-v", "-u"],
            &["-v", "b"],
            &["-v", "e"],
            &["-v", "*.log"],
            &["-v", "x.log"],
            &["-f", "-v", "x.log"],
            &["nosuch"],
            &["-v", "d/*"],
            &[],
            &["-A", "-u"],
        ],
    );
}

#[test]
fn subdirectory() {
    each(
        "d",
        &[&["-v", "."], &["-v", "new"], &["-v", "-u"], &["-v", "-A"]],
    );
}
