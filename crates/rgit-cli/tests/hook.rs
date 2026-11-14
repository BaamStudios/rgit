// SPDX-License-Identifier: MIT
//! Hooks against the reference git (#76): `git hook run` / `git hook list`,
//! the commit hooks, `post-checkout` (switch, checkout, clone) and
//! `pre-push`. The hooks print their arguments and environment; that
//! output must reach standard error the same way.

use rgit_difftest::{Check, Fixture};

/// A hook that reports how it was called. `GIT_INDEX_FILE` is shown
/// relative to the git directory, with the process id of a temporary
/// index masked.
const REPORT: &str = r#"#!/bin/sh
idx=$(printf '%s' "$GIT_INDEX_FILE" | sed -e 's/next-index-[0-9]*/next-index-PID/' -e 's#^.*/\.git/#.git/#')
echo "$(basename "$0") [$*] pwd=$(pwd) idx=$idx editor=$GIT_EDITOR prefix=$GIT_PREFIX author=$GIT_AUTHOR_NAME <$GIT_AUTHOR_EMAIL> $GIT_AUTHOR_DATE"
"#;

const COMMIT_HOOKS: [&str; 4] = [
    "pre-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
];

const STATE: [Check; 7] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Refs,
    Check::Objects,
    Check::Index,
    Check::Worktree,
];

fn base() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a", "a\n")
        .write("b", "b\n")
        .git(["add", "a", "b"])
        .git(["commit", "-q", "-m", "one"])
        .tick()
}

fn with_hooks(mut fx: Fixture, names: &[&str], script: &str) -> Fixture {
    for n in names {
        fx = fx.write_executable(format!(".git/hooks/{n}"), script);
    }
    fx
}

fn check(mut fx: Fixture, args: &[&str]) {
    if fx.is_skipped() {
        return;
    }
    fx.compare(args)
        .checks(STATE)
        .git_file("COMMIT_EDITMSG")
        .run();
}

#[test]
fn commit_hooks_arguments_and_environment() {
    let cases: &[&[&str]] = &[
        &["commit", "-m", "x"],
        &["commit", "-a", "-m", "x"],
        &["commit", "-m", "x", "a"],
        &["commit", "-i", "-m", "x", "a"],
        &["commit", "--no-verify", "-m", "x"],
        &["commit", "-n", "--verify", "-m", "x"],
        &[
            "commit",
            "--author",
            "N A <n@a>",
            "--date",
            "2005-04-07T22:13:13",
            "-m",
            "x",
        ],
        &["commit", "--amend", "--no-edit"],
        &["commit", "--amend", "-m", "x"],
        &["commit", "-q", "--allow-empty", "-m", "x"],
        &["commit", "-m", "nothing staged"],
    ];
    for args in cases {
        let fx = with_hooks(base(), &COMMIT_HOOKS, REPORT)
            .write("a", "changed\n")
            .write("c", "c\n")
            .git(["add", "c"]);
        let fx = if args.last() == Some(&"nothing staged") {
            with_hooks(base(), &COMMIT_HOOKS, REPORT)
        } else {
            fx
        };
        check(fx, args);
    }
    // With an editor, and from a subdirectory (GIT_PREFIX).
    let mut fx = with_hooks(base(), &COMMIT_HOOKS, REPORT)
        .write("d/e", "e\n")
        .git(["add", "d/e"]);
    if !fx.is_skipped() {
        fx.compare(["commit"])
            .env("GIT_EDITOR", "f() { echo edited >\"$1\"; }; f")
            .in_dir("d")
            .checks(STATE)
            .git_file("COMMIT_EDITMSG")
            .run();
    }
}

#[test]
fn commit_hooks_failing_and_editing() {
    let fail = "#!/bin/sh\necho \"$(basename \"$0\") says no\"\nexit 3\n";
    for hook in ["pre-commit", "prepare-commit-msg", "commit-msg"] {
        let fx = with_hooks(base(), &[hook], fail)
            .write("c", "c\n")
            .git(["add", "c"]);
        check(fx, &["commit", "-m", "x"]);
        let fx = with_hooks(base(), &[hook], fail)
            .write("c", "c\n")
            .git(["add", "c"]);
        check(fx, &["commit", "--no-verify", "-m", "x"]);
    }
    // A failing post-commit hook does not change the outcome.
    let fx = with_hooks(base(), &["post-commit"], fail)
        .write("c", "c\n")
        .git(["add", "c"]);
    check(fx, &["commit", "-m", "x"]);
    // commit-msg rewrites the message, or empties it.
    let edit = "#!/bin/sh\necho 'new message' >\"$1\"\n";
    let fx = with_hooks(base(), &["commit-msg"], edit)
        .write("c", "c\n")
        .git(["add", "c"]);
    check(fx, &["commit", "-m", "x"]);
    let empty = "#!/bin/sh\n: >\"$1\"\n";
    let fx = with_hooks(base(), &["commit-msg"], empty)
        .write("c", "c\n")
        .git(["add", "c"]);
    check(fx, &["commit", "-m", "x"]);
    // prepare-commit-msg sees the message (cleaned up) and may extend it.
    let pcm = "#!/bin/sh\necho \"--- $*\"; cat \"$1\"; echo 'Signed: hook' >>\"$1\"\n";
    let fx = with_hooks(base(), &["prepare-commit-msg"], pcm)
        .write("c", "c\n")
        .git(["add", "c"]);
    check(fx, &["commit", "-m", "subject  ", "-m", "# not a comment"]);
}

