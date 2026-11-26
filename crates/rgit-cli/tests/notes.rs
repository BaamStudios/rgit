// SPDX-License-Identifier: MIT
//! Differential tests of `rgit notes` against the reference git: the
//! subcommands, the fan-out of written notes trees, notes in the `log`
//! family and `format-patch`, rewriting on `commit --amend` and `rebase`,
//! and `notes merge` with every strategy.

use rgit_difftest::{Check, Fixture, Normalizer};

fn commit(fx: Fixture, msg: &str) -> Fixture {
    fx.tick()
        .write(format!("{msg}.t"), format!("{msg}\n"))
        .git(["add", "."])
        .git(["commit", "-q", "-m", msg])
        .git(["tag", msg])
}

fn repo(commits: &[&str]) -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"]);
    if fx.is_skipped() {
        return fx;
    }
    for c in commits {
        fx = commit(fx, c);
    }
    fx
}

#[test]
fn subcommands() {
    let mut fx = repo(&["c1", "c2", "c3"]);
    if fx.is_skipped() {
        return;
    }
    fx = fx
        .same(["notes", "add", "-m", "first", "c1"])
        .same(["notes", "add", "-m", "again", "c1"])
        .same(["notes", "add", "-f", "-m", "a", "-m", "b", "c1"])
        .same(["notes", "append", "-m", "more", "c1"])
        .same([
            "notes",
            "append",
            "--separator=--",
            "-m",
            "x",
            "-m",
            "y",
            "c1",
        ])
        .same([
            "notes",
            "append",
            "--no-separator",
            "--no-stripspace",
            "-m",
            "z",
            "c1",
        ])
        .same([
            "notes",
            "add",
            "--no-stripspace",
            "-m",
            "\n a  \n\n\n b\n",
            "c2",
        ])
        .same(["notes", "copy", "c1", "c3"])
        .same(["notes", "copy", "c2", "c3"])
        .same(["notes", "copy", "-f", "c2", "c3"])
        .same(["notes", "copy"])
        .same(["notes", "list"])
        .same(["notes"])
        .same(["notes", "list", "c2"])
        .same(["notes", "show", "c1"])
        .same(["notes", "show", "HEAD~2"])
        .same(["notes", "get-ref"])
        .same(["notes", "--ref", "other", "get-ref"])
        .same(["notes", "--ref", "refs/heads/x", "get-ref"])
        .same(["notes", "--ref", "other", "add", "-m", "o", "c2"])
        .same(["notes", "remove", "c3", "c2"])
        .same(["notes", "remove", "c3"])
        .same(["notes", "remove", "--ignore-missing", "c3"])
        .same(["notes", "add", "-m", "", "c3"])
        .same(["notes", "add", "--allow-empty", "-m", "", "c3"])
        .same(["notes", "edit", "-m", "edited", "c3"])
        .same(["notes", "add", "-C", "c1:c1.t", "c2"])
        .same(["notes", "add", "-f", "-C", "c1", "c2"])
        .same(["notes", "show", "nope"])
        .same(["notes", "frob"])
        .same(["notes", "c1"])
        .same(["notes", "prune", "-n", "-v"]);
    fx.compare(["notes", "copy", "--stdin"])
        .stdin(b"c1 c2\n".to_vec())
        .run();
    let c3 = String::from_utf8(fx.git_stdout(["rev-parse", "c3"])).unwrap();
    let c1 = String::from_utf8(fx.git_stdout(["rev-parse", "c1"])).unwrap();
    fx.compare(["notes", "remove", "--stdin"])
        .stdin(format!("{c1}{c3}").into_bytes())
        .run();
    fx.compare(["notes", "list"])
        .env("GIT_NOTES_REF", "refs/notes/other")
        .run();
    fx.compare(["notes", "add", "-m", "x"])
        .env("GIT_NOTES_REF", "refs/heads/main")
        .run();
}

