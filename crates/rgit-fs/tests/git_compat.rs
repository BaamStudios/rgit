// SPDX-License-Identifier: MIT
//! Differential tests: lock conflicts between rgit-fs and the real `git`.
//! Skipped (pass) when no `git` binary is on PATH.

mod common;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{Scratch, git};
use rgit_fs::{LockError, LockFile, LockHolder, LockOptions};

fn init_repo(s: &Scratch) -> Option<()> {
    let out = git(s.path())?
        .args(["init", "-q", "-b", "main", "."])
        .output()
        .unwrap();
    assert!(out.status.success());
    std::fs::write(s.path().join("f"), "content\n").unwrap();
    Some(())
}

fn run(mut cmd: Command) -> (i32, String) {
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stderr).unwrap(),
    )
}

/// What git would print for our failed attempt, in git's format.
fn expected_stderr(err: &LockError) -> String {
    let mut s = String::new();
    if let Some(w) = err.warning() {
        s.push_str(&format!("warning: {w}\n"));
    }
    s.push_str(&format!("fatal: {err}\n"));
    s
}

#[test]
fn git_fails_on_our_lock_with_same_message() {
    let s = Scratch::new("git-vs-us");
    let Some(()) = init_repo(&s) else { return };
    let index = s.path().join(".git/index");
    for pid_file in [false, true] {
        let lock = LockFile::acquire(&index, &LockOptions::new().pid_file(pid_file)).unwrap();
        let ours = LockFile::acquire(&index, &LockOptions::new()).unwrap_err();
        if pid_file {
            assert!(matches!(
                ours,
                LockError::Exists {
                    holder: LockHolder::Alive { .. },
                    ..
                }
            ));
        }
        let mut cmd = git(s.path()).unwrap();
        cmd.args(["add", "f"]);
        let (code, stderr) = run(cmd);
        assert_eq!(code, 128);
        assert_eq!(stderr, expected_stderr(&ours));
        drop(lock);
    }
    // Lock released: git works again and leaves no lock behind.
    let mut cmd = git(s.path()).unwrap();
    cmd.args(["add", "f"]);
    assert_eq!(run(cmd).0, 0);
    assert!(!s.path().join(".git/index.lock").exists());
}

#[test]
fn ref_lock_message_matches() {
    let s = Scratch::new("ref");
    let Some(()) = init_repo(&s) else { return };
    let mut cmd = git(s.path()).unwrap();
    cmd.args([
        "-c",
        "user.name=T",
        "-c",
        "user.email=t@e",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "x",
    ]);
    assert_eq!(run(cmd).0, 0);
    let refpath = s.path().join(".git/refs/heads/main");
    let _lock = LockFile::acquire(&refpath, &LockOptions::new()).unwrap();
    let ours = LockFile::acquire(&refpath, &LockOptions::new()).unwrap_err();
    let mut cmd = git(s.path()).unwrap();
    cmd.args(["update-ref", "refs/heads/main", "HEAD"]);
    let (code, stderr) = run(cmd);
    assert_eq!(code, 128);
    assert_eq!(
        stderr,
        format!(
            "fatal: update_ref failed for ref 'refs/heads/main': cannot lock ref 'refs/heads/main': {ours}\n"
        )
    );
}

#[test]
fn pid_file_variants_match_git() {
    let s = Scratch::new("pidvariants");
    let Some(()) = init_repo(&s) else { return };
    let git_dir = s.path().join(".git");
    std::fs::write(git_dir.join("index.lock"), "").unwrap();
    let alive = format!("pid {}\n", std::process::id());
    let cases: Vec<&[u8]> = vec![
        b"",
        b"pid 99999",
        b"pid 99999\n",
        b"pid  99999",
        b"pid 0",
        b"pid -5",
        b"pid 99999 x",
        b"PID 99999",
        b"pid 99999\nmore",
        b" pid 99999",
        b"pid 0x10",
        b"pid",
        b"pid \n",
        b"   \n",
        b"pid +99999",
        b"pid 99999 \t\n",
        alive.as_bytes(),
    ];
    for content in cases {
        std::fs::write(git_dir.join("index~pid.lock"), content).unwrap();
        let ours = LockFile::acquire(git_dir.join("index"), &LockOptions::new()).unwrap_err();
        let mut cmd = git(s.path()).unwrap();
        cmd.args(["add", "f"]);
        let (code, stderr) = run(cmd);
        assert_eq!(code, 128, "{content:?}");
        assert_eq!(
            stderr,
            expected_stderr(&ours),
            "pid file {:?}",
            String::from_utf8_lossy(content)
        );
    }
}

#[test]
fn we_fail_while_git_holds_the_lock() {
    let s = Scratch::new("us-vs-git");
    let Some(()) = init_repo(&s) else { return };
    std::fs::write(s.path().join(".gitattributes"), "*.slow filter=slow\n").unwrap();
    std::fs::write(s.path().join("a.slow"), "x\n").unwrap();
    let mut cmd = git(s.path()).unwrap();
    // The clean filter runs while git holds index.lock.
    cmd.args([
        "-c",
        "core.lockfilePid=true",
        "-c",
        "filter.slow.clean=sleep 1; cat",
        "add",
        "a.slow",
    ])
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    let mut child = cmd.spawn().unwrap();
    let lock_path = s.path().join(".git/index.lock");
    let start = Instant::now();
    while !lock_path.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "git never took the lock"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let err = LockFile::acquire(s.path().join(".git/index"), &LockOptions::new()).unwrap_err();
    match &err {
        LockError::Exists {
            holder: LockHolder::Alive { pid },
            ..
        } => assert_eq!(*pid, u64::from(child.id())),
        other => panic!("unexpected {other:?}"),
    }
    assert!(
        err.to_string()
            .contains(&format!("Lock may be held by process {}", child.id()))
    );
    assert!(child.wait().unwrap().success());
    // git is done: we get the lock, and the commit is visible to git.
    let mut lock = LockFile::acquire(s.path().join(".git/config"), &LockOptions::new()).unwrap();
    let mut config = std::fs::read(s.path().join(".git/config")).unwrap();
    config.extend_from_slice(b"[rgit]\n\tprobe = yes\n");
    lock.write_all(&config).unwrap();
    lock.commit().unwrap();
    let mut cmd = git(s.path()).unwrap();
    cmd.args(["config", "rgit.probe"]);
    let out = cmd.output().unwrap();
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "yes\n");
}

#[test]
fn git_pid_file_format_matches_ours() {
    let s = Scratch::new("pidformat");
    let Some(()) = init_repo(&s) else { return };
    std::fs::write(s.path().join(".gitattributes"), "*.slow filter=slow\n").unwrap();
    std::fs::write(s.path().join("a.slow"), "x\n").unwrap();
    let copy = s.path().join("pid-copy");
    let mut cmd = git(s.path()).unwrap();
    cmd.args([
        "-c",
        "core.lockfilePid=true",
        "-c",
        &format!(
            "filter.slow.clean=cp .git/index~pid.lock {}; cat",
            copy.display()
        ),
        "add",
        "a.slow",
    ]);
    let child = cmd.spawn().unwrap();
    let pid = child.id();
    assert!(child.wait_with_output().unwrap().status.success());
    let theirs = std::fs::read_to_string(&copy).unwrap();
    assert_eq!(theirs, format!("pid {pid}\n"));
    assert!(!Path::new(&s.path().join(".git/index~pid.lock")).exists());
}
