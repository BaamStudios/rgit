// SPDX-License-Identifier: MIT
//! `tag` against the reference git: tag objects (same ids for the same
//! input), refs, reflogs, `TAG_EDITMSG`, the editor template, listing with
//! filters, sorting and formats, deletion and the error messages. Dates
//! are fixed by the fixture.

use rgit_difftest::{Check, Fixture};

/// An editor that shows the template on stderr and writes a message.
const EDITOR: &str = "f() { cat \"$1\" >&2; printf 'edited\\n\\n# gone\\n' >\"$1\"; }; f";

/// An editor that shows the template and leaves it alone.
const SHOW_EDITOR: &str = "f() { cat \"$1\" >&2; }; f";

const CHECKS: [Check; 7] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Refs,
    Check::Reflogs,
    Check::Objects,
    Check::Worktree,
];

/// Three commits on `main`, a side branch, and tags of every kind.
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .git(["config", "core.logAllRefUpdates", "always"])
        .write("a", "1\n")
        .git(["add", "a"])
        .git(["commit", "-q", "-m", "one\nsecond line\n\nbody"])
        .tick()
        .git(["tag", "v1.0"])
        .git(["tag", "-m", "annotated one", "ann1"])
        .git(["tag", "-m", "of a tree", "tree-tag", "HEAD^{tree}"])
        .git(["tag", "light-blob", "HEAD:a"])
        .write("a", "2\n")
        .git(["commit", "-q", "-a", "-m", "two"])
        .tick()
        .git(["tag", "v1.10"])
        .git(["tag", "-a", "-m", "line one\nline two\nline three", "v1.2"])
        .git(["tag", "v1.2-rc1", "HEAD~1"])
        .write("a", "3\n")
        .git(["commit", "-q", "-a", "-m", "three"])
        .tick()
        .git(["branch", "side", "HEAD~2"])
        .write("msg", "from a file\n# comment\n")
}

