// SPDX-License-Identifier: MIT
//! Differential tests of rebase and sequencer details against the
//! reference git: `--update-refs` (#155), a `fixup`/`squash` that leaves
//! the commit empty (#443), `:/<text>` names in the shared resolver
//! (#429), attributes brought by an earlier pick (#430), the todo list
//! checks (labels, refs, merge commits), `rebase.instructionFormat`
//! without placeholder, `core.commentChar=auto`, the signing option in
//! the "amend" advice and `cherry-pick`/`revert --edit` of several
//! commits.
//!
//! The editors are scripts in `.git`: the sequence editor prints the list
//! it gets to stderr and replaces it with `.git/new-todo` when that
//! exists; the message editor prints the message. No real editor is ever
//! started.

use rgit_difftest::{Comparison, Fixture};

const SEQ_EDITOR: &str =
    "#!/bin/sh\ncat \"$1\" >&2\nif test -f .git/new-todo; then cat .git/new-todo >\"$1\"; fi\n";
const MSG_EDITOR: &str = "#!/bin/sh\necho '--- editor' >&2\ncat \"$1\" >&2\n";

/// `main`: A (a) - M (m); `topic` (checked out): A - B (b) - C (c) -
/// R (reverts C) - D (d) - "fixup! B" (empty) - E (e). Branches `at-b`
/// and `also-b` point to B, `at-d` to D, `tip` to E.
fn fixture() -> Fixture {
    let fx = Fixture::new()
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
        .git(["checkout", "-q", "-b", "topic"])
        .tick()
        .write("b", "b\n")
        .git(["add", "b"])
        .git(["commit", "-q", "-m", "B"])
        .git(["branch", "at-b"])
        .git(["branch", "also-b"])
        .tick()
        .write("c", "c\n")
        .git(["add", "c"])
        .git(["commit", "-q", "-m", "C"])
        .tick()
        .git(["revert", "--no-edit", "HEAD"])
        .tick()
        .write("d", "d\n")
        .git(["add", "d"])
        .git(["commit", "-q", "-m", "D"])
        .git(["branch", "at-d"])
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "fixup! B"])
        .tick()
        .write("e", "e\n")
        .git(["add", "e"])
        .git(["commit", "-q", "-m", "E"])
        .git(["branch", "tip"]);
    fx.git(["checkout", "-q", "main"])
        .tick()
        .write("m", "m\n")
        .git(["add", "m"])
        .git(["commit", "-q", "-m", "M"])
        .git(["checkout", "-q", "topic"])
        .tick()
}

