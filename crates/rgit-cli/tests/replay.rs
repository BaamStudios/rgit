// SPDX-License-Identifier: MIT
//! Differential tests of `git replay` (#142) against the reference git:
//! the printed `update` commands (new commit ids included) and the refs
//! after an atomic update.

use rgit_difftest::{Check, Fixture};

fn history() -> Fixture {
    let mut fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"));
    if fx.is_skipped() {
        return fx;
    }
    fx = fx.git(["init", "-q", "-b", "main"]);
    let commit = |fx: Fixture, name: &str| {
        fx.write(format!("{name}.t"), format!("{name}\n"))
            .git(["add", "."])
            .tick()
            .git(["commit", "-q", "-m", name])
    };
    fx = commit(fx, "A");
    fx = commit(fx, "B");
    fx = fx.git(["switch", "-q", "-c", "topic1"]);
    fx = commit(fx, "C");
    fx = fx.git(["switch", "-q", "-c", "topic2"]);
    fx = commit(fx, "D");
    fx = commit(fx, "E");
    fx = fx.git(["switch", "-q", "topic1"]);
    fx = commit(fx, "F");
    fx = fx.git(["switch", "-q", "-c", "topic4", "main"]);
    fx = commit(fx, "I");
    fx = commit(fx, "J");
    fx = fx.git(["switch", "-q", "main"]);
    fx = commit(fx, "L");
    fx = fx.git(["switch", "-q", "-c", "conflict", "main~1"]);
    fx = fx.write("C.t", "conflict\n").git(["add", "."]).tick().git([
        "commit",
        "-q",
        "-m",
        "C.conflict",
    ]);
    fx.git(["switch", "-q", "main"])
}

#[test]
fn replay_print() {
    let mut fx = history();
    if fx.is_skipped() {
        return;
    }
    for args in [
        &[
            "replay",
            "--ref-action=print",
            "--onto",
            "main",
            "topic1..topic2",
        ][..],
        &[
            "replay",
            "--ref-action=print",
            "--onto",
            "main",
            "^main",
            "topic2",
            "topic4",
        ],
        &[
            "replay",
            "--ref-action=print",
            "--contained",
            "--onto",
            "main",
            "main..topic2",
        ],
        &[
            "replay",
            "--ref-action=print",
            "--advance",
            "main",
            "topic1..topic2",
        ],
        &[
            "replay",
            "--ref-action=print",
            "--revert",
            "topic4",
            "topic4~2..topic4",
        ],
        &[
            "replay",
            "--ref-action=print",
            "--onto=main",
            "--ref=refs/heads/new",
            "topic1..topic2",
        ],
        &["replay", "--onto", "topic1", "main~1..conflict"],
        &["replay", "topic1..topic2"],
        &["replay", "--advance", "main", "main", "topic1"],
        &[
            "replay",
            "--onto",
            "main",
            "--advance",
            "main",
            "topic1..topic2",
        ],
        &["replay", "--onto", "main", "--ref=main", "topic1..topic2"],
        &[
            "replay",
            "--ref-action=bogus",
            "--onto",
            "main",
            "topic1..topic2",
        ],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
}

#[test]
fn replay_updates_refs() {
    let mut fx = history();
    if fx.is_skipped() {
        return;
    }
    fx.compare(["replay", "--onto", "main", "topic1..topic2"])
        .run();
    fx.compare(["replay", "--revert", "topic4", "topic4~1..topic4"])
        .run();
}

/// A commit and its revert replayed across a rename (#744, t6429): the
/// rename found for the first pick carries over to the second, so rename
/// detection runs once (one `diffcore_rename` trace2 region, as in git).
#[test]
fn replay_remembers_renames() {
    let mut fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"));
    if fx.is_skipped() {
        return;
    }
    let numbers = |a: u32, b: u32| (a..=b).map(|i| format!("{i}\n")).collect::<String>();
    fx = fx
        .git(["init", "-q", "-b", "main"])
        .write("numbers", numbers(11, 30))
        .git(["add", "."])
        .tick()
        .git(["commit", "-q", "-m", "orig"])
        .git(["branch", "topic"])
        .write("numbers", numbers(1, 30))
        .git(["add", "."])
        .git(["mv", "numbers", "sequence"])
        .tick()
        .git(["commit", "-q", "-m", "renamed"])
        .git(["switch", "-q", "topic"])
        .write("numbers", numbers(11, 13))
        .git(["add", "."])
        .tick()
        .git(["commit", "-q", "-m", "A"])
        .tick()
        .git(["revert", "--no-edit", "HEAD"])
        .git(["switch", "-q", "main"]);
    let args = [
        "replay",
        "--ref-action=print",
        "--onto",
        "HEAD",
        "main~1..topic",
    ];
    fx.compare(args).checks(Check::OUTPUT).run();

    let trace = fx.work_dir().join("../perf");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(args)
        .current_dir(fx.work_dir())
        .env("HOME", fx.work_dir())
        .env("XDG_CONFIG_HOME", fx.work_dir())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_TRACE2_PERF", &trace)
        .output()
        .expect("run rgit");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(&trace).unwrap_or_default();
    let calls = text
        .lines()
        .filter(|l| l.contains("region_enter") && l.contains("diffcore_rename"))
        .count();
    assert_eq!(calls, 1, "{text}");
}
