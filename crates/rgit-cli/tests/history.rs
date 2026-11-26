// SPDX-License-Identifier: MIT
//! Differential tests of `git history` (#142) against the reference git:
//! usage and errors, the printed `update` commands (new commit ids
//! included), refs, reflogs, objects, index and work tree after `reword`,
//! `drop`, `fixup` and `split`.

use rgit_difftest::{Check, Fixture};

/// A fresh fixture, or `None` when there is no reference git or it has no
/// `git history` (older versions have none).
fn fixture() -> Option<Fixture> {
    let fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"));
    if fx.is_skipped() {
        return None;
    }
    if fx.git_output(["history", "-h"]).status.code() != Some(0) {
        eprintln!("skipping: the reference git has no `git history`");
        return None;
    }
    Some(fx.git(["init", "-q", "-b", "main"]))
}

fn commit(fx: Fixture, name: &str) -> Fixture {
    fx.write(format!("{name}.t"), format!("{name}\n"))
        .git(["add", "."])
        .tick()
        .git(["commit", "-q", "-m", name])
        .git(["tag", name])
}

/// A, B, then `zz`: Z1, Z2; `aa`: A1, A2 (from B); `mm`: M1 (from Z1);
/// `main`: C; `atB` and `atB2` at B; an editor script `ed.sh`.
fn graph() -> Option<Fixture> {
    let mut fx = fixture()?;
    fx = commit(fx, "A");
    fx = commit(fx, "B");
    fx = fx.git(["switch", "-q", "-c", "zz"]);
    fx = commit(fx, "Z1");
    fx = commit(fx, "Z2");
    fx = fx.git(["switch", "-q", "-c", "aa", "B"]);
    fx = commit(fx, "A1");
    fx = commit(fx, "A2");
    fx = fx.git(["switch", "-q", "-c", "mm", "Z1"]);
    fx = commit(fx, "M1");
    fx = fx.git(["switch", "-q", "main"]);
    fx = commit(fx, "C");
    fx = fx.git(["branch", "atB", "B"]).git(["branch", "atB2", "B"]);
    fx = fx.write_executable(
        ".git/ed.sh",
        "#!/bin/sh\nprintf 'reworded\\n\\nbody\\n' >\"$1\"\n",
    );
    Some(fx.tick())
}

fn editor(fx: &Fixture) -> String {
    fx.work_dir().join(".git/ed.sh").display().to_string()
}

#[test]
fn history_usage_and_errors() {
    let Some(mut fx) = graph() else {
        return;
    };
    for args in [
        &["history"][..],
        &["history", "-h"],
        &["history", "garbage"],
        &["history", "-x"],
        &["history", "--dry-run"],
        &["history", "reword", "-h"],
        &["history", "split", "-h"],
        &["history", "fixup", "-h"],
        &["history", "drop", "-h"],
        &["history", "reword"],
        &["history", "drop", "HEAD", "HEAD"],
        &["history", "split"],
        &["history", "fixup", "nope"],
        &["history", "reword", "--update-refs=foo", "HEAD"],
        &["history", "reword", "--update-refs"],
        &["history", "drop", "--empty=bogus", "HEAD"],
        &["history", "reword", "--empty=keep", "HEAD"],
        &["history", "drop", "HEAD:A.t"],
        &["history", "drop", "HEAD~..HEAD"],
        &["history", "drop", "A"],
        &["history", "fixup", "HEAD"],
        &["history", "reword", "--update-refs=head", "Z1"],
    ] {
        fx.compare(args).env("GIT_EDITOR", "false").run();
    }
}

#[test]
fn history_dry_run_order() {
    let Some(mut fx) = graph() else {
        return;
    };
    let ed = editor(&fx);
    for args in [
        &["history", "reword", "--dry-run", "B"][..],
        &["history", "reword", "--dry-run", "--update-refs=head", "B"],
        &["history", "reword", "--dry-run", "HEAD"],
        &[
            "history",
            "reword",
            "--dry-run",
            "--update-refs=head",
            "HEAD",
        ],
        &["history", "drop", "--dry-run", "B"],
        &["history", "drop", "--dry-run", "Z1"],
        &["history", "drop", "--dry-run", "--update-refs=head", "HEAD"],
    ] {
        fx.compare(args).env("GIT_EDITOR", &ed).run();
    }
    fx = fx.git(["switch", "-q", "--detach", "C"]);
    for args in [
        &["history", "reword", "--dry-run", "B"][..],
        &[
            "history",
            "reword",
            "--dry-run",
            "--update-refs=head",
            "HEAD",
        ],
        &["history", "drop", "--dry-run", "HEAD"],
    ] {
        fx.compare(args).env("GIT_EDITOR", &ed).run();
    }
}

