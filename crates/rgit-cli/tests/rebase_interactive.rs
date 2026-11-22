// SPDX-License-Identifier: MIT
//! Differential tests of the interactive rebase (#155): the todo list the
//! sequence editor sees, the commands (`pick`, `reword`, `edit`,
//! `squash`, `fixup [-C|-c]`, `exec`, `break`, `drop`, `label`, `reset`,
//! `merge`), `--autosquash`, `--exec`, `--edit-todo`,
//! `rebase.missingCommitsCheck`, against the reference git.
//!
//! The editors are scripts in `.git`: the sequence editor prints the list
//! it gets to stderr and replaces it with `.git/new-todo` when that
//! exists; the message editor prints the message and replaces it with
//! `.git/new-msg` when that exists. No real editor is ever started.

use rgit_difftest::{Comparison, Fixture};

const SEQ_EDITOR: &str =
    "#!/bin/sh\ncat \"$1\" >&2\nif test -f .git/new-todo; then cat .git/new-todo >\"$1\"; fi\n";
const MSG_EDITOR: &str = "#!/bin/sh\necho '--- editor' >&2\ncat \"$1\" >&2\nif test -f .git/new-msg; then cat .git/new-msg >\"$1\"; fi\n";

/// `main`: A (a) - M (m); `topic` (checked out): A - B (b) - C (c) -
/// D (d) - "fixup! B" - "squash! C" - "amend! B" - E (e).
fn fixture() -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .git(["config", "advice.detachedHead", "false"])
        .write_executable(".git/seq.sh", SEQ_EDITOR)
        .write_executable(".git/msg.sh", MSG_EDITOR)
        .env("GIT_SEQUENCE_EDITOR", ".git/seq.sh")
        .env("GIT_EDITOR", ".git/msg.sh")
        .tick()
        .write("a", "a\n")
        .git(["add", "a"])
        .git(["commit", "-q", "-m", "A"])
        .git(["checkout", "-q", "-b", "topic"]);
    for (file, content, subject, body) in [
        ("b", "b\n", "B", ""),
        ("c", "c\n", "C", "C body"),
        ("d", "d\n", "D", ""),
        ("b", "b\nfix\n", "fixup! B", ""),
        ("c", "c\nsq\n", "squash! C", "squash body"),
        ("b", "b\nfix\namend\n", "amend! B", "B reworded"),
        ("e", "e\n", "E", ""),
    ] {
        fx = fx.tick().write(file, content).git(["add", file]);
        fx = if body.is_empty() {
            fx.git(["commit", "-q", "-m", subject])
        } else {
            fx.git(["commit", "-q", "-m", subject, "-m", body])
        };
    }
    fx.git(["checkout", "-q", "main"])
        .tick()
        .write("m", "m\n")
        .git(["add", "m"])
        .git(["commit", "-q", "-m", "M"])
        .git(["checkout", "-q", "topic"])
        .tick()
}

const STATE_FILES: &[&str] = &[
    "AUTO_MERGE",
    "REBASE_HEAD",
    "MERGE_MSG",
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "rebase-merge/head-name",
    "rebase-merge/onto",
    "rebase-merge/orig-head",
    "rebase-merge/git-rebase-todo",
    "rebase-merge/git-rebase-todo.backup",
    "rebase-merge/done",
    "rebase-merge/msgnum",
    "rebase-merge/end",
    "rebase-merge/message",
    "rebase-merge/author-script",
    "rebase-merge/stopped-sha",
    "rebase-merge/amend",
    "rebase-merge/interactive",
    "rebase-merge/drop_redundant_commits",
    "rebase-merge/keep_redundant_commits",
    "rebase-merge/no-reschedule-failed-exec",
    "rebase-merge/reschedule-failed-exec",
    "rebase-merge/rewritten-list",
];

