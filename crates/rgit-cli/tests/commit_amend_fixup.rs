// SPDX-License-Identifier: MIT
//! `commit --amend` with `--fixup`/`--squash`, `--trailer` on a verbatim
//! message without a final newline, amending a root commit without
//! changes and the `core.commentChar=auto` advice, against the reference
//! git.

use rgit_difftest::Fixture;

fn base() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("f", "a\n")
        .git(["add", "f"])
        .git(["commit", "-q", "-m", "first subject"])
        .git(["tag", "first"])
        .tick()
        .write("f", "b\n")
        .git(["commit", "-q", "-a", "-m", "second subject"])
        .tick()
        .write("f", "c\n")
        .git(["add", "f"])
        .write("msg", "subject")
        .write("msg2", "a\n\nb")
        .write(".git/info/exclude", "msg*\nconfig-include\n")
}

fn cases(prep: impl Fn() -> Fixture, cases: &[&[&str]]) {
    let mut failed = Vec::new();
    for args in cases {
        let mut fx = prep();
        if fx.is_skipped() {
            return;
        }
        if let Err(m) = fx.compare(*args).env("GIT_EDITOR", "true").try_run() {
            failed.push(m.to_string());
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

#[test]
fn amend_with_fixup_and_squash() {
    cases(
        base,
        &[
            &["commit", "--amend", "--fixup=first"],
            &["commit", "--amend", "--fixup=amend:first"],
            &["commit", "--amend", "--fixup=reword:first"],
            &["commit", "--amend", "--squash=first"],
            &["commit", "--amend", "--squash=first", "-m", "extra"],
            &["commit", "--amend", "--fixup=first", "--no-edit"],
            &["commit", "--squash=first", "--fixup=first"],
        ],
    );
}

#[test]
fn trailers_after_message_without_newline() {
    let mut all: Vec<Vec<&str>> = Vec::new();
    for file in ["msg", "msg2"] {
        for cleanup in [
            "--cleanup=verbatim",
            "--cleanup=whitespace",
            "--cleanup=strip",
        ] {
            for trailer in ["Signed-off-by: x <y>", "x: y"] {
                all.push(vec![
                    "commit",
                    "-q",
                    cleanup,
                    "-F",
                    file,
                    "--trailer",
                    trailer,
                ]);
            }
        }
    }
    let refs: Vec<&[&str]> = all.iter().map(Vec::as_slice).collect();
    cases(base, &refs);
}

#[test]
fn amend_root_without_changes() {
    cases(
        || base().git(["reset", "-q", "--hard", "first"]),
        &[&["commit", "--amend"], &["commit", "--amend", "--no-edit"]],
    );
}

#[test]
fn comment_char_auto_advice() {
    cases(
        || {
            let fx = base();
            if fx.is_skipped() {
                return fx;
            }
            let include = fx.work_dir().join("config-include");
            fx.write(
                "config-include",
                "[core]\n    commentString=:\n    commentString=%\n    commentChar=auto\n",
            )
            .git(["config", "core.commentChar", "!"])
            .git(["config", "include.path", &include.display().to_string()])
        },
        &[&["commit", "-q", "--amend"]],
    );
    cases(
        || base().git(["config", "core.commentChar", "auto"]),
        &[
            &["commit", "-q", "--amend"],
            &["-c", "core.commentChar=auto", "commit", "-q", "--amend"],
        ],
    );
}