#[test]
fn history_reword_and_drop() {
    let Some(mut fx) = graph() else {
        return;
    };
    let ed = editor(&fx);
    fx.compare(["history", "reword", "B"])
        .env("GIT_EDITOR", &ed)
        .git_file("COMMIT_EDITMSG")
        .run();
    fx = fx
        .git(["-c", &format!("core.editor={ed}"), "history", "reword", "B"])
        .tick();
    fx.compare(["history", "reword", "--update-refs=head", "HEAD~"])
        .env("GIT_EDITOR", &ed)
        .run();
    fx.compare(["history", "drop", "B"]).run();
    // Local changes that the drop would overwrite.
    fx = fx.write("C.t", "dirty\n");
    fx.compare(["history", "drop", "HEAD"]).run();
    fx = fx.git(["checkout", "-q", "C.t"]).write("A.t", "dirty\n");
    fx.compare(["history", "drop", "HEAD"]).run();
}

#[test]
fn history_drop_empty() {
    let Some(mut fx) = fixture() else {
        return;
    };
    fx = fx
        .write("file", "C1\n")
        .git(["add", "file"])
        .tick()
        .git(["commit", "-q", "-m", "base"]);
    fx = fx
        .write("file", "C2\n")
        .git(["add", "file"])
        .tick()
        .git(["commit", "-q", "-m", "drop-me"])
        .git(["tag", "drop-me"]);
    fx = commit(fx, "middle");
    fx = fx.write("file", "C1\n").git(["add", "file"]).tick().git([
        "commit",
        "-q",
        "-m",
        "revert-drop-me",
    ]);
    fx = fx
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "empty"]);
    fx = commit(fx, "after").tick();
    for empty in ["--empty=drop", "--empty=keep", "--empty=abort"] {
        fx.compare(["history", "drop", empty, "drop-me"]).run();
    }
    // A conflict while replaying.
    fx = fx
        .write("file", "C3\n")
        .git(["add", "file"])
        .tick()
        .git(["commit", "-q", "-m", "conflict"]);
    fx.compare(["history", "drop", "HEAD~"]).run();
}

#[test]
fn history_fixup() {
    let Some(mut fx) = graph() else {
        return;
    };
    let ed = editor(&fx);
    fx = fx
        .write("A.t", "A\nfix\n")
        .write("N.t", "new\n")
        .git(["add", "A.t", "N.t"])
        .write("A.t", "A\nfix\nunstaged\n");
    fx.compare(["history", "fixup", "--dry-run", "B"]).run();
    fx.compare(["history", "fixup", "--reedit-message", "B"])
        .env("GIT_EDITOR", &ed)
        .git_file("COMMIT_EDITMSG")
        .run();
    // Nothing staged any more.
    fx.compare(["history", "fixup", "B"]).run();
    fx = fx.tick();
    // Conflicts with the staged changes.
    fx = fx.write("C.t", "conflict\n").git(["add", "C.t"]);
    fx.compare(["history", "fixup", "A"]).run();
    // The target becomes empty.
    fx = fx.git(["reset", "-q", "--hard"]).git(["rm", "-q", "C.t"]);
    for empty in ["--empty=drop", "--empty=keep", "--empty=abort"] {
        fx.compare(["history", "fixup", empty, "HEAD"]).run();
    }
    fx = fx.git(["reset", "-q", "--hard"]).git(["rm", "-q", "A.t"]);
    fx.compare(["history", "fixup", "A"]).run();
}

#[test]
fn history_split() {
    let Some(mut fx) = fixture() else {
        return;
    };
    fx = commit(fx, "initial");
    fx = fx
        .write("f", "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\n")
        .write("bar", "")
        .write("foo", "")
        .git(["add", "."])
        .tick()
        .git(["commit", "-q", "-m", "split-me"]);
    fx = fx
        .write("f", "a\nB\nc\nd\nE\nf\ng\nh\ni\nj\nk\nL\n")
        .git(["commit", "-q", "-a", "-m", "mod"])
        .git(["branch", "side", "HEAD~"])
        .tick();
    fx = fx.write_executable(
        ".git/ed.sh",
        "#!/bin/sh\ncat \"$1\" >>.git/MESSAGES\necho \"msg $(wc -l <.git/MESSAGES)\" >\"$1\"\n",
    );
    let ed = editor(&fx);
    for (args, input) in [
        (&["history", "split", "HEAD"][..], "s\ny\nn\nj\n?\ny\n"),
        (&["history", "split", "--dry-run", "HEAD~"], "y\nn\n"),
        (&["history", "split", "HEAD~", "--", "f", "foo"], "y\nn\n"),
        (
            &["history", "split", "HEAD~"],
            "g\n2\nk\nJ\nK\np\ne\nx\nyy\nn\ny\nn\n",
        ),
        (&["history", "split", "HEAD", "--", "nope"], ""),
        (&["history", "split", "HEAD"], "a\n"),
        (&["history", "split", "HEAD"], "q\n"),
        (
            &["history", "split", "--update-refs=head", "HEAD"],
            "y\nn\n",
        ),
    ] {
        fx.compare(args)
            .env("GIT_EDITOR", &ed)
            .stdin(input)
            .git_file("COMMIT_EDITMSG")
            .run();
    }
    fx.compare(["history", "split", "HEAD"])
        .env("GIT_EDITOR", "false")
        .stdin("y\nn\n")
        .checks(Check::OUTPUT)
        .run();
}