#[track_caller]
fn all_same(cases: &[&[&str]]) {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let mut failures = Vec::new();
    for args in cases {
        if let Err(m) = fx
            .compare(*args)
            .checks(CHECKS)
            .git_file("TAG_EDITMSG")
            .try_run()
        {
            failures.push(m.to_string());
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[track_caller]
fn with_editor(editor: &str, cases: &[&[&str]]) {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let mut failures = Vec::new();
    for args in cases {
        if let Err(m) = fx
            .compare(*args)
            .checks(CHECKS)
            .git_file("TAG_EDITMSG")
            .env("GIT_EDITOR", editor)
            .try_run()
        {
            failures.push(m.to_string());
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
fn lightweight() {
    all_same(&[
        &["tag", "new"],
        &["tag", "new", "HEAD~1"],
        &["tag", "--create-reflog", "new", "HEAD~2"],
        &["tag", "tree", "HEAD^{tree}"],
        &["tag", "of-tag", "ann1"],
        &["tag", "v1.0"],
        &["tag", "-f", "v1.0"],
        &["tag", "-f", "v1.0", "HEAD~1"],
        &["tag", "-f", "new"],
        &["tag", "v1.0/x"],
        &["tag", "x", "nosuch"],
        &["tag", "bad..name"],
        &["tag", "HEAD"],
        &["tag", "--", "-dash"],
        &["tag", "x", "HEAD", "extra"],
    ]);
}

#[test]
fn annotated() {
    all_same(&[
        &["tag", "-a", "-m", "msg", "new"],
        &["tag", "-m", "a", "-m", "b", "new"],
        &["tag", "-m", "", "new"],
        &["tag", "-F", "msg", "new"],
        &["tag", "-F", "nosuch", "new"],
        &["tag", "-m", "a", "-F", "msg", "new"],
        &["tag", "-m", "  lead\n\n# comment\nx  ", "new"],
        &[
            "tag",
            "-m",
            "  lead\n\n# comment\nx  ",
            "--cleanup=whitespace",
            "new",
        ],
        &[
            "tag",
            "-m",
            "  lead\n\n# comment\nx  ",
            "--cleanup=verbatim",
            "new",
        ],
        &["tag", "-m", "x", "--cleanup=scissors", "new"],
        &["tag", "-m", "of a tree", "new", "HEAD^{tree}"],
        &["tag", "-m", "of a blob", "new", "HEAD:a"],
        &["tag", "-m", "nested", "new", "ann1"],
        &[
            "-c",
            "advice.nestedTag=false",
            "tag",
            "-m",
            "nested",
            "new",
            "ann1",
        ],
        &["tag", "-f", "-m", "again", "ann1"],
        &[
            "tag",
            "-m",
            "body",
            "--trailer",
            "a=b",
            "--trailer",
            "c:d",
            "new",
        ],
        &["tag", "-m", "body\n\nKey: v", "--trailer", "a=b", "new"],
        &["tag", "-m", "x", "HEAD"],
    ]);
}

#[test]
fn editor() {
    with_editor(
        EDITOR,
        &[
            &["tag", "-a", "new"],
            &["tag", "-e", "-m", "given", "new"],
            &["tag", "-e", "-F", "msg", "new"],
            &["tag", "-f", "-a", "ann1"],
            &["tag", "--trailer", "a=b", "new"],
            &["tag", "-a", "--cleanup=verbatim", "new"],
        ],
    );
    with_editor(
        SHOW_EDITOR,
        &[
            &["tag", "-a", "new"],
            &["tag", "-a", "--cleanup=whitespace", "new"],
            &["-c", "core.commentChar=;", "tag", "-a", "new"],
            &["tag", "-f", "-a", "ann1"],
        ],
    );
    with_editor("false", &[&["tag", "-a", "new"]]);
}

#[test]
fn delete() {
    all_same(&[
        &["tag", "-d", "v1.0"],
        &["tag", "-d", "ann1", "v1.10"],
        &["tag", "-d", "v1.0", "nosuch", "ann1"],
        &["tag", "-d", "nosuch"],
        &["tag", "-d"],
        &["tag", "--delete", "tree-tag"],
    ]);
}

#[test]
fn list() {
    all_same(&[
        &["tag"],
        &["tag", "-l"],
        &["tag", "-l", "v*"],
        &["tag", "--list", "v1.?", "a*"],
        &["tag", "-l", "V*"],
        &["tag", "-i", "-l", "V*"],
        &["tag", "-n"],
        &["tag", "-n0"],
        &["tag", "-n2"],
        &["tag", "-n99", "-l", "v1.2"],
        &["tag", "-n", "v1.0"],
        &["tag", "--contains", "HEAD~1"],
        &["tag", "--contains"],
        &["tag", "--no-contains", "HEAD~1"],
        &["tag", "--contains", "HEAD~2", "--no-contains", "HEAD"],
        &["tag", "--merged", "HEAD~1"],
        &["tag", "--no-merged", "HEAD~1"],
        &["tag", "--merged", "side"],
        &["tag", "--points-at", "HEAD~1"],
        &["tag", "--points-at", "HEAD~2"],
        &["tag", "--points-at", "ann1"],
        &["tag", "--points-at=HEAD~1", "--points-at=HEAD~2", "v*"],
        &[
            "tag",
            "--format=%(refname) %(objecttype) %(objectname:short)",
        ],
        &["tag", "--format=%(refname:short)", "--omit-empty", "v*"],
        &["tag", "-n", "--format=%(refname)"],
    ]);
}

#[test]
fn sort() {
    all_same(&[
        &["tag", "--sort=-refname"],
        &["tag", "--sort=version:refname"],
        &["tag", "--sort=-v:refname"],
        &["tag", "--sort=objecttype"],
        &["tag", "--sort=version:refname", "--sort=objecttype"],
        &["tag", "--sort=version:tag"],
        &[
            "-c",
            "versionsort.suffix=-rc",
            "tag",
            "--sort=version:refname",
        ],
        &["-c", "tag.sort=-refname", "tag"],
        &["-c", "tag.sort=-refname", "tag", "--sort=refname"],
        &[
            "-c",
            "tag.sort=-refname",
            "tag",
            "--no-sort",
            "--sort=objecttype",
        ],
        &["tag", "--sort=bogus"],
        &["-c", "tag.sort=v:bogus", "tag"],
    ]);
}

#[test]
fn errors() {
    all_same(&[
        &["tag", "-l", "-d", "x"],
        &["tag", "-a"],
        &["tag", "-f"],
        &["tag", "-m", "x"],
        &["tag", "-l", "-m", "x"],
        &["tag", "-d", "-a", "x"],
        &["tag", "-n", "-d", "x"],
        &["tag", "-d", "--contains", "HEAD", "x"],
        &["tag", "-d", "--points-at", "HEAD", "x"],
        &["tag", "--contains", "nosuch"],
        &["tag", "--contains", "tree-tag"],
        &["tag", "--merged", "tree-tag"],
        &["tag", "--points-at", "nosuch"],
        &["tag", "--no-with", "HEAD"],
        &["tag", "-n", "--column"],
        &["tag", "-nx"],
        &["tag", "-l", "--format=%(rest)"],
        &["tag", "-v", "ann1"],
        &["tag", "-v", "v1.0"],
        &["tag", "-v", "nosuch"],
    ]);
}
