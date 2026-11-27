// SPDX-License-Identifier: MIT
//! The file system monitor (`core.fsmonitor`): differential tests of the
//! hook protocol and the `FSMN` extension (`update-index --fsmonitor`,
//! `--fsmonitor-valid`, `ls-files -f`, `status` checking only what the
//! hook reports), and an rgit-only test of the built-in daemon.

use rgit_difftest::{Check, Fixture};

/// The hook prints a fixed token and the paths listed in `.git/reported`.
const HOOK: &str = "#!/bin/sh\nprintf 'tok\\0'\ncat .git/reported 2>/dev/null\nexit 0\n";

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a", "a\n")
        .write("d/b", "b\n")
        .write("d/c", "c\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .write_executable(".git/hooks/fsm", HOOK)
        .git(["config", "core.fsmonitor", ".git/hooks/fsm"])
}

#[test]
fn extension_and_valid_bits() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    fx.compare(["update-index", "--fsmonitor"])
        .checks(Check::OUTPUT)
        .run();
    let mut fx = fx.git(["update-index", "--fsmonitor"]);
    fx.compare(["ls-files", "-f"]).checks(Check::OUTPUT).run();
    fx.compare(["update-index", "--no-fsmonitor"])
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["-c", "core.fsmonitor=", "update-index", "--fsmonitor"])
        .checks(Check::OUTPUT)
        .run();
    let mut fx = fx.git(["update-index", "--fsmonitor-valid", "a"]);
    fx.compare(["ls-files", "-f"]).checks(Check::OUTPUT).run();
    let mut fx = fx.git(["update-index", "--refresh"]);
    fx.compare(["ls-files", "-f"]).checks(Check::OUTPUT).run();
    let mut fx = fx.git(["update-index", "--no-fsmonitor-valid", "d/b"]);
    fx.compare(["ls-files", "-f"]).checks(Check::OUTPUT).run();
}

#[test]
fn status_checks_only_reported_paths() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx
        .git(["update-index", "--fsmonitor"])
        .git(["update-index", "--refresh"])
        .tick()
        .write("a", "changed\n")
        .write("d/b", "changed\n")
        .write("d/new", "n\n")
        .write(".git/reported", "d/b\0");
    for args in [
        &["status", "--porcelain"][..],
        &["status", "--porcelain=v2"],
        &["diff", "--stat"],
        &["ls-files", "-f"],
        &["-c", "core.fsmonitor=", "status", "--porcelain"],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
    // A reported directory invalidates everything below it.
    let mut fx = fx.write(".git/reported", "d\0");
    fx.compare(["status", "--porcelain"])
        .checks(Check::OUTPUT)
        .run();
    // "/" means everything.
    let mut fx = fx.write(".git/reported", "/\0");
    fx.compare(["status", "--porcelain"])
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn hook_failures() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx
        .git(["update-index", "--fsmonitor"])
        .write("a", "changed\n")
        .write_executable(".git/hooks/fsm", "#!/bin/sh\nexit 0\n");
    fx.compare(["status", "--porcelain"])
        .checks(Check::OUTPUT)
        .run();
    let mut fx = fx.write_executable(".git/hooks/fsm", "#!/bin/sh\nexit 1\n");
    fx.compare(["status", "--porcelain"])
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["-c", "core.fsmonitorHookVersion=3", "status", "--porcelain"])
        .checks(Check::OUTPUT)
        .run();
    let mut fx = fx.write_executable(".git/hooks/fsm", "#!/bin/sh\nprintf 'a\\0'\n");
    fx.compare(["-c", "core.fsmonitorHookVersion=1", "ls-files", "-f"])
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn bare_repository_is_incompatible() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "--bare", "bare.git"]);
    if fx.is_skipped() {
        return;
    }
    for args in [
        &[
            "-C",
            "bare.git",
            "-c",
            "core.fsmonitor=foo",
            "update-index",
            "--fsmonitor",
        ][..],
        &[
            "-C",
            "bare.git",
            "-c",
            "core.fsmonitor=true",
            "update-index",
            "--fsmonitor",
        ],
        &["-C", "bare.git", "fsmonitor--daemon", "run"],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
}

/// Stops the daemon of `dir` when dropped, so that a failing test does not
/// leave it running.
#[cfg(target_os = "linux")]
struct Daemon<'a> {
    fx: &'a Fixture,
    dir: std::path::PathBuf,
}

#[cfg(target_os = "linux")]
impl Daemon<'_> {
    fn rgit(&self, args: &[&str]) -> std::process::Output {
        let home = self.fx.home_dir();
        std::process::Command::new(env!("CARGO_BIN_EXE_rgit"))
            .args(args)
            .current_dir(&self.dir)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .expect("run rgit")
    }
}

#[cfg(target_os = "linux")]
impl Drop for Daemon<'_> {
    fn drop(&mut self) {
        let _ = self.rgit(&["fsmonitor--daemon", "stop"]);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn builtin_daemon() {
    let fx = fixture().git(["config", "core.fsmonitor", "true"]);
    if fx.is_skipped() {
        return;
    }
    let d = Daemon {
        dir: fx.work_dir(),
        fx: &fx,
    };
    let status = d.rgit(&["fsmonitor--daemon", "status"]);
    assert_eq!(status.status.code(), Some(1));
    let start = d.rgit(&["fsmonitor--daemon", "start"]);
    assert!(start.status.success(), "{start:?}");
    let again = d.rgit(&["fsmonitor--daemon", "start"]);
    assert_eq!(again.status.code(), Some(128));
    assert!(
        String::from_utf8_lossy(&again.stderr).contains("fsmonitor--daemon is already running")
    );
    let status = d.rgit(&["fsmonitor--daemon", "status"]);
    assert!(String::from_utf8_lossy(&status.stdout).starts_with("fsmonitor-daemon is watching '"));

    // The first query adds the extension; afterwards only reported paths
    // are checked, and the daemon reports every change.
    assert!(d.rgit(&["update-index", "--fsmonitor"]).status.success());
    assert!(d.rgit(&["status", "--porcelain"]).stdout.is_empty());
    let lsf = d.rgit(&["ls-files", "-f"]);
    assert_eq!(String::from_utf8_lossy(&lsf.stdout), "h a\nh d/b\nh d/c\n");
    std::fs::write(d.dir.join("d/b"), "changed\n").unwrap();
    std::fs::write(d.dir.join("new"), "n\n").unwrap();
    let st = d.rgit(&["status", "--porcelain"]);
    assert_eq!(String::from_utf8_lossy(&st.stdout), " M d/b\n?? new\n");

    let stop = d.rgit(&["fsmonitor--daemon", "stop"]);
    assert!(stop.status.success());
    assert_eq!(
        d.rgit(&["fsmonitor--daemon", "status"]).status.code(),
        Some(1)
    );
    let stop = d.rgit(&["fsmonitor--daemon", "stop"]);
    assert_eq!(stop.status.code(), Some(128));
}