#[test]
fn pre_commit_changes_the_index() {
    let add = "#!/bin/sh\necho added >added && git add added\n";
    for args in [
        &["commit", "-m", "x"][..],
        &["commit", "-a", "-m", "x"],
        &["commit", "-m", "x", "a"],
    ] {
        let fx = with_hooks(base(), &["pre-commit"], add)
            .write("a", "changed\n")
            .git(["add", "a"])
            .write("b", "changed\n");
        check(fx, args);
    }
}

#[test]
fn ignored_hook_advice() {
    for args in [
        &["commit", "-m", "x"][..],
        &["-c", "advice.ignoredHook=false", "commit", "-m", "x"],
    ] {
        let fx = base()
            .write(".git/hooks/pre-commit", "#!/bin/sh\nexit 1\n")
            .write("c", "c\n")
            .git(["add", "c"]);
        check(fx, args);
    }
}

#[test]
fn hook_run_and_list() {
    let mut fx = base()
        .write_executable(
            ".git/hooks/test-hook",
            "#!/bin/sh\necho \"hookdir $*\"; echo err >&2\n",
        )
        .write_executable(".git/hooks/pre-commit", "#!/bin/sh\nexit 5\n")
        .write("my-hooks/test-hook", "#!/bin/sh\necho custom\n")
        .write("input", "1\n2\n")
        .git(["config", "hook.one.event", "test-hook"])
        .git(["config", "hook.one.command", "echo one \"$@\"; cat"])
        .git(["config", "hook.two.event", "test-hook"])
        .git(["config", "hook.two.command", "echo two"])
        .git(["config", "hook.off.event", "test-hook"])
        .git(["config", "hook.off.command", "echo off"])
        .git(["config", "hook.off.enabled", "false"]);
    if fx.is_skipped() {
        return;
    }
    // The usage of `hook run` spans two lines; rgit aligns the second one
    // differently (rgit-options), so only the exit code is compared. The
    // order of output from hooks run in parallel depends on timing.
    for args in [
        &["hook"][..],
        &["hook", "run"],
        &[
            "hook",
            "run",
            "--allow-unknown-hook-name",
            "test-hook",
            "a",
            "b",
        ],
        &[
            "hook",
            "run",
            "--allow-unknown-hook-name",
            "-j2",
            "test-hook",
        ],
    ] {
        fx.compare(args).only_exit_code().run();
    }
    let cases: &[&[&str]] = &[
        &["hook", "list"],
        &["hook", "run", "prereceive"],
        &["hook", "list", "prereceive"],
        &["hook", "run", "--ignore-missing", "pre-receive"],
        &["hook", "list", "pre-receive"],
        &["hook", "run", "pre-commit"],
        &["hook", "run", "--allow-unknown-hook-name", "test-hook"],
        &[
            "hook",
            "run",
            "--allow-unknown-hook-name",
            "test-hook",
            "--",
            "a",
            "b c",
        ],
        &[
            "hook",
            "run",
            "--allow-unknown-hook-name",
            "--to-stdin=input",
            "test-hook",
        ],
        &[
            "hook",
            "run",
            "--allow-unknown-hook-name",
            "--to-stdin=nosuch",
            "test-hook",
        ],
        &[
            "hook",
            "run",
            "--allow-unknown-hook-name",
            "-j-2",
            "test-hook",
        ],
        &["hook", "run", "--allow-unknown-hook-name", "missing"],
        &[
            "hook",
            "run",
            "--allow-unknown-hook-name",
            "--ignore-missing",
            "missing",
        ],
        &["hook", "list", "--allow-unknown-hook-name", "test-hook"],
        &[
            "hook",
            "list",
            "--allow-unknown-hook-name",
            "--show-scope",
            "test-hook",
        ],
        &[
            "hook",
            "list",
            "--allow-unknown-hook-name",
            "-z",
            "test-hook",
        ],
        &[
            "-c",
            "hook.test-hook.enabled=false",
            "hook",
            "list",
            "--allow-unknown-hook-name",
            "test-hook",
        ],
        &[
            "-c",
            "hook.test-hook.enabled=false",
            "hook",
            "run",
            "--allow-unknown-hook-name",
            "test-hook",
        ],
        &[
            "-c",
            "core.hooksPath=my-hooks",
            "hook",
            "run",
            "--allow-unknown-hook-name",
            "test-hook",
        ],
        &[
            "-c",
            "hook.jobs=-3",
            "hook",
            "run",
            "--allow-unknown-hook-name",
            "--ignore-missing",
            "x",
        ],
        &[
            "-c",
            "hook.broken.event=x",
            "hook",
            "run",
            "--allow-unknown-hook-name",
            "test-hook",
        ],
        &[
            "-c",
            "hook.pre-push.event=x",
            "-c",
            "hook.pre-push.command=true",
            "hook",
            "run",
            "--allow-unknown-hook-name",
            "test-hook",
        ],
        &[
            "-c",
            "hook.a.event=x",
            "-c",
            "hook.a.command=exit 1",
            "-c",
            "hook.b.event=x",
            "-c",
            "hook.b.command=exit 2",
            "hook",
            "run",
            "--allow-unknown-hook-name",
            "x",
        ],
    ];
    for args in cases {
        fx.compare(*args).output_only().run();
    }
    // From a subdirectory: hooks run at the top, with GIT_PREFIX.
    let mut fx = fx
        .write("d/f", "f\n")
        .git(["config", "hook.where.event", "where"])
        .git(["config", "hook.where.command", "echo $(pwd) $GIT_PREFIX"]);
    fx.compare(["hook", "run", "--allow-unknown-hook-name", "where"])
        .in_dir("d")
        .output_only()
        .run();
}