#[test]
fn editor() {
    let mut fx = repo(&["c1"]);
    if fx.is_skipped() {
        return;
    }
    fx = fx.write_executable(
        "ed.sh",
        "#!/bin/sh\ncat \"$1\" >>log\nprintf '%s\\n' \"$MSG\" >\"$1\"\n",
    );
    let ed = fx.work_dir().join("ed.sh");
    let ed = ed.to_str().unwrap();
    for (msg, args) in [
        ("hello", &["notes", "add"][..]),
        ("edited", &["notes", "edit"][..]),
        ("", &["notes", "edit"][..]),
        ("x", &["notes", "add", "-m", "start", "-e"][..]),
        ("y", &["notes", "append"][..]),
        ("# comment only", &["notes", "edit", "--allow-empty"][..]),
    ] {
        fx.compare(args).env("GIT_EDITOR", ed).env("MSG", msg).run();
    }
}

#[test]
fn fanout_matches_git() {
    let mut fx = repo(&["c1"]);
    if fx.is_skipped() {
        return;
    }
    // Enough notes for every first digit to hold two of them (fan-out 1).
    let mut blobs = Vec::new();
    for i in 0..90 {
        fx = fx.write("b", format!("blob {i}\n"));
        let id = String::from_utf8(fx.git_stdout(["hash-object", "-w", "b"])).unwrap();
        blobs.push(id.trim().to_owned());
    }
    for (i, b) in blobs.iter().enumerate().take(85) {
        fx = fx.git(["notes", "add", "-m", &format!("n{i}"), b]);
    }
    for b in &blobs[85..] {
        fx = fx.same(["notes", "add", "-m", "late", b]);
    }
    for b in blobs.iter().take(80) {
        fx = fx.git(["notes", "remove", b]);
    }
    for b in &blobs[80..] {
        fx = fx.same(["notes", "remove", b]);
    }
}

#[test]
fn display() {
    let mut fx = repo(&["c1", "c2", "c3"]);
    if fx.is_skipped() {
        return;
    }
    fx = fx
        .git(["notes", "add", "-m", "n1\n indented\n\nlast", "c2"])
        .git(["notes", "add", "-m", "n3", "c3"])
        .git(["notes", "--ref", "other", "add", "-m", "o3", "c3"]);
    let cases: &[&[&str]] = &[
        &["log"],
        &["log", "--oneline", "--notes"],
        &["log", "--pretty=raw", "--show-notes"],
        &["log", "--pretty=short", "--notes", "--notes=other"],
        &["log", "--notes=other"],
        &[
            "log",
            "--notes",
            "--notes=other",
            "--no-notes",
            "--notes=other",
        ],
        &["log", "--show-notes=*"],
        &["log", "--no-standard-notes", "--show-notes=other"],
        &["log", "--format=%s|%N|"],
        &["log", "--no-notes", "--format=%s|%N|"],
        &["log", "--pretty=email", "--notes"],
        &["log", "--pretty=email", "--notes", "--stat"],
        &["log", "--graph", "--notes"],
        &["log", "-z", "--oneline", "--notes"],
        &["log", "--notes=missing", "-1"],
        &["show", "c2"],
        &["show", "--oneline", "c2"],
        &[
            "format-patch",
            "--stdout",
            "--no-signature",
            "--notes",
            "-3",
        ],
        &[
            "format-patch",
            "--stdout",
            "--no-signature",
            "--notes=other",
            "-1",
        ],
        &["rev-list", "--notes", "HEAD"],
        &["diff-tree", "--format=%N", "c3"],
    ];
    for c in cases {
        fx.compare(*c)
            .checks(Check::OUTPUT)
            .normalize(Normalizer::version_line())
            .run();
    }
    fx.compare(["log", "-3"])
        .env("GIT_NOTES_DISPLAY_REF", "refs/notes/other")
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["-c", "notes.displayRef=refs/notes/*", "log", "-3"])
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["-c", "notes.displayRef", "log", "-1"])
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn rewrite() {
    let mut fx = repo(&["n1", "n2", "n3"]);
    if fx.is_skipped() {
        return;
    }
    fx = fx
        .git(["config", "notes.rewriteRef", "refs/notes/*"])
        .git(["notes", "add", "-m", "a note", "n3"])
        .git(["notes", "add", "-m", "n2 note", "n2"])
        .tick()
        .same(["commit", "--amend", "-m", "n3 amended"])
        .same(["rebase", "-q", "--onto", "n1", "n2"])
        .same(["notes", "show"]);
    let n1 = String::from_utf8(fx.git_stdout(["rev-parse", "n1"])).unwrap();
    let n2 = String::from_utf8(fx.git_stdout(["rev-parse", "n2"])).unwrap();
    for mode in [
        "concatenate",
        "overwrite",
        "ignore",
        "cat_sort_uniq",
        "bogus",
    ] {
        fx.compare(["notes", "copy", "--for-rewrite=foo"])
            .stdin(format!("{} {}", n2.trim(), n1.trim()).into_bytes())
            .env("GIT_NOTES_REWRITE_MODE", mode)
            .run();
    }
}

