// SPDX-License-Identifier: MIT
//! Differential tests of the first `switch` slice (#42): branch creation and
//! switches between commits with the same tree.

use rgit_difftest::Fixture;

#[test]
fn switch_same_as_git() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("f", "x\n")
        .git(["add", "f"])
        .git(["commit", "-q", "-m", "first\nsecond line"])
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "two"])
        .git(["branch", "other", "HEAD~1"])
        .git(["branch", "same"]);
    let cases: &[&[&str]] = &[
        &["switch", "-c", "new"],
        &["switch", "-c", "new", "HEAD~1"],
        &["switch", "-c", "same"],
        &["switch", "same"],
        &["switch", "other"],
        &["switch", "main"],
        &["switch", "nope"],
        &["switch", "--detach", "HEAD~1"],
        &["switch", "-q", "-c", "quiet"],
    ];
    for args in cases {
        fx.compare(*args).run();
    }
}
