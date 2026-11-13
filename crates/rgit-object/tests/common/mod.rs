// SPDX-License-Identifier: MIT

//! Helpers for running the reference `git` binary (located by
//! `rgit-difftest`: `$RGIT_REFERENCE_GIT`, `upstream/git/git`, `PATH`) with
//! stdin and a clean environment.

#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use rgit_hash::HashKind;

fn reference() -> Option<PathBuf> {
    static REF: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    REF.get_or_init(rgit_difftest::env::reference_git).clone()
}

pub fn git_available() -> bool {
    reference().is_some()
}

/// A `git` command with a clean environment.
pub fn git_cmd(dir: &Path) -> Command {
    let mut c = Command::new(reference().expect("reference git"));
    c.current_dir(dir)
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "@1112911993 -0700")
        .env("GIT_COMMITTER_DATE", "@1112912053 -0700")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_DEFAULT_HASH");
    c
}

/// Run git with `stdin`, returning the raw output (success or not).
pub fn git_raw(dir: &Path, args: &[&str], stdin: &[u8]) -> Output {
    let mut child = git_cmd(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn git");
    let mut input = child.stdin.take().unwrap();
    let data = stdin.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = input.write_all(&data);
    });
    let out = child.wait_with_output().unwrap();
    writer.join().unwrap();
    out
}

/// Run git and return stdout; panics on failure.
pub fn git(dir: &Path, args: &[&str], stdin: &[u8]) -> Vec<u8> {
    let out = git_raw(dir, args, stdin);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// A fresh empty repository in the temp dir.
pub fn scratch_repo(name: &str, kind: HashKind) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "rgit-object-{name}-{}-{}-{}",
        std::process::id(),
        kind.name(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(
        &dir,
        &["init", "-q", &format!("--object-format={}", kind.name())],
        b"",
    );
    dir
}

/// A small deterministic PRNG (xorshift64*).
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.max(1))
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}
