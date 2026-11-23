// SPDX-License-Identifier: MIT
//! Differential tests of `rebase --rebase-merges` (#155) against the
//! reference git: the generated todo list (chains, `branch-point` labels,
//! label names from branches and merge subjects, cousins, roots,
//! `--update-refs`), replaying it (fast-forwards, `--force-rebase`,
//! octopus merges, `merge -c`, conflicts and `--continue`) and
//! `rebase.rebaseMerges`.
//!
//! The sequence editor prints the list it gets to stderr and replaces it
//! with `.git/new-todo` when that exists; the message editor appends a
//! line. No real editor is ever started.

use rgit_difftest::{Comparison, Fixture};

const SEQ_EDITOR: &str =
    "#!/bin/sh\ncat \"$1\" >&2\nif test -f .git/new-todo; then cat .git/new-todo >\"$1\"; fi\n";
const MSG_EDITOR: &str = "#!/bin/sh\necho edited >>\"$1\"\n";

/// ```text
///     -- B --                   (first)
///    /       \
///  A - C - D - E - H            (main, checked out)
///    \    \       /
///     \    F - G                (second)
///      \
///       X                       (cousin, merged by H2 on top of H)
/// ```
fn fixture() -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .git(["config", "advice.detachedHead", "false"])
        .write_executable(".git/seq.sh", SEQ_EDITOR)
        .write_executable(".git/msg.sh", MSG_EDITOR)
        .env("GIT_SEQUENCE_EDITOR", ".git/seq.sh")
        .env("GIT_EDITOR", ".git/msg.sh");
    let commit = |fx: Fixture, name: &str| {
        fx.tick()
            .write(format!("{name}.t"), format!("{name}\n"))
            .git(["add", "."])
            .git(["commit", "-q", "-m", name])
            .git(["tag", name])
    };
    fx = commit(fx, "A");
    fx = fx.git(["checkout", "-q", "-b", "first"]);
    fx = commit(fx, "B");
    fx = fx.git(["checkout", "-q", "main"]);
    fx = commit(fx, "C");
    fx = commit(fx, "D");
    fx = fx
        .tick()
        .git(["merge", "-q", "--no-ff", "-m", "E", "first"])
        .git(["tag", "E"])
        .git(["checkout", "-q", "-b", "second", "C"]);
    fx = commit(fx, "F");
    fx = commit(fx, "G");
    fx = fx
        .git(["checkout", "-q", "main"])
        .tick()
        .git(["merge", "-q", "--no-ff", "-m", "Merge branch 'second'", "G"])
        .git(["tag", "H"])
        .git(["checkout", "-q", "--detach", "A"]);
    fx = commit(fx, "X");
    fx.git(["checkout", "-q", "main"])
        .tick()
        .git([
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "Merge pull request #7 from a/x.y",
            "X",
        ])
        .tick()
}

