// SPDX-License-Identifier: MIT
//! Differential tests of `checkout [<tree-ish>] [--] <pathspec>...`
//! (restoring files from the index or a tree) against the reference git.

use rgit_difftest::Fixture;

/// `v1` has a, b, d/x, d/y; HEAD (`main`) changes a, adds c. Then a, b
/// and d/x are modified in the work tree, `new` is staged.
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a", "a\n")
        .write("b", "b\n")
        .write("d/x", "x\n")
        .write("d/y", "y\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .git(["tag", "v1"])
        .tick()
        .write("a", "a2\n")
        .write("c", "c\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "two"])
        .tick()
        .write("a", "local\n")
        .write("b", "local\n")
        .write("d/x", "local\n")
        .write("new", "n\n")
        .git(["add", "new"])
}

#[test]
fn checkout_paths() {
    let cases: &[&[&str]] = &[
        &["checkout", "--", "a"],
        &["checkout", "a"],
        &["checkout", "a", "b"],
        &["checkout", "-q", "a"],
        &["checkout", "."],
        &["checkout", "--", "."],
        &["checkout", "d"],
        &["checkout", "d/*"],
        &["checkout", "*.txt"],
        &["checkout", "--", "nope", "a"],
        &["checkout", "nope"],
        &["checkout", "HEAD", "a"],
        &["checkout", "HEAD", "--", "a"],
        &["checkout", "v1", "a", "b"],
        &["checkout", "v1", "--", "a", "b"],
        &["checkout", "v1", "c"],
        &["checkout", "v1", "."],
        &["checkout", "HEAD", "new"],
        &["checkout", "HEAD~1", "--", "d"],
        &["checkout", "nope", "--", "a"],
        &["checkout", "main", "feat"],
        &["checkout", "v1:", "a"],
        &["checkout", "HEAD:a"],
    ];
    for args in cases {
        let fx = fixture();
        if fx.is_skipped() {
            return;
        }
        let mut fx = fx;
        fx.compare(*args).run();
    }
    // From a subdirectory, and a path removed from the index.
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx;
    fx.compare(["checkout", "x"]).in_dir("d").run();
    let mut fx = fixture().git(["rm", "-q", "--cached", "b"]);
    fx.compare(["checkout", "b"]).run();
    let mut fx = fixture().git(["rm", "-q", "--cached", "b"]);
    fx.compare(["checkout", "HEAD", "b"]).run();
}
