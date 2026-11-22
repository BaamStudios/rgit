// SPDX-License-Identifier: MIT
//! End to end: `rgit` killed by SIGINT or SIGTERM while it holds a lock
//! leaves no `.lock` file behind (`index.lock` while the `pre-commit` hook
//! runs, a ref lock of a prepared `update-ref --stdin` transaction). The
//! lock files themselves are tested in `rgit-fs/tests/signals.rs`.

#![cfg(unix)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn command(home: &Path, repo: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_rgit"));
    c.args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE");
    c
}

fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let repo = dir.path().join("r");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("f"), "a\n").unwrap();
    for args in [
        &["init", "-q"][..],
        &["add", "f"],
        &["commit", "-q", "-m", "one"],
    ] {
        assert!(command(&home, &repo, args).status().unwrap().success());
    }
    (dir, home, repo)
}

fn locks(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            locks(&p, out);
        } else if p.extension().is_some_and(|x| x == "lock") {
            out.push(p);
        }
    }
}

fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "timeout waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn kill(child: &mut Child, signal: &str) {
    let st = Command::new("kill")
        .args([format!("-{signal}"), child.id().to_string()])
        .status()
        .unwrap();
    assert!(st.success());
    let st = child.wait().unwrap();
    assert!(!st.success(), "{signal}: {st:?}");
}

#[test]
fn index_lock_during_pre_commit_hook() {
    for signal in ["INT", "TERM"] {
        let (dir, home, repo) = setup();
        let ready = dir.path().join("ready");
        let hook = repo.join(".git/hooks/pre-commit");
        std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::fs::write(
            &hook,
            format!(
                "#!/bin/sh\necho $$ >'{0}.tmp' && mv '{0}.tmp' '{0}'\nexec sleep 60\n",
                ready.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(repo.join("f"), "b\n").unwrap();
        let mut child = command(&home, &repo, &["commit", "-q", "-a", "-m", "two"])
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        wait_for("the hook", || ready.exists());
        let mut held = Vec::new();
        locks(&repo.join(".git"), &mut held);
        assert_eq!(held, vec![repo.join(".git/index.lock")]);
        kill(&mut child, signal);
        // The hook's sleep outlives rgit; stop it.
        let pid = std::fs::read_to_string(&ready).unwrap();
        let _ = Command::new("kill").arg(pid.trim()).status();
        let mut left = Vec::new();
        locks(&repo.join(".git"), &mut left);
        assert!(left.is_empty(), "{signal}: {left:?}");
    }
}

#[test]
fn ref_lock_of_prepared_transaction() {
    for signal in ["INT", "TERM"] {
        let (_dir, home, repo) = setup();
        let mut child = command(&home, &repo, &["update-ref", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin
            .write_all(b"start\nupdate refs/heads/new HEAD\nprepare\n")
            .unwrap();
        stdin.flush().unwrap();
        let lock = repo.join(".git/refs/heads/new.lock");
        wait_for("the ref lock", || lock.exists());
        kill(&mut child, signal);
        drop(stdin);
        let mut left = Vec::new();
        locks(&repo.join(".git"), &mut left);
        assert!(left.is_empty(), "{signal}: {left:?}");
    }
}
