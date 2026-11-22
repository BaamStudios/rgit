// SPDX-License-Identifier: MIT
//! The interactive modes against the reference git, answers fed on
//! standard input: `add -p` (navigation, split, edit, mode changes,
//! deletions), the patch modes of reset, checkout, restore, stash and
//! commit, `add -e` and the `add -i` menu. Prompts, the index and the
//! work tree must match.

use rgit_difftest::Fixture;

fn rgit() -> Fixture {
    Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"))
}

/// Two committed files, both changed in the work tree in two places.
fn changed() -> Fixture {
    rgit()
        .git(["init", "-q"])
        .write("a", "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n")
        .write("b", "b\n")
        .write("d/c", "c\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "first"])
        .tick()
        .write("a", "1\nX\n3\n4\n5\n6\n7\n8\n9\n10\nY\n12\n")
        .write("b", "b\nb2\n")
        .write("d/c", "c2\n")
}

/// [`changed`] with the changes staged and the second commit's parent
/// different.
fn staged() -> Fixture {
    changed()
        .git(["commit", "-q", "-a", "-m", "second"])
        .tick()
        .write("a", "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\nZ\n12\n")
        .write("b", "b\nb3\n")
        .git(["add", "a", "b"])
        .write("b", "b\nb4\n")
}

fn each(make: fn() -> Fixture, cases: &[(&[&str], &str)]) {
    for (args, input) in cases {
        let mut fx = make();
        if fx.is_skipped() {
            return;
        }
        fx.compare(*args).stdin(input.as_bytes().to_vec()).run();
    }
}

#[test]
fn add_patch_answers() {
    each(
        changed,
        &[
            (&["add", "-p"], "y\nn\ny\n"),
            (&["add", "-p"], "n\nq\n"),
            (&["add", "-p"], "a\nd\n"),
            (&["add", "-p"], ""),
            (&["add", "-p"], "?\nx\nyy\nj\nJ\nk\nK\ng\n/\ns\np\ny\n"),
            (&["add", "-p", "--", "d"], "y\n"),
            (&["add", "-p", "-U1"], "s\ny\nn\n"),
            (&["add", "-p", "-U1"], "s\ng\n2\ny\ng1\nn\n"),
            (&["add", "-p", "-U1"], "s\n/Y\ny\nK\ny\n"),
            (&["add", "-p", "--no-auto-advance"], "y\n>\nn\n<\nq\n"),
            (&["add", "-p", "-U", "-1"], ""),
            (&["add", "-U", "3"], ""),
        ],
    );
}

#[test]
fn add_patch_split_and_join() {
    // Splitting, then taking both halves, joins the shared context again.
    each(
        changed,
        &[
            (&["add", "-p", "-U3", "--", "a"], "s\ny\ny\n"),
            (&["add", "-p"], "s\nn\ny\n"),
        ],
    );
}

#[test]
fn add_patch_edit() {
    let editor = "#!/bin/sh\nsed -e 's/^+X/+E/' \"$1\" >\"$1.tmp\" && mv \"$1.tmp\" \"$1\"\n";
    for input in ["e\nn\nn\n", "s\ne\nn\nn\n"] {
        let mut fx = changed().write_executable("ed.sh", editor);
        if fx.is_skipped() {
            return;
        }
        let ed = fx.work_dir().join("ed.sh");
        fx.compare(["add", "-p", "-U1", "--", "a"])
            .env("GIT_EDITOR", &ed)
            .stdin(input)
            .run();
    }
    // An edit that does not apply, then giving up.
    let mut fx = changed().write_executable("ed.sh", "#!/bin/sh\necho garbage >\"$1\"\n");
    if fx.is_skipped() {
        return;
    }
    let ed = fx.work_dir().join("ed.sh");
    fx.compare(["add", "-p", "--", "b"])
        .env("GIT_EDITOR", &ed)
        .stdin("e\nx\nn\nq\n")
        .run();
}

#[test]
fn add_patch_deletion_and_mode() {
    let make = || {
        changed()
            .remove("b")
            .git(["update-index", "--chmod=+x", "a"])
            .write("new", "n\n")
            .git(["add", "-N", "new"])
    };
    for input in ["n\nn\ny\nn\nn\n", "y\ny\ny\ny\ny\n", "?\nq\n"] {
        let mut fx = make();
        if fx.is_skipped() {
            return;
        }
        fx.compare(["-c", "core.filemode=false", "add", "-p"])
            .stdin(input)
            .run();
    }
}

#[test]
fn reset_checkout_restore_patch() {
    each(
        staged,
        &[
            (&["reset", "-p"], "y\nn\n"),
            (&["reset", "-p", "HEAD^"], "n\ny\ny\n"),
            (&["reset", "-p", "--", "b"], "y\n"),
            (&["checkout", "-p"], "y\nn\ny\n"),
            (&["checkout", "-p", "HEAD"], "y\ny\ny\ny\n"),
            (&["checkout", "-p", "HEAD"], "n\ny\nn\n"),
            (&["checkout", "-p", "HEAD^", "--", "d"], "y\n"),
            (&["restore", "-p"], "y\ny\n"),
            (&["restore", "-p", "-s", "HEAD^"], "y\nn\ny\ny\n"),
            (
                &["restore", "-p", "-S", "-W", "-s", "HEAD^"],
                "y\nn\ny\ny\ny\n",
            ),
            (&["reset", "-p", "HEAD:a"], ""),
        ],
    );
}

#[test]
fn stash_and_commit_patch() {
    each(
        staged,
        &[
            (&["stash", "-p"], "y\nn\ny\n"),
            (&["stash", "push", "-p", "--no-keep-index"], "y\ny\ny\n"),
            (&["stash", "-p"], "n\nn\nn\n"),
            (&["commit", "-q", "-p", "-m", "patched"], "y\nn\ny\n"),
            (&["commit", "-q", "-p", "-m", ""], "y\n"),
        ],
    );
}

#[test]
fn add_edit() {
    let mut fx = changed().write_executable("ed.sh", "#!/bin/sh\nsed -i -e '/^+X/d' \"$1\"\n");
    if fx.is_skipped() {
        return;
    }
    let ed = fx.work_dir().join("ed.sh");
    fx.compare(["add", "-e"]).env("GIT_EDITOR", &ed).run();
    each(changed, &[(&["-c", "core.editor=false", "add", "-e"], "")]);
}

#[test]
fn add_interactive_menu() {
    let make = || {
        changed()
            .git(["add", "b"])
            .write("u1", "u\n")
            .write("u2", "u\n")
            .write("bin", "\0x")
    };
    for input in [
        "",
        "s\n?\nh\nx y\n9\n\nq\n",
        "u\n?\n1,3\n-1\nx\n\nq\n",
        "u\n*\n",
        "r\n1\n\n",
        "a\n2-\n\n",
        "p\na\n\ny\n",
        "d\n1\n",
        "3\n1\n\n4\n1\n\n6\n1\n",
    ] {
        let mut fx = make();
        if fx.is_skipped() {
            return;
        }
        fx.compare(["add", "-i"]).stdin(input).run();
    }
}