#[test]
fn post_checkout() {
    let hook = "#!/bin/sh\necho \"post-checkout $*\"\n";
    let setup = || {
        with_hooks(base(), &["post-checkout"], hook)
            .git(["branch", "side"])
            .write("a", "two\n")
            .git(["commit", "-q", "-a", "-m", "two"])
            .tick()
    };
    let cases: &[&[&str]] = &[
        &["switch", "side"],
        &["switch", "-q", "main"],
        &["switch", "-c", "new"],
        &["switch", "--detach", "HEAD~1"],
        &["switch", "--orphan", "orphan"],
        &["checkout", "side"],
        &["checkout", "-b", "new"],
        &["checkout", "--orphan", "orphan"],
        &["checkout"],
        &["checkout", "side", "--", "a"],
        &["checkout", "--", "a"],
    ];
    for args in cases {
        let mut fx = setup();
        if fx.is_skipped() {
            return;
        }
        fx.compare(*args).checks(STATE).run();
    }
    // A failing hook.
    let mut fx =
        with_hooks(base(), &["post-checkout"], "#!/bin/sh\nexit 4\n").git(["branch", "side"]);
    if !fx.is_skipped() {
        fx.compare(["switch", "side"]).checks(STATE).run();
        fx.compare(["checkout", "main", "--", "a"])
            .checks(STATE)
            .run();
    }
}

#[test]
fn clone_runs_post_checkout() {
    let mut fx = base()
        .write_executable(
            "tpl/hooks/post-checkout",
            "#!/bin/sh\necho \"post-checkout $* in $(basename \"$(pwd)\") $(basename \"$GIT_DIR\")\"\n",
        )
        .write_executable("bad/hooks/post-checkout", "#!/bin/sh\nexit 3\n");
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["clone", "-q", "--template=tpl", ".", "c1"][..],
        &["clone", "-q", "--no-checkout", "--template=tpl", ".", "c2"],
        &["clone", "-q", "--template=bad", ".", "c3"],
    ] {
        fx.compare(args)
            .checks([Check::ExitCode, Check::Stdout, Check::Stderr])
            .run();
    }
}

#[test]
fn pre_push() {
    let mut fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"));
    if fx.is_skipped() {
        return;
    }
    let rp = format!(
        "env -u GIT_EXEC_PATH '{}' receive-pack",
        fx.reference_git().display()
    );
    fx = fx
        .git(["init", "-q", "--bare", "srv.git"])
        .git(["init", "-q", "--bare", "other.git"])
        .git(["init", "-q", "-b", "main"])
        .write("f", "one\n")
        .git(["add", "f"])
        .git(["commit", "-q", "-m", "one"])
        .tick()
        .git(["remote", "add", "origin", "srv.git"])
        .git(["config", "remote.origin.receivepack", &rp])
        .write_executable(
            ".git/hooks/pre-push",
            "#!/bin/sh\necho \"pre-push [$*]\"; while read l; do echo \"  $l\"; done; echo err >&2; exit ${PRE_PUSH_EXIT:-0}\n",
        );
    let url_push = format!("--receive-pack={rp}");
    let cases: &[&[&str]] = &[
        &["push", "origin", "main"],
        &["push", "-n", "origin", "main:dry"],
        &["push", "--no-verify", "origin", "main:nv"],
        &["push", &url_push, "other.git", "HEAD:url"],
        &["push", "origin", ":nothere", "HEAD~0:refs/heads/sha"],
    ];
    for args in cases {
        fx.compare(*args)
            .checks([Check::ExitCode, Check::Stdout, Check::Stderr, Check::Refs])
            .run();
    }
    let mut fx = fx
        .git(["push", "-q", "origin", "main"])
        .env("PRE_PUSH_EXIT", "1");
    fx.compare(["push", "origin", "main:refused"])
        .checks([Check::ExitCode, Check::Stdout, Check::Stderr, Check::Refs])
        .run();
    fx.compare(["push", "origin", ":main"])
        .checks([Check::ExitCode, Check::Stdout, Check::Stderr, Check::Refs])
        .run();
}