const STATE_FILES: &[&str] = &[
    "REBASE_HEAD",
    "MERGE_MSG",
    "CHERRY_PICK_HEAD",
    "rebase-merge/head-name",
    "rebase-merge/git-rebase-todo",
    "rebase-merge/done",
    "rebase-merge/msgnum",
    "rebase-merge/end",
    "rebase-merge/message",
    "rebase-merge/author-script",
    "rebase-merge/stopped-sha",
    "rebase-merge/amend",
    "rebase-merge/rewritten-list",
    "rebase-merge/update-refs",
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
fn meld_that_empties_the_commit_stops() {
    // topic~4 is C, topic~3 reverts it.
    let mut all = Vec::new();
    for cmd in ["fixup", "squash"] {
        let t = todo(&["pick topic~4", &format!("{cmd} topic~3"), "pick topic~2"]);
        all.push(format!("{t}; git rebase -i HEAD~5"));
        all.push(format!("{t}; git rebase -i HEAD~5; git rebase --continue"));
        all.push(format!("{t}; git rebase -i HEAD~5; git rebase --skip"));
        all.push(format!("{t}; git rebase -i HEAD~5; git rebase --abort"));
    }
    // In the middle of a chain, and an empty commit melded into an empty
    // one (no stop).
    all.push(format!(
        "{}; git rebase -i HEAD~5",
        todo(&["pick topic~4", "fixup topic~3", "fixup topic~2"])
    ));
    all.push(format!(
        "{}; git rebase -i HEAD~2",
        todo(&["pick topic~1", "fixup topic~1"])
    ));
    let refs: Vec<&str> = all.iter().map(String::as_str).collect();
    scripts(|fx| fx, &refs);
}

#[test]
fn update_refs_lists() {
    scripts(
        |fx| fx,
        &[
            "git rebase -i --update-refs main",
            "git -c rebase.updateRefs=true rebase -i main",
            "git -c rebase.updateRefs=true rebase -i --no-update-refs main",
            "git rebase -i --update-refs --autosquash main",
            "git rebase -i --update-refs --exec true main",
            "git rebase --update-refs main",
            "git -c core.commentChar=% rebase -i --update-refs main",
            "git -c rebase.instructionFormat='%s%d' rebase -i --update-refs main",
            // A branch checked out in another worktree gets a comment, and
            // then so does the one being rebased.
            "git worktree add -q ../wt at-d && git rebase -i --update-refs main; git worktree remove --force ../wt",
        ],
    );
}

#[test]
fn update_refs_edits() {
    let stop = todo(&[
        "pick topic~5",
        "update-ref refs/heads/at-b",
        "break",
        "pick topic~4",
        "pick topic~3",
        "pick topic~2",
        "update-ref refs/heads/at-d",
        "update-ref refs/heads/new-branch",
        "pick topic~1",
        "pick topic",
        "update-ref refs/heads/tip",
    ]);
    scripts(
        |fx| fx,
        &[
            // Locked while the rebase runs.
            &format!(
                "{stop}; git rebase -i --update-refs main; git branch -f at-d main; git branch -f also-b main; git rebase --continue"
            ),
            // Edited away: unlocked and not updated; added: recorded.
            &format!(
                "{stop}; git rebase -i --update-refs main; {}; git rebase --edit-todo; git branch -f at-d main; git rebase --continue",
                todo(&[
                    "pick topic~4",
                    "update-ref refs/heads/also-b",
                    "pick topic~2"
                ])
            ),
            // Moved by someone else meanwhile: the update fails.
            &format!(
                "{stop}; git rebase -i --update-refs main; git update-ref refs/heads/at-d main; git rebase --continue"
            ),
            // Created meanwhile.
            &format!(
                "{stop}; git rebase -i --update-refs main; git branch new-branch main; git rebase --continue"
            ),
        ],
    );
}

#[test]
fn todo_checks() {
    let prep = |fx: Fixture| {
        fx.git(["merge", "-q", "--no-ff", "-m", "merge", "main"])
            .tick()
    };
    scripts(
        prep,
        &[
            &format!(
                "{}; git rebase -i HEAD",
                todo(&[
                    "exec >execed",
                    "label #",
                    "label :invalid",
                    "label ok-label",
                    "label a..b",
                    "update-ref :bad",
                    "update-ref topic",
                    "update-ref refs/heads/ok",
                    "update-ref refs/heads/x.lock",
                ])
            ),
            &format!(
                "{}; git rebase -i HEAD",
                todo(&[
                    "pick HEAD",
                    "reword HEAD",
                    "edit HEAD",
                    "fixup HEAD",
                    "squash HEAD",
                    "drop HEAD",
                ])
            ),
            &format!(
                "{}; git -c advice.rebaseTodoError=false rebase -i HEAD",
                todo(&["pick HEAD", "edit HEAD"])
            ),
        ],
    );
}

#[test]
fn instruction_format_and_comment_char() {
    scripts(
        |fx| fx,
        &[
            "git -c rebase.instructionFormat=blah rebase -i main",
            "git -c rebase.instructionFormat=blah rebase --autosquash --force-rebase -i HEAD^",
            "git -c rebase.instructionFormat='%h %s' rebase -i HEAD~2",
            "git -c core.commentChar=auto rebase -i HEAD~2",
            "git -c core.commentChar=auto commit -q --allow-empty -m x",
        ],
    );
}

#[test]
fn signing_option_in_amend_advice() {
    let edit = todo(&["edit topic~1", "pick topic"]);
    scripts(
        |fx| fx,
        &[
            &format!("{edit}; git rebase -i --gpg-sign='\"S I Gner\"' HEAD~2"),
            &format!("{edit}; git rebase -i -S HEAD~2"),
            &format!("{edit}; git -c commit.gpgSign=true rebase -i --no-gpg-sign HEAD~2"),
        ],
    );
}

#[test]
fn message_search_names() {
    scripts(
        |fx| fx,
        &[
            "git rebase --onto ':/M' ':/^C'",
            "git rebase --onto ':/!-M' main",
            "git reset -q --hard ':/^D' && git log --format=%s -1",
            "git cherry-pick ':/^M'",
            "git rebase ':/nothing matches this'",
        ],
    );
}

#[test]
fn attributes_of_earlier_picks_apply() {
    // `attr` adds a .gitattributes that turns a.txt into CRLF in the work
    // tree; a later commit adds a.txt.
    let prep = |fx: Fixture| {
        fx.git(["checkout", "-q", "-b", "attr", "main"])
            .tick()
            .write(".gitattributes", "*.txt text eol=crlf\n")
            .git(["add", ".gitattributes"])
            .git(["commit", "-q", "-m", "attributes"])
            .tick()
            .write("a.txt", "one\ntwo\n")
            .git(["add", "a.txt"])
            .git(["commit", "-q", "-m", "text"])
            .tick()
    };
    scripts(
        prep,
        &[
            "git rebase --force-rebase main~1",
            "git rebase -i main~1",
            "git checkout -q main && git cherry-pick attr~1 attr",
        ],
    );
}

#[test]
fn pick_and_revert_several_with_edit() {
    scripts(
        |fx| fx.env("EDITOR", ":").env("GIT_EDITOR", ":"),
        &[
            "git revert --edit topic topic~2",
            "git checkout -q main && git cherry-pick --edit topic~2 topic",
        ],
    );
}
