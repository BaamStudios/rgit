// SPDX-License-Identifier: MIT
//! Differential tests of rerere (#156): `rgit rerere` and its subcommands,
//! the passes of merge, cherry-pick, rebase and commit, and the `rr-cache`
//! files and `MERGE_RR` they write, against the reference git. A
//! resolution recorded by one tool must be replayed by the other.

use std::path::Path;
use std::process::Command;

use rgit_difftest::{Check, Comparison, Fixture};

/// The id of the conflict "M" / "X" (one hunk, sides sorted).
const ID: &str = "7d6565ce286284f27e5f9357c14f612faa07c542";

fn seq(lines: &[&str]) -> String {
    lines.iter().map(|l| format!("{l}\n")).collect()
}

/// `f1` and `f2` conflict the same way (line 4: `M` on main, `X` on x),
/// `f3` differently; `f4` merges cleanly. rerere is on.
fn fixture() -> Fixture {
    let base = seq(&["1", "2", "3", "4", "5", "6", "7", "8"]);
    let x = seq(&["1", "2", "3", "X", "5", "6", "7", "8"]);
    let m = seq(&["1", "2", "3", "M", "5", "6", "7", "8"]);
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .git(["config", "rerere.enabled", "true"])
        .git(["config", "advice.detachedHead", "false"])
        .write("f1", &base)
        .write("f2", &base)
        .write("f3", seq(&["a", "b", "c", "d"]))
        .write("f4", seq(&["p", "q", "r"]))
        .git(["add", "."])
        .git(["commit", "-q", "-m", "base"])
        .git(["checkout", "-q", "-b", "x"])
        .tick()
        .write("f1", &x)
        .write("f2", &x)
        .write("f3", seq(&["a", "B", "c", "d"]))
        .write("f4", seq(&["p", "q", "R"]))
        .git(["commit", "-q", "-a", "-m", "x"])
        .git(["checkout", "-q", "main"])
        .tick()
        .write("f1", &m)
        .write("f2", &m)
        .write("f3", seq(&["a", "b2", "c", "d"]))
        .git(["commit", "-q", "-a", "-m", "m"])
}

/// The rr-cache files of the conflict of `f1`/`f2` and `MERGE_RR`.
fn rr_files(c: Comparison<'_>) -> Comparison<'_> {
    let mut c = c.git_file("MERGE_RR");
    for v in ["", ".1", ".2"] {
        for image in ["preimage", "postimage"] {
            c = c.git_file(format!("rr-cache/{ID}/{image}{v}"));
        }
    }
    c
}

fn resolved() -> String {
    seq(&["1", "2", "3", "M", "X", "5", "6", "7", "8"])
}

#[test]
fn merge_records_preimages() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    rr_files(fx.compare(["merge", "x"])).run();
    // The same merge with rerere off records nothing.
    let mut fx = fixture().git(["config", "rerere.enabled", "false"]);
    rr_files(fx.compare(["merge", "x"])).run();
}

#[test]
fn subcommands_while_conflicted() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let _ = fx.git_output(["merge", "x"]);
    let mut fx = fx.write("f1", resolved());
    for args in [
        &["rerere", "status"][..],
        &["rerere", "remaining"],
        &["rerere", "diff"],
        &["rerere", "bogus"],
        &["rerere", "-h"],
        &["rerere", "--no-no-rerere-autoupdate"],
    ] {
        rr_files(fx.compare(args)).run();
    }
    // Records f1's resolution and replays it on f2.
    rr_files(fx.compare(["rerere"])).run();
    rr_files(fx.compare(["rerere", "--rerere-autoupdate"])).run();
    rr_files(fx.compare(["rerere", "clear"])).run();
    rr_files(fx.compare(["rerere", "forget", "f1"])).run();
    rr_files(fx.compare(["rerere", "forget"])).run();
}

#[test]
fn resolution_of_git_is_replayed() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let fx = fx;
    let _ = fx.git_output(["merge", "x"]);
    let mut fx = fx
        .write("f1", resolved())
        .write("f2", resolved())
        .write("f3", seq(&["a", "B", "b2", "c", "d"]))
        .git(["commit", "-q", "-a", "-m", "resolved"])
        .git(["reset", "-q", "--hard", "HEAD^"]);
    for (config, args) in [
        ("false", &["merge", "x"][..]),
        ("false", &["merge", "--rerere-autoupdate", "x"]),
        ("true", &["merge", "x"]),
        ("true", &["merge", "--no-rerere-autoupdate", "x"]),
        ("false", &["cherry-pick", "--rerere-autoupdate", "x"]),
        ("true", &["cherry-pick", "--no-rerere-autoupdate", "x"]),
        ("true", &["cherry-pick", "x"]),
    ] {
        fx = fx
            .git(["reset", "-q", "--hard"])
            .git(["config", "rerere.autoUpdate", config]);
        rr_files(fx.compare(args)).run();
    }
    let mut fx = fx
        .git(["reset", "-q", "--hard"])
        .git(["checkout", "-q", "x"]);
    rr_files(fx.compare(["rebase", "--rerere-autoupdate", "main"])).run();
    let mut fx = fx.git(["rebase", "--abort"]);
    rr_files(fx.compare(["rebase", "--apply", "main"])).run();
}

