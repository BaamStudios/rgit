// SPDX-License-Identifier: MIT
//! Differential tests of `rgit read-tree` against the reference git:
//! output, exit code and the resulting index (including the cache tree).

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a", "a\n")
        .write("b", "b\n")
        .write("d/c", "c\n")
        .write("d/ee/f", "f\n")
        .write("dd/g", "g\n")
        .symlink("a", "l")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "init"])
        .tick()
        .write("a", "a2\n")
        .write("new", "n\n")
        .git(["add", "."])
}

const STATE: [Check; 4] = [Check::ExitCode, Check::Stdout, Check::Stderr, Check::Index];

#[test]
fn read_tree_cases() {
    for args in [
        &["HEAD"][..],
        &["HEAD^{tree}"],
        &["--empty"],
        &[],
        &["nosuch"],
    ] {
        let mut fx = fixture();
        let mut argv = vec!["read-tree"];
        argv.extend_from_slice(args);
        fx.compare(&argv).checks(STATE).run();
    }
}
