// SPDX-License-Identifier: MIT
//! Tests of the harness itself: the reference git is compared with itself
//! (must always match, which also proves the fixture is deterministic) and
//! with deliberately wrong wrappers (must produce readable diffs).

use rgit_difftest::{Check, Fixture, Normalizer};

/// A fixture whose "rgit" is the reference git itself.
fn git_vs_git() -> Fixture {
    let fx = Fixture::new();
    if fx.is_skipped() {
        return fx;
    }
    let reference = fx.reference_git().to_path_buf();
    fx.rgit(reference)
}

/// A fixture whose "rgit" runs the reference git and then post-processes
/// the result with `script` (shell code, `$REAL` is the reference git).
#[cfg(unix)]
fn git_vs_wrapper(script: &str) -> Fixture {
    let fx = Fixture::new();
    if fx.is_skipped() {
        return fx;
    }
    let reference = fx.reference_git().to_path_buf();
    let dir = fx
        .home_dir()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("wrapper");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("fake-rgit");
    rgit_difftest::write_executable(
        &path,
        format!("#!/bin/sh\nREAL='{}'\n{script}\n", reference.display()),
    );
    fx.rgit(path)
}

#[test]
fn identical_binaries_match_on_all_checks() {
    git_vs_git()
        .same(["init", "-q"])
        .write("a.txt", "hello\n")
        .write("dir/b.txt", "world\n")
        .same(["add", "."])
        .same(["commit", "-q", "-m", "first"])
        .tick()
        .write("a.txt", "changed\n")
        .same(["status", "--porcelain=v2", "--branch"])
        .same(["commit", "-q", "-a", "-m", "second"])
        .same(["log", "--format=%H %an <%ae> %ad %s", "--date=raw"])
        .same(["branch", "topic"])
        .same(["tag", "-a", "-m", "v1", "v1"])
        .same(["checkout", "-q", "topic"])
        .same(["unknown-command-xyz"]);
}

#[test]
fn deterministic_identity_and_time() {
    let fx = Fixture::new()
        .git(["init", "-q"])
        .write("f", "x\n")
        .git(["add", "f"])
        .git(["commit", "-q", "-m", "m"]);
    if fx.is_skipped() {
        return;
    }
    let out = fx.git_stdout(["log", "-1", "--format=%an <%ae> %at|%cn <%ce> %ct|%D"]);
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "Ada Author <ada@example.com> 1700000000|Cy Committer <cy@example.com> 1700000000|HEAD -> main\n"
    );
    // Same content, same time: the commit id is stable across runs and machines.
    let id = fx.git_stdout(["rev-parse", "HEAD"]);
    let fx2 = Fixture::new()
        .git(["init", "-q"])
        .write("f", "x\n")
        .git(["add", "f"])
        .git(["commit", "-q", "-m", "m"]);
    assert_eq!(fx2.git_stdout(["rev-parse", "HEAD"]), id);
}

#[test]
fn stdin_and_subdirectory() {
    let mut fx = git_vs_git().same(["init", "-q"]).write("sub/x", "1\n");
    fx.compare(["hash-object", "-w", "--stdin"])
        .stdin("data\n")
        .run();
    fx.compare(["rev-parse", "--show-prefix"])
        .in_dir("sub")
        .run();
}

#[cfg(unix)]
#[test]
fn wrong_stdout_produces_unified_diff() {
    let mut fx = git_vs_wrapper(r#""$REAL" "$@" | sed 's/^/X/'"#).git(["init", "-q"]);
    if fx.is_skipped() {
        return;
    }
    let err = fx
        .compare(["symbolic-ref", "--short", "HEAD"])
        .output_only()
        .try_run()
        .unwrap_err();
    let report = err.report();
    assert!(
        report.contains("### stdout differs\n--- git (stdout)\n+++ rgit (stdout)\n@@ -1,1 +1,1 @@\n-main\n+Xmain\n"),
        "{report}"
    );
    assert!(!report.contains("### exit code"), "{report}");
}

#[cfg(unix)]
#[test]
fn wrong_state_is_reported_per_aspect() {
    // The wrapper commits correctly, then leaves an extra file and a stray ref behind.
    let mut fx = git_vs_wrapper(
        r#""$REAL" "$@"; rc=$?; echo junk > extra.txt; "$REAL" update-ref refs/heads/stray HEAD; exit $rc"#,
    )
    .git(["init", "-q"])
    .write("a", "a\n")
    .git(["add", "a"]);
    if fx.is_skipped() {
        return;
    }
    let report = fx
        .compare(["commit", "-q", "-m", "c"])
        .try_run()
        .unwrap_err()
        .to_string();
    assert!(report.contains("### refs differs"), "{report}");
    assert!(report.contains("+refs/heads/stray "), "{report}");
    assert!(report.contains("### worktree differs"), "{report}");
    assert!(report.contains("+== 100644 extra.txt\n+junk\n"), "{report}");
    assert!(!report.contains("### stdout"), "{report}");
    // The fixture continues from the reference result: no stray branch.
    let refs = fx.git_stdout(["for-each-ref", "--format=%(refname)"]);
    assert_eq!(String::from_utf8(refs).unwrap(), "refs/heads/main\n");
}

#[cfg(unix)]
#[test]
fn wrong_exit_code_shows_output_for_context() {
    let mut fx = git_vs_wrapper(r#""$REAL" "$@"; exit 3"#);
    if fx.is_skipped() {
        return;
    }
    let report = fx
        .compare(["--version"])
        .only_exit_code()
        .try_run()
        .unwrap_err()
        .to_string();
    assert!(report.contains("-exit code 0\n+exit code 3\n"), "{report}");
    assert!(report.contains("### rgit output (for context)"), "{report}");
}

#[cfg(unix)]
#[test]
#[should_panic(expected = "### stdout differs")]
fn run_panics_on_mismatch() {
    let mut fx = git_vs_wrapper(r#"echo different"#);
    if fx.is_skipped() {
        panic!("### stdout differs (skipped: no reference git)");
    }
    fx.compare(["--version"]).output_only().run();
}

#[cfg(unix)]
#[test]
fn normalizers_accept_known_differences() {
    let mut fx = git_vs_wrapper(r#"echo "git version 9.9.9.rgit.0.1.0""#);
    fx.compare(["--version"])
        .output_only()
        .normalize_stdout(Normalizer::version_line())
        .run();
}

#[test]
fn skipping_checks() {
    let mut fx = git_vs_git().same(["init", "-q"]);
    fx.compare(["status"])
        .checks([Check::ExitCode, Check::Stdout])
        .run();
    fx.compare(["status"]).skip(Check::Index).run();
}
