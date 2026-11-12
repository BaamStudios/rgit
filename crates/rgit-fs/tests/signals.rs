// SPDX-License-Identifier: MIT
//! Lock and temp files disappear when the holder is killed by a signal or
//! calls `exit` without running destructors.
//!
//! The parent test re-runs this test binary with `RGIT_FS_CHILD` set; the
//! `child` test then takes `index.lock`, `config.lock` and a ref lock (plus
//! PID files and a temp file), prints `ready` and waits to be killed.
#![cfg(unix)]

mod common;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use common::Scratch;
use rgit_fs::{LockFile, LockOptions, TempFile};

const MODE: &str = "RGIT_FS_CHILD";
const DIR: &str = "RGIT_FS_DIR";

#[test]
fn child() {
    let Ok(mode) = std::env::var(MODE) else {
        return;
    };
    let dir = std::path::PathBuf::from(std::env::var(DIR).unwrap());
    let opts = LockOptions::new().pid_file(true);
    let mut locks = Vec::new();
    for name in ["index", "config", "refs/heads/main"] {
        let mut l = LockFile::acquire(dir.join(name), &opts).unwrap();
        l.write_all(b"partial").unwrap();
        locks.push(l);
    }
    let _tmp = TempFile::new_in(dir.join("objects"), rgit_fs::OBJECT_PREFIX).unwrap();
    println!("ready");
    std::io::stdout().flush().unwrap();
    match mode.as_str() {
        "exit" => {
            // Wait until the parent has seen the files.
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
            std::process::exit(3)
        }
        _ => std::thread::sleep(Duration::from_secs(60)),
    }
}

fn leftovers(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    for sub in ["", "refs/heads", "objects"] {
        for e in std::fs::read_dir(dir.join(sub)).unwrap() {
            let name = e.unwrap().file_name().to_string_lossy().into_owned();
            if name.ends_with(".lock") || name.starts_with("tmp_") {
                found.push(format!("{sub}/{name}"));
            }
        }
    }
    found
}

/// Starts the child, waits until it holds its locks, returns it.
fn spawn_child(dir: &Path, mode: &str) -> std::process::Child {
    for sub in ["refs/heads", "objects"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child", "--nocapture", "--test-threads=1"])
        .env(MODE, mode)
        .env(DIR, dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    loop {
        let line = lines.next().expect("child exited early").unwrap();
        if line.trim_end().ends_with("ready") {
            break;
        }
    }
    assert_eq!(leftovers(dir).len(), 7, "{:?}", leftovers(dir));
    child
}

#[test]
fn signals_remove_lock_files() {
    // SIGQUIT is handled the same way but left out here: its default action
    // dumps core, which would trigger crash reporting on developer machines.
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        let s = Scratch::new("sig");
        let mut child = spawn_child(s.path(), "sleep");
        unsafe { libc::kill(child.id() as libc::pid_t, sig) };
        let status = child.wait().unwrap();
        assert_eq!(status.signal(), Some(sig), "status {status:?}");
        assert!(
            leftovers(s.path()).is_empty(),
            "signal {sig}: {:?}",
            leftovers(s.path())
        );
    }
}

#[test]
fn exit_removes_lock_files() {
    let s = Scratch::new("exit");
    let mut child = spawn_child(s.path(), "exit");
    child.stdin.take().unwrap().write_all(b"go\n").unwrap();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(3));
    assert!(leftovers(s.path()).is_empty(), "{:?}", leftovers(s.path()));
}

#[test]
fn sigkill_leaves_lock_and_git_reports_it() {
    // SIGKILL cannot be handled: the lock stays, and the PID file lets the
    // next writer say the lock is stale.
    let s = Scratch::new("kill");
    let mut child = spawn_child(s.path(), "sleep");
    let pid = child.id();
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    child.wait().unwrap();
    let err = LockFile::acquire(s.path().join("index"), &LockOptions::new()).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "Unable to create '{}': File exists.\n\nLock was held by process {pid}, which is no longer running; the lock file appears to be stale",
            s.path().join("index.lock").display()
        )
    );
}