const STATE_FILES: &[&str] = &[
    "REBASE_HEAD",
    "MERGE_MSG",
    "MERGE_HEAD",
    "ORIG_HEAD",
    "rebase-merge/git-rebase-todo",
    "rebase-merge/git-rebase-todo.backup",
    "rebase-merge/done",
    "rebase-merge/msgnum",
    "rebase-merge/end",
    "rebase-merge/message",
    "rebase-merge/author-script",
    "rebase-merge/stopped-sha",
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
        let alias = format!("alias.t=!{s}; echo; git log --graph --format='%s %T %an' --all");
        if let Err(m) = files(fx.compare(["-c", alias.as_str(), "t"])).try_run() {
            failed.push(m.to_string());
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

#[test]
fn generated_lists() {
    scripts(
        |fx| fx,
        &[
            "git rebase -i -r A",
            "git rebase -i -r C",
            "git rebase -i --rebase-merges=rebase-cousins C",
            "git rebase -i --rebase-merges=no-rebase-cousins C",
            "git rebase -i -r --root",
            "git rebase -i -r --onto A D",
            "git rebase -i -r HEAD",
            "git rebase -i -r --update-refs A",
            "git rebase -i -r --autosquash -x true A",
            "git -c rebase.instructionFormat=%an:%s rebase -i -r C",
            "git -c rebase.abbreviateCommands=true rebase -i -r C",
            "git -c rebase.maxLabelLength=3 rebase -i -r A",
            "git branch some/long.name G && git branch aa F && git rebase -i -r A",
        ],
    );
}

#[test]
fn replaying() {
    scripts(
        |fx| fx,
        &[
            "git rebase -r A",
            "git rebase -r C",
            "git rebase -r HEAD^",
            "git rebase -r --force-rebase A",
            "git rebase --rebase-merges=rebase-cousins HEAD^",
            "git rebase --rebase-merges=rebase-cousins C",
            "git -c rebase.rebaseMerges=true rebase --force-rebase C",
            "git -c rebase.rebaseMerges=rebase-cousins rebase HEAD^",
            "git -c rebase.rebaseMerges=true rebase --no-rebase-merges C",
            "git -c rebase.rebaseMerges=bogus rebase C",
            "git rebase --rebase-merges=bogus C",
            "git rebase -r --root --force-rebase",
            "git rebase -r --update-refs --force-rebase A",
            "git rebase -r -x 'git log -1 --format=%s' A",
            "git rebase -r --onto first C",
            "git checkout -q B && git rebase -r --onto X A",
        ],
    );
}

#[test]
fn octopus_and_edited_lists() {
    let octopus = "git checkout -q -b three A && echo t >t && git add t && git commit -q -m T && \
                   git checkout -q -b two A && echo u >u && git add u && git commit -q -m U && \
                   git checkout -q -b one A && echo v >v && git add v && git commit -q -m V && \
                   git merge -q -m Octo two three";
    let o1 = format!("{octopus} && git rebase -r A");
    let o2 = format!("{octopus} && git rebase -r --force-rebase A");
    let o3 = format!("{octopus} && git rebase -i -r A");
    scripts(
        |fx| fx,
        &[
            &o1,
            &o2,
            &o3,
            // `merge -c` rewords, also when the merge is reused.
            "printf 'merge -c E B\\nmerge -c H G\\n' >.git/new-todo && git rebase -i -r D",
            "printf 'reset onto\\nmerge -C H G # x\\n' >.git/new-todo && git rebase -i -r --force-rebase D",
            // A new merge gets a message to edit.
            "printf 'merge G\\n' >.git/new-todo && git rebase -i -r D",
            "printf 'merge B G\\n' >.git/new-todo && git rebase -i -r D",
            // Labels: bad names, missing labels, trees, the new root.
            "printf 'reset nope\\n' >.git/new-todo && git rebase -i -r D",
            "printf 'reset A^{tree}\\n' >.git/new-todo && git rebase -i -r D",
            "printf 'merge A^{tree}\\n' >.git/new-todo && git rebase -i -r D",
            "printf 'reset [new root]\\npick B\\nlabel b\\nreset [new root]\\npick X\\nmerge b\\n' >.git/new-todo && git rebase -i -r D",
            "printf 'merge HEAD^\\n' >.git/new-todo && git rebase -i -r HEAD",
            "printf 'break\\n' >>.git/new-todo; git rebase -i -r HEAD^ && git rev-parse --verify refs/rewritten/onto && git rebase --abort; git rev-parse --verify refs/rewritten/onto",
        ],
    );
}

#[test]
fn conflicts() {
    // `D` and `G` both change `C.t`: the recreated `H` conflicts.
    let prep = |fx: Fixture| {
        fx.git(["checkout", "-q", "-b", "other", "C"])
            .write("G.t", "other\n")
            .git(["add", "G.t"])
            .git(["commit", "-q", "-m", "other G"])
            .git(["checkout", "-q", "main"])
    };
    scripts(
        prep,
        &[
            "git rebase -r other",
            "git rebase -r other; git checkout --theirs G.t; git add G.t; git rebase --continue",
            "git rebase -r other; git rebase --skip",
            "git rebase -r other; git rebase --abort",
            "echo x >G.t; git rebase -r --onto other A",
            "printf 'merge -C H other\\n' >.git/new-todo && git rebase -i -r G",
        ],
    );
}
