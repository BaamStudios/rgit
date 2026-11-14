// SPDX-License-Identifier: MIT
//! `git stash` against the reference git: the stash commits (same ids for
//! the same inputs and dates), `refs/stash` and its reflog, the index, the
//! work tree, messages and exit codes.

use rgit_difftest::Fixture;

fn rgit() -> Fixture {
    Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"))
}

/// One commit with three files.
fn base() -> Fixture {
    rgit()
        .git(["init", "-q"])
        .write("a", "a\n")
        .write("b", "b\n")
        .write("d/c", "c\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "first line", "-m", "body"])
        .tick()
}

/// Staged and unstaged changes, a new file and an untracked one.
fn dirty() -> Fixture {
    base()
        .write("a", "a2\n")
        .git(["add", "a"])
        .write("a", "a3\n")
        .write("b", "b2\n")
        .write("n", "new\n")
        .git(["add", "n"])
        .write("u", "untracked\n")
        .write(".gitignore", "*.o\n")
        .write("x.o", "ignored\n")
}

fn each(make: fn() -> Fixture, cases: &[&[&str]]) {
    for args in cases {
        let mut fx = make();
        if fx.is_skipped() {
            return;
        }
        fx.compare(*args).run();
    }
}

#[test]
fn push_variants() {
    each(
        dirty,
        &[
            &["stash"],
            &["stash", "push"],
            &["stash", "-q"],
            &["stash", "push", "-m", "my message"],
            &["stash", "push", "-m", "two\nlines"],
            &["stash", "save", "saved", "message"],
            &["stash", "-k"],
            &["stash", "--keep-index", "--no-keep-index"],
            &["stash", "-u"],
            &["stash", "-a"],
            &["stash", "-S"],
            &["stash", "push", "--", "a", "n"],
            &["stash", "push", "-k", "--", "a", "b"],
            &["stash", "push", "-u", "--", "u", "b"],
            &["stash", "push", "nope"],
            &["stash", "--", "nope"],
            &["stash", "-S", "-u"],
            &["stash", "bogus"],
            &["stash", "--bogus"],
            &["stash", "create"],
            &["stash", "create", "some", "message"],
        ],
    );
}

#[test]
fn push_edge_cases() {
    each(base, &[&["stash"], &["stash", "-S"]]);
    let mut fx = rgit().git(["init", "-q"]).write("f", "x\n");
    if fx.is_skipped() {
        return;
    }
    fx.compare(["stash"]).run();
    // Detached HEAD, a file removed from the index but still on disk.
    base()
        .git(["checkout", "-q", "--detach"])
        .git(["rm", "-q", "--cached", "b"])
        .write("a", "changed\n")
        .same(["stash"])
        .same(["stash", "list"]);
}

#[test]
fn list_show_drop() {
    let mut fx = dirty()
        .same(["stash", "-u"])
        .tick()
        .write("a", "again\n")
        .same(["stash", "push", "-m", "second"])
        .tick()
        .write("b", "third\n")
        .same(["stash"]);
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["stash", "list"][..],
        &["stash", "list", "-2"],
        &["stash", "list", "--format=%gd %H"],
        &["stash", "list", "--oneline"],
        &["stash", "show"],
        &["stash", "show", "-p", "stash@{1}"],
        &["stash", "show", "2"],
        &["stash", "show", "-u", "stash@{2}"],
        &["stash", "show", "--only-untracked", "-p", "stash@{2}"],
        &["stash", "show", "--name-status", "stash@{2}"],
        &["stash", "show", "stash@{0}", "stash@{1}"],
        &["stash", "show", "nope"],
        &["stash", "show", "HEAD"],
        &["stash", "show", "--bogus"],
    ] {
        fx.compare(args).run();
    }
    fx = fx
        .same(["stash", "drop", "stash@{1}"])
        .same(["stash", "list"])
        .same(["stash", "drop", "-q"])
        .same(["stash", "drop", "1"])
        .same(["stash", "drop"])
        .same(["stash", "list"])
        .same(["stash", "show"]);
    let _ = fx;
}

#[test]
fn apply_and_pop() {
    let stashed = || dirty().git(["stash", "-u"]).tick();
    each(
        stashed,
        &[
            &["stash", "apply"],
            &["stash", "apply", "--index"],
            &["stash", "apply", "-q"],
            &["stash", "pop"],
            &["stash", "pop", "--index"],
            &["stash", "pop", "stash@{0}"],
            &["stash", "apply", "stash@{3}"],
            &["stash", "branch", "side"],
            &["stash", "branch"],
            &["stash", "clear"],
        ],
    );
    // Conflicts with a later commit.
    let conflict = || {
        base()
            .write("a", "stashed\n")
            .git(["stash", "-q"])
            .write("a", "committed\n")
            .git(["commit", "-q", "-am", "second"])
            .tick()
    };
    each(
        conflict,
        &[
            &["stash", "apply"],
            &["stash", "pop"],
            &["stash", "apply", "--label-ours=UP", "--label-theirs=ST"],
        ],
    );
    // Local changes in the way.
    let in_the_way = || {
        base()
            .write("a", "stashed\n")
            .write("u", "u\n")
            .git(["stash", "-q", "-u"])
            .write("a", "local\n")
    };
    each(in_the_way, &[&["stash", "apply"], &["stash", "pop"]]);
    let untracked_in_the_way = || {
        base()
            .write("u", "u\n")
            .git(["stash", "-q", "-u"])
            .write("u", "other\n")
    };
    each(untracked_in_the_way, &[&["stash", "apply"]]);
}

#[test]
fn store_and_create() {
    let mut fx = base().write("a", "changed\n");
    if fx.is_skipped() {
        return;
    }
    let w = String::from_utf8(fx.git_stdout(["stash", "create", "made"]))
        .expect("utf-8")
        .trim()
        .to_owned();
    for args in [
        &["stash", "store", w.as_str()][..],
        &["stash", "store", "-m", "custom", w.as_str()],
        &["stash", "store", "HEAD"],
        &["stash", "store"],
    ] {
        fx.compare(args).run();
    }
}

#[test]
fn export_and_import() {
    let mut fx = dirty()
        .git(["stash", "-q"])
        .tick()
        .write("a", "x\n")
        .git(["stash", "-q"])
        .tick();
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["stash", "export", "--print"][..],
        &["stash", "export", "--to-ref", "refs/heads/exp"],
        &["stash", "export", "--print", "stash@{1}"],
        &["stash", "export"],
        &["stash", "import", "HEAD"],
    ] {
        fx.compare(args).run();
    }
    let exported = String::from_utf8(fx.git_stdout(["stash", "export", "--print"]))
        .expect("utf-8")
        .trim()
        .to_owned();
    fx.git(["stash", "clear"])
        .same(["stash", "import", exported.as_str()])
        .same(["stash", "list"]);
}

#[test]
fn from_subdirectory() {
    let mut fx = base().write("d/c", "changed\n").write("a", "changed\n");
    if fx.is_skipped() {
        return;
    }
    fx.compare(["stash", "push", "--", "c"]).in_dir("d").run();
    fx.git(["stash", "-q"]).tick().same(["stash", "list"]);
}
