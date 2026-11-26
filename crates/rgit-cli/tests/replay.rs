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
