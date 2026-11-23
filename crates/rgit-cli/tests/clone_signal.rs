// SPDX-License-Identifier: MIT
//! `rgit clone` killed by a signal while it waits for the server leaves no
//! repository behind: a directory it created is removed, an empty
//! directory that existed before is emptied (#334).

#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn rgit(home: &Path, dir: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_rgit"));
    c.current_dir(dir)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_PROTOCOL")
        .env_remove("GIT_TEST_PROTOCOL_VERSION");
    c
}

#[test]
fn clone_killed_by_signal_leaves_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let status = rgit(&home, tmp.path())
        .args(["init", "-q", "--bare", "src.git"])
        .status()
        .unwrap();
    assert!(status.success());
    let url = format!("file://{}", tmp.path().join("src.git").display());
    for (target, existed) in [("new", false), ("empty", true)] {
        let dir = tmp.path().join(target);
        if existed {
            std::fs::create_dir(&dir).unwrap();
        }
        // The "server" never answers; it ends when rgit's end of the pipe
        // closes, so nothing outlives the test.
        let mut child = rgit(&home, tmp.path())
            .args(["clone", "-q", "--upload-pack", "read x", &url, target])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let start = Instant::now();
        while !dir.join(".git").join("HEAD").exists() {
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "clone did not start"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(200));
        let pid = libc::pid_t::try_from(child.id()).unwrap();
        // SAFETY: plain kill(2) of our own child.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
        let status = child.wait().unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGTERM), "{target}");
        if existed {
            let left: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
            assert!(left.is_empty(), "{target}: {left:?}");
        } else {
            assert!(!dir.exists(), "{target} still exists");
        }
    }
}
