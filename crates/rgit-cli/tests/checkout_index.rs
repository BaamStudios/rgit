// SPDX-License-Identifier: MIT
//! Differential tests of `rgit checkout-index` against the reference git.

use rgit_difftest::{Check, Fixture};

const ALL: [Check; 5] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Index,
    Check::Worktree,
];

fn fixture() -> Fixture {
    commit(files())
}

/// The fixture's files, not yet added.
fn files() -> Fixture {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a", "a\n")
        .write("b", "b\n")
        .write("d/x", "x\n")
        .write_executable("sc", "s\n");
    #[cfg(unix)]
    let fx = fx.symlink("a", "l");
    fx
}

fn commit(fx: Fixture) -> Fixture {
    fx.git(["add", "."])
        .git(["commit", "-q", "-m", "init"])
        .tick()
}

/// [`fixture`] with the index entry of `b` stamped long before the index
/// was written, then `b` removed: a freshly checked out `b` never matches
/// the cached mtime, not even its seconds (`core.checkStat=minimal`),
/// whatever the wall clock says while the test runs (#350).
fn fixture_old_b() -> Fixture {
    let fx = files();
    if !fx.is_skipped() {
        let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        std::fs::File::options()
            .write(true)
            .open(fx.work_dir().join("b"))
            .and_then(|f| f.set_modified(old))
            .expect("set mtime of b");
    }
    commit(fx).remove("b")
}

/// Runs `checkout-index <args>`; with `core.checkStat=minimal` and
/// `core.trustCtime=false` the fixture's copies of unchanged files still
/// match the index.
fn run(fx: Fixture, args: &[&str]) {
    let mut fx = fx;
    let mut argv = vec![
        "-c",
        "core.checkStat=minimal",
        "-c",
        "core.trustCtime=false",
        "checkout-index",
    ];
    argv.extend_from_slice(args);
    fx.compare(&argv).checks(ALL).run();
}

#[test]
fn basics() {
    for args in [
        &[][..],
        &["-a"],
        &["-a", "-q"],
        &["a"],
        &["nonex"],
        &["-q", "nonex"],
        &["d"],
        &["-a", "a"],
        &["-u", "-f", "-a"],
        &["-a", "--prefix=out/"],
        &["-a", "--prefix=pre-"],
        &["-f", "a"],
    ] {
        run(fixture(), args);
    }
}

#[test]
fn missing_and_modified_files() {
    let gone = || {
        let fx = fixture().remove("a").remove("b").remove("d").remove("sc");
        #[cfg(unix)]
        let fx = fx.remove("l");
        fx
    };
    run(gone(), &["-a"]);
    run(gone(), &["-u", "-a"]);
    run(gone(), &["-n", "-a"]);
    run(gone(), &["a", "d/x"]);
    run(fixture().write("a", "mod\n"), &["a"]);
    run(fixture().write("a", "mod\n"), &["-f", "a"]);
    run(fixture().write("a", "mod\n").remove("b"), &["-n", "-a"]);
    run(fixture().write("a", "mod\n"), &["-q", "-a"]);
    // a directory where a file goes, and a file where a directory goes
    run(fixture().remove("b").write("b/z", "z\n"), &["-f", "b"]);
    run(fixture().remove("d").write("d", "q\n"), &["-f", "d/x"]);
    run(fixture().remove("d").write("d", "q\n"), &["d/x"]);
    // The second `b` sees the file the first one just wrote (#350). Without
    // `-u` the index keeps the old stat data, so it "already exists"; with
    // `-u` the entry is refreshed at once and the second `b` is up to date.
    run(fixture_old_b(), &["b", "b"]);
    run(fixture_old_b(), &["-u", "b", "b"]);
    run(fixture_old_b(), &["-u", "-a", "b"]);
    run(fixture_old_b(), &["-a", "-a"]);
}

#[test]
fn stdin_and_subdirectories() {
    let mut fx = fixture().remove("a").remove("d");
    fx.compare(["checkout-index", "--stdin"])
        .stdin("a\nd/x\nnonex\n")
        .checks(ALL)
        .run();
    let mut fx = fixture().remove("a").remove("d");
    fx.compare(["checkout-index", "-z", "--stdin"])
        .stdin("a\0d/x\0")
        .checks(ALL)
        .run();
    let mut fx = fixture().remove("b").remove("d/x");
    fx.compare(["checkout-index", "x", "../b", "../nonex"])
        .in_dir("d")
        .checks(ALL)
        .run();
    run(fixture(), &["--stdin", "a"]);
    run(fixture(), &["../x"]);
}

