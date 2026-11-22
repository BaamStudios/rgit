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

/// A commit whose subtree object is missing: `checkout`, `switch`,
/// `reset --hard` and `read-tree` stop with "unable to read tree (<id>)"
/// (t6030 "bisect fails if tree is broken").
#[test]
fn missing_subtree() {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a", "a\n")
        .git(["add", "a"])
        .git(["commit", "-q", "-m", "one"])
        .git(["tag", "one"])
        .tick()
        .write("sub/x", "x\n")
        .git(["add", "sub/x"])
        .git(["commit", "-q", "-m", "two"])
        .git(["tag", "two"])
        .git(["checkout", "-q", "one"]);
    if fx.is_skipped() {
        return;
    }
    let sub = String::from_utf8(fx.git_stdout(["rev-parse", "two:sub"])).unwrap();
    let sub = sub.trim().to_owned();
    let mut fx = fx.remove(format!(".git/objects/{}/{}", &sub[..2], &sub[2..]));
    for args in [
        &["checkout", "-q", "two", "--"][..],
        &["checkout", "two"],
        &["switch", "--detach", "two"],
        &["reset", "--hard", "two"],
        &["read-tree", "-m", "-u", "two"],
        &["read-tree", "two"],
    ] {
        fx.compare(args).run();
    }
}

/// Switching branches checks an assume-unchanged entry that has to change
/// for real: its local modification stops the checkout.
#[test]
fn assume_unchanged_dirty_file_blocks_switch() {
    let mut fx = rgit_difftest::Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("f", "one\n")
        .git(["add", "f"])
        .git(["commit", "-q", "-m", "one"])
        .git(["branch", "other"])
        .tick()
        .write("f", "two\n")
        .git(["commit", "-q", "-a", "-m", "two"])
        .git(["checkout", "-q", "other"])
        .write("f", "dirty\n")
        .git(["update-index", "--assume-unchanged", "f"]);
    fx.compare(["checkout", "main"]).run();
    // A clean assume-unchanged file does not stop it.
    let mut fx = rgit_difftest::Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("f", "one\n")
        .git(["add", "f"])
        .git(["commit", "-q", "-m", "one"])
        .git(["branch", "other"])
        .tick()
        .write("f", "two\n")
        .git(["commit", "-q", "-a", "-m", "two"])
        .git(["checkout", "-q", "other"])
        .git(["update-index", "--assume-unchanged", "f"]);
    fx.compare(["checkout", "main"]).run();
}