#[test]
fn commit_and_continue_record_resolutions() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let _ = fx.git_output(["merge", "x"]);
    let mut fx = fx
        .write("f1", resolved())
        .write("f2", resolved())
        .write("f3", seq(&["a", "B", "c", "d"]));
    rr_files(fx.compare(["commit", "-q", "-a", "-m", "merged"])).run();

    let fx = fixture().git(["checkout", "-q", "x"]);
    let _ = fx.git_output(["rebase", "main"]);
    let mut fx = fx
        .write("f1", resolved())
        .write("f2", resolved())
        .write("f3", seq(&["a", "B", "c", "d"]))
        .git(["add", "."]);
    rr_files(fx.compare(["rebase", "--continue"])).run();
    rr_files(fx.compare(["rebase", "--skip"])).run();
    rr_files(fx.compare(["rebase", "--abort"])).run();
}

/// Runs rgit in the fixture's work directory with its isolated home.
fn rgit(fx: &Fixture, args: &[&str]) -> std::process::Output {
    let home = fx.home_dir();
    Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(args)
        .current_dir(fx.work_dir())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .output()
        .expect("rgit runs")
}

#[test]
fn resolution_of_rgit_is_replayed_by_git() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let out = rgit(&fx, &["merge", "x"]);
    assert_eq!(out.status.code(), Some(1));
    let work = fx.work_dir();
    std::fs::write(work.join("f1"), resolved()).unwrap();
    let out = rgit(&fx, &["rerere"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        err,
        "Recorded resolution for 'f1'.\nResolved 'f2' using previous resolution.\n\
         Recorded preimage for 'f3'\n"
    );
    let out = rgit(&fx, &["reset", "-q", "--hard"]);
    assert!(out.status.success());
    let out = fx.git_output(["merge", "x"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("Resolved 'f1' using previous resolution."),
        "{err}"
    );
    assert!(
        err.contains("Resolved 'f2' using previous resolution."),
        "{err}"
    );
    assert_eq!(
        std::fs::read(work.join("f2")).unwrap(),
        resolved().as_bytes()
    );
}

fn age(path: &Path, days: u64) {
    let t = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86400);
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_modified(t).unwrap();
}

#[test]
fn gc_expiry() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let _ = fx.git_output(["merge", "x"]);
    let mut fx = fx.write("f1", resolved());
    let _ = fx.git_output(["rerere"]);
    // f1's resolution (variant 0) and f3's unresolved preimage.
    let rr = fx.work_dir().join(".git/rr-cache");
    for e in std::fs::read_dir(&rr).unwrap().flatten() {
        for f in std::fs::read_dir(e.path()).unwrap().flatten() {
            age(&f.path(), 20);
        }
    }
    let ids: Vec<String> = std::fs::read_dir(&rr)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    for args in [
        &["rerere", "gc"][..],
        &["-c", "gc.rerereResolved=5", "rerere", "gc"],
        &["-c", "gc.rerereUnresolved=5.days.ago", "rerere", "gc"],
        &[
            "-c",
            "gc.rerereResolved=now",
            "-c",
            "gc.rerereUnresolved=never",
            "rerere",
            "gc",
        ],
    ] {
        let mut c = rr_files(fx.compare(args));
        for id in &ids {
            c = c.git_file(format!("rr-cache/{id}/preimage"));
        }
        c.run();
    }
}

#[test]
fn outside_repository_help() {
    let mut fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"));
    if fx.is_skipped() {
        return;
    }
    fx.compare(["rerere", "-h"]).checks(Check::OUTPUT).run();
}

/// `forget` on paths already resolved and added: the conflict comes back
/// from the resolve-undo records of the index.
#[test]
fn forget_resolved_paths() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let _ = fx.git_output(["merge", "x"]);
    let fx = fx
        .write("f1", resolved())
        .write("f2", resolved())
        .git(["rerere"])
        .git(["reset", "-q", "--hard"]);
    let _ = fx.git_output(["merge", "x"]);
    let mut fx = fx.git(["add", "f1", "f2"]);
    rr_files(fx.compare(["rerere", "forget", "f1"])).run();
    rr_files(fx.compare(["rerere", "forget", "f2", "f3"])).run();

    // The same with the resolve-undo records written by rgit add.
    let fx = fixture();
    let _ = fx.git_output(["merge", "x"]);
    let fx = fx
        .write("f1", resolved())
        .git(["rerere"])
        .git(["reset", "-q", "--hard"]);
    let _ = fx.git_output(["merge", "x"]);
    assert!(rgit(&fx, &["add", "f1"]).status.success());
    let out = rgit(&fx, &["rerere", "forget", "f1"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "Updated preimage for 'f1'\nForgot resolution for 'f1'\n"
    );
    let rr = std::fs::read(fx.work_dir().join(".git/MERGE_RR")).unwrap();
    // f1 is back in MERGE_RR (next to the still unresolved f3).
    assert!(rr.starts_with(format!("{ID}\tf1\0").as_bytes()));
}