#[cfg(unix)]
#[test]
fn symlinks_as_files() {
    let mut fx = fixture().remove("l");
    fx.compare(["-c", "core.symlinks=false", "checkout-index", "l"])
        .checks(ALL)
        .run();
}

#[test]
fn unmerged_entries() {
    use std::io::Write;
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let blob = String::from_utf8(fx.git_stdout(["rev-parse", ":b"])).unwrap();
    let info = format!(
        "0 {zero}\ta\n100644 {blob} 1\ta\n100644 {blob} 2\ta\n",
        zero = "0".repeat(40),
        blob = blob.trim()
    );
    let mut child = std::process::Command::new(fx.reference_git())
        .args(["update-index", "--index-info"])
        .current_dir(fx.work_dir())
        .env("HOME", fx.home_dir())
        .env("XDG_CONFIG_HOME", fx.home_dir())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(info.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    for args in [&["-f", "a"][..], &["-f", "-a"], &["-q", "a"]] {
        let mut argv = vec!["checkout-index"];
        argv.extend_from_slice(args);
        fx.compare(&argv).checks(ALL).run();
    }
}

/// A merge leaving conflicts in `a` (stages 1-3), `b` (1, 2: deleted on
/// the other side) and `d/c` (2, 3: added on both sides), next to the
/// merged `m`, `sc` (executable) and `l` (symbolic link).
fn conflicted() -> Fixture {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a", "base\n")
        .write("b", "base\n")
        .write("m", "m\n")
        .write_executable("sc", "s\n");
    #[cfg(unix)]
    let fx = fx.symlink("m", "l");
    let fx = fx
        .git(["add", "."])
        .git(["commit", "-q", "-m", "base"])
        .git(["checkout", "-q", "-b", "side"])
        .write("a", "side\n")
        .write("d/c", "side\n")
        .git(["rm", "-q", "b"])
        .git(["add", "."])
        .git(["commit", "-q", "-m", "side"])
        .git(["checkout", "-q", "main"])
        .write("a", "main\n")
        .write("b", "main\n")
        .write("d/c", "main\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "main"]);
    if !fx.is_skipped() {
        // Fails with the conflicts.
        let _ = fx.git_output(["merge", "-q", "side"]);
    }
    fx
}

/// `--temp` and `--stage`: temporary file names are random, so they are
/// replaced by a placeholder in the output (their contents are checked by
/// the conformance script t2004).
#[test]
fn temp_and_stage() {
    use rgit_difftest::Normalizer;
    let checks = [Check::ExitCode, Check::Stdout, Check::Stderr];
    let names = || Normalizer::regex(r"\.merge_(file|link)_[A-Za-z0-9]{6}", ".merge_${1}_X");
    for args in [
        &["--temp", "a"][..],
        &["--temp", "-a"],
        &["--temp", "l", "sc", "m"],
        &["--stage=2", "--temp", "a"],
        &["--stage=all", "a"],
        &["--stage=all", "-a"],
        &["--stage=all", "--temp", "-a", "--stage=3"],
        &["--stage=1", "--temp", "-a"],
        &["--stage=3", "-f", "a"],
        &["--stage=2", "a"],
        &["--stage=2", "sc"],
        &["--stage=4", "a"],
        &["--stage=all", "--no-temp", "a"],
        &["--temp", "nope"],
        &["--stage=all", "nope", "sc"],
    ] {
        let mut fx = conflicted();
        if fx.is_skipped() {
            return;
        }
        let mut argv = vec!["checkout-index"];
        argv.extend_from_slice(args);
        fx.compare(&argv).checks(checks).normalize(names()).run();
    }
    let mut fx = conflicted();
    fx.compare(["checkout-index", "--stage=all", "-a"])
        .in_dir("d")
        .checks(checks)
        .normalize(names())
        .run();
    let mut fx = conflicted();
    fx.compare(["checkout-index", "--temp", "../m", "c"])
        .in_dir("d")
        .checks(checks)
        .normalize(names())
        .run();
}