#[test]
fn merge() {
    let mut fx = repo(&["c1", "c2", "c3", "c4"]);
    if fx.is_skipped() {
        return;
    }
    fx = fx
        .git(["config", "core.notesRef", "refs/notes/x"])
        .git(["notes", "add", "-m", "x2", "c2"])
        .git(["notes", "add", "-m", "x3", "c3"])
        .git(["notes", "add", "-m", "x4\nline", "c4"])
        .git(["update-ref", "refs/notes/y", "refs/notes/x"])
        .git(["update-ref", "refs/notes/z", "refs/notes/x"])
        .git(["notes", "--ref", "y", "add", "-f", "-m", "y1", "c1"])
        .git(["notes", "--ref", "y", "remove", "c2"])
        .git(["notes", "--ref", "y", "add", "-f", "-m", "y3", "c3"])
        .git(["notes", "--ref", "y", "add", "-f", "-m", "y4\nline", "c4"])
        .git(["notes", "--ref", "z", "add", "-f", "-m", "z1", "c1"])
        .git(["notes", "--ref", "z", "add", "-f", "-m", "z2", "c2"])
        .git(["notes", "--ref", "z", "remove", "c3"])
        .git(["notes", "--ref", "z", "add", "-f", "-m", "x4\nz line", "c4"])
        .git(["notes", "--ref", "w", "add", "-m", "w1", "c1"]);
    for s in ["ours", "theirs", "union", "cat_sort_uniq"] {
        fx = fx
            .git(["update-ref", "refs/notes/m", "refs/notes/y"])
            .same([
                "-c",
                "core.notesRef=refs/notes/m",
                "notes",
                "merge",
                "-vv",
                "-s",
                s,
                "z",
            ]);
    }
    let _ = fx
        .git(["update-ref", "refs/notes/m", "refs/notes/y"])
        .same(["-c", "core.notesRef=refs/notes/m", "notes", "merge", "z"])
        .same(["-c", "core.notesRef=refs/notes/m", "notes", "merge", "z"])
        .same(["notes", "merge", "--commit", "--abort"])
        .same(["notes", "merge", "--commit", "-v"])
        .git(["update-ref", "refs/notes/m", "refs/notes/x"])
        .same([
            "-c",
            "core.notesRef=refs/notes/m",
            "notes",
            "merge",
            "-q",
            "z",
        ])
        .same(["notes", "merge", "--abort", "-v"])
        .same([
            "-c",
            "core.notesRef=refs/notes/m",
            "notes",
            "merge",
            "-vv",
            "y",
        ])
        .same([
            "-c",
            "core.notesRef=refs/notes/m",
            "notes",
            "merge",
            "-vv",
            "x",
        ])
        .same([
            "-c",
            "core.notesRef=refs/notes/m",
            "notes",
            "merge",
            "-vv",
            "w",
        ])
        .same(["-c", "core.notesRef=refs/notes/new", "notes", "merge", "y"])
        .same(["-c", "core.notesRef=refs/notes/m", "notes", "merge", "nope"])
        .same(["-c", "core.notesRef=refs/notes/q", "notes", "merge", "nope"])
        .same(["-c", "core.notesRef=refs/heads/main", "notes", "merge", "x"])
        .same(["-c", "notes.mergeStrategy=foo", "notes", "merge", "x"])
        .same(["notes", "merge", "-s", "foo", "x"])
        .same(["notes", "merge"]);
}