fn files(mut c: Comparison<'_>) -> Comparison<'_> {
    for f in STATE_FILES {
        c = c.git_file(*f);
    }
    c
}

/// Runs each shell script (where `git` is the binary under test) in a
/// fresh fixture prepared by `prep`; reports every differing case.
fn scripts(prep: impl Fn(Fixture) -> Fixture, scripts: &[&str]) {
    let mut failed = Vec::new();
    for s in scripts {
        let fx = prep(fixture());
        if fx.is_skipped() {
            return;
        }
        let mut fx = fx;
        let alias = format!("alias.t=!{s}");
        if let Err(m) = files(fx.compare(["-c", alias.as_str(), "t"])).try_run() {
            failed.push(m.to_string());
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

/// The todo list `lines` for the sequence editor, as a shell command.
fn todo(lines: &[&str]) -> String {
    let mut s = String::from("printf '%s\\n'");
    for l in lines {
        s.push_str(&format!(" '{l}'"));
    }
    s.push_str(" >.git/new-todo");
    s
}

#[test]
fn generated_list() {
    scripts(
        |fx| fx,
        &[
            "git rebase -i main",
            "git rebase -i HEAD~3",
            "git rebase -i --autosquash main",
            "git -c rebase.autoSquash=true rebase -i main",
            "git -c rebase.autoSquash=true rebase -i --no-autosquash main",
            "git rebase --autosquash main",
            "git -c rebase.abbreviateCommands=true rebase -i main",
            "git -c rebase.instructionFormat='%an <%ae>: %s' rebase -i HEAD~2",
            "git -c core.commentChar=';' rebase -i HEAD~2",
            "git -c rebase.missingCommitsCheck=warn rebase -i HEAD~2",
            "git rebase -i --root",
            "git rebase -i HEAD",
            "git rebase -i --onto main topic~2",
            "git rebase -i --keep-base main",
        ],
    );
}

#[test]
fn exec() {
    scripts(
        |fx| fx,
        &[
            "git rebase --exec 'echo run' main",
            "git rebase -i -x 'echo one' -x 'echo two' HEAD~2",
            "git rebase --autosquash --exec true main",
            "git rebase --exec false HEAD~2",
            "git rebase --exec false HEAD~2; git rebase --continue",
            "git rebase --reschedule-failed-exec --exec false HEAD~2",
            "git rebase --exec 'echo x >>b' HEAD~2",
            "git rebase --exec 'echo x >>b; git add b' HEAD~2",
            "git rebase --exec nonexistent-command-xyz HEAD~1",
            "git rebase --exec 'echo new >new' HEAD~1",
            // -q hides "Executing: ..." (t3400 "rebase --exec -q is quiet").
            "git rebase --exec true -q main",
            "git rebase -i -q -x true HEAD~2",
            // The diffstat of the new base on unrelated histories
            // (t3406 "rebase -i onto unrelated history").
            "git checkout -q --orphan o && git rm -q -rf . && echo z >z && git add z && git commit -q -m Z && git rebase -i --stat main",
        ],
    );
}

#[test]
fn custom_lists() {
    let cases = [
        todo(&["pick topic~6", "drop topic~5", "pick topic~4"]),
        todo(&["pick topic~4", "pick topic~6"]),
        todo(&[
            "# comment",
            "",
            "  pick topic~6   with text  ",
            "",
            "p topic~5",
            "# trailing",
        ]),
        todo(&["break", "pick topic~6"]),
        todo(&["pick topic~6", "break", "pick topic~5"]),
        todo(&["pick topic~6", "exec echo hello", "noop"]),
        todo(&["# nothing"]),
        todo(&["bogus line", "pick nonexistent", "exec", "pick"]),
        todo(&["break now", "noop x", "fixup -C", "merge -C"]),
    ];
    let all: Vec<String> = cases
        .iter()
        .map(|t| format!("{t}; git rebase -i main"))
        .collect();
    let refs: Vec<&str> = all.iter().map(String::as_str).collect();
    scripts(|fx| fx, &refs);
}

#[test]
fn squash_and_fixup() {
    let cases = [
        todo(&["pick topic~6", "fixup topic~3", "pick topic~5"]),
        todo(&["pick topic~6", "squash topic~5"]),
        todo(&["pick topic~6", "fixup topic~5", "squash topic~4"]),
        todo(&["pick topic~6", "fixup -C topic~4", "pick topic~5"]),
        todo(&["pick topic~6", "fixup -c topic~4"]),
        todo(&["pick topic~6", "squash topic~5", "fixup -C topic~4"]),
        todo(&["pick topic~6", "fixup -C topic~1", "fixup topic~3"]),
        todo(&["pick topic~5", "squash topic~2"]),
        todo(&[
            "pick topic~6",
            "fixup topic~3",
            "fixup -C topic~1",
            "exec true",
        ]),
        todo(&["pick topic~6", "squash topic~1"]),
    ];
    let all: Vec<String> = cases
        .iter()
        .flat_map(|t| {
            [
                format!("{t}; git rebase -i main"),
                format!("{t}; git rebase -i HEAD~7"),
            ]
        })
        .collect();
    let refs: Vec<&str> = all.iter().map(String::as_str).collect();
    scripts(|fx| fx, &refs);
    // With the hook that sees the rewritten commits.
    scripts(
        |fx| {
            fx.write_executable(
                ".git/hooks/post-rewrite",
                "#!/bin/sh\necho \"post-rewrite $*\" >&2\ncat >&2\n",
            )
        },
        &[&format!(
            "{}; git rebase -i main",
            todo(&[
                "pick topic~6",
                "fixup topic~3",
                "squash topic~1",
                "pick topic~5"
            ])
        )],
    );
}

#[test]
fn reword_and_edit() {
    let reword = todo(&["pick topic~6", "reword topic~5", "pick topic~4"]);
    let edit = todo(&["pick topic~6", "edit topic~5", "pick topic~4"]);
    scripts(
        |fx| fx,
        &[
            &format!("{reword}; git rebase -i main"),
            &format!("{reword}; git rebase -i HEAD~7"),
            &format!("{reword}; echo New subject >.git/new-msg; git rebase -i main"),
            &format!("{edit}; git rebase -i main"),
            &format!("{edit}; git rebase -i HEAD~7"),
            &format!("{edit}; git rebase -i main; git status; git rebase --continue"),
            &format!("{edit}; git rebase -i main; echo more >>c; git add c; git rebase --continue"),
            &format!(
                "{edit}; git rebase -i main; git commit -q --amend -m changed; git rebase --continue"
            ),
            &format!(
                "{edit}; git rebase -i main; git reset -q HEAD^; git status; git add c; git commit -q -m split; git rebase --continue"
            ),
            &format!("{edit}; git rebase -i main; git rebase --abort"),
            &format!("{edit}; git rebase -i main; git rebase --skip"),
        ],
    );
    // reword runs the pre-commit and commit-msg hooks (also with
    // --no-verify, which only skips pre-rebase); squash and fixup do not
    // (t7504 "hook is called for reword during `rebase -i`").
    let squash = todo(&["pick topic~6", "squash topic~5", "fixup topic~4"]);
    scripts(
        |fx| {
            fx.write_executable(".git/hooks/pre-commit", "#!/bin/sh\necho pre-commit >&2\n")
                .write_executable(
                    ".git/hooks/commit-msg",
                    "#!/bin/sh\necho \"commit-msg $(head -n 1 \"$1\")\" >&2\necho hooked >\"$1\"\n",
                )
        },
        &[
            &format!("{reword}; git rebase -i main"),
            &format!("{reword}; git rebase --no-verify -i main"),
            &format!("{squash}; git rebase -i main"),
        ],
    );
}

#[test]
fn edit_todo_and_missing_commits() {
    let one = todo(&["pick topic~6"]);
    scripts(
        |fx| fx,
        &[
            &format!(
                "{}; git rebase -i main; {}; git rebase --edit-todo; git rebase --continue",
                todo(&["break", "pick topic~6", "pick topic~5"]),
                todo(&["drop topic~5", "", "pick topic~4"])
            ),
            &format!(
                "{}; git rebase -i main; {}; git rebase --edit-todo; git rebase --continue; {}; git rebase --edit-todo; git rebase --continue",
                todo(&["break", "pick topic~6"]),
                todo(&["bogus"]),
                todo(&["pick topic~5"])
            ),
            &format!("{one}; git -c rebase.missingCommitsCheck=warn rebase -i main"),
            &format!("{one}; git -c rebase.missingCommitsCheck=error rebase -i main"),
            &format!(
                "{one}; git -c rebase.missingCommitsCheck=error rebase -i main; git -c rebase.missingCommitsCheck=error rebase --continue"
            ),
            &format!(
                "{one}; git -c rebase.missingCommitsCheck=error rebase -i main; git rebase --abort"
            ),
            &format!(
                "{}; git -c rebase.missingCommitsCheck=error rebase -i HEAD~2",
                todo(&["drop topic~1", "pick topic"])
            ),
            &format!(
                "{}; git rebase -i main; git rebase --continue",
                todo(&["bogus", "pick topic~6"])
            ),
            "git rebase --edit-todo",
            "git rebase main~1 2>/dev/null; git rebase --edit-todo",
        ],
    );
}

#[test]
fn conflicts() {
    // `conf` changes b like `fixup! B` does.
    let prep = |fx: Fixture| {
        fx.git(["checkout", "-q", "-b", "conf", "main"])
            .tick()
            .write("b", "other\n")
            .git(["add", "b"])
            .git(["commit", "-q", "-m", "conflicting B"])
            .git(["checkout", "-q", "topic"])
            .tick()
    };
    scripts(
        prep,
        &[
            &format!(
                "{}; git rebase -i conf",
                todo(&["pick topic~6", "pick topic~5"])
            ),
            &format!(
                "{}; git rebase -i conf; echo r >b; git add b; git rebase --continue",
                todo(&["reword topic~6", "pick topic~5"])
            ),
            &format!(
                "{}; git rebase -i conf; echo r >b; git add b; git rebase --continue",
                todo(&["pick topic~5", "squash topic~6", "pick topic~4"])
            ),
            &format!(
                "{}; git rebase -i conf; echo r >b; git add b; git rebase --continue",
                todo(&["pick topic~5", "fixup topic~6", "fixup topic~4"])
            ),
            &format!(
                "{}; git rebase -i conf; git rebase --skip",
                todo(&["pick topic~6", "pick topic~5"])
            ),
        ],
    );
}

#[test]
fn autosquash_matching() {
    // More markers: by object name, by prefix, nested, and one whose
    // target comes later (it stays where it is).
    let prep = |fx: Fixture| {
        let b = String::from_utf8(fx.git_stdout(["rev-parse", "topic~6"])).expect("utf-8");
        let b = b.trim().to_owned();
        fx.tick()
            .write("f", "f\n")
            .git(["add", "f"])
            .git(["commit", "-q", "-m", &format!("fixup! {b}")])
            .tick()
            .write("g", "g\n")
            .git(["add", "g"])
            .git(["commit", "-q", "-m", "squash! D"])
            .tick()
            .write("h", "h\n")
            .git(["add", "h"])
            .git(["commit", "-q", "-m", "fixup! fixup! B"])
            .tick()
            .write("i", "i\n")
            .git(["add", "i"])
            .git(["commit", "-q", "-m", "fixup! Later"])
            .tick()
            .write("j", "j\n")
            .git(["add", "j"])
            .git(["commit", "-q", "-m", "Later"])
            .tick()
    };
    scripts(
        prep,
        &[
            "git rebase -i --autosquash main",
            "git rebase --autosquash -x true main",
            "git -c rebase.autoSquash=true rebase -i -x 'echo ran' main",
        ],
    );
}

#[test]
fn more_stops_and_options() {
    let edit = todo(&["edit topic~6", "pick topic~5"]);
    scripts(
        |fx| fx,
        &[
            &format!(
                "{}; git rebase -q -i main",
                todo(&["pick topic~6", "pick topic~5"])
            ),
            &format!(
                "{}; git rebase -i main",
                todo(&["x echo a", "f -c topic~5", "pick topic~4"])
            ),
            &format!("{}; git rebase -i main", todo(&["pick topic~6", "break"])),
            &format!("{edit}; git rebase -i main; git rebase --skip"),
            &format!(
                "{edit}; git rebase -i main; echo e >>b; git status --short; git rebase --continue"
            ),
            &format!("{edit}; git rebase -i main; git rebase --edit-todo; git rebase --continue"),
            &format!(
                "{}; git -c rebase.missingCommitsCheck=bogus rebase -i main",
                todo(&["pick topic~6"])
            ),
            &format!(
                "{}; git -c rebase.missingCommitsCheck=error rebase -i main; {}; git rebase --edit-todo; git -c rebase.missingCommitsCheck=error rebase --continue",
                todo(&["pick topic~6"]),
                todo(&[
                    "pick topic~6",
                    "drop topic~5",
                    "drop topic~4",
                    "drop topic~3",
                    "drop topic~2",
                    "drop topic~1",
                    "drop topic"
                ])
            ),
        ],
    );
    scripts(
        |fx| fx.write("a", "dirty\n"),
        &[&format!(
            "{}; git rebase -i --autostash main; git stash list",
            todo(&["pick topic~6", "break"])
        )],
    );
}

#[test]
fn labels_and_merges() {
    let cases = [
        todo(&[
            "label onto",
            "pick topic~6",
            "label one",
            "reset onto",
            "pick topic~5",
            "merge one",
            "pick topic~4",
        ]),
        todo(&[
            "label onto",
            "pick topic~6",
            "label one",
            "reset onto",
            "pick topic~5",
            "merge one # Join the first",
        ]),
        todo(&["reset nonexistent-label", "pick topic~6"]),
    ];
    let all: Vec<String> = cases
        .iter()
        .map(|t| format!("{t}; git rebase -i main; git for-each-ref refs/rewritten"))
        .collect();
    let refs: Vec<&str> = all.iter().map(String::as_str).collect();
    scripts(|fx| fx, &refs);
}
