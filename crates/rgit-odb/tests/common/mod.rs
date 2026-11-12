// SPDX-License-Identifier: MIT

//! Helpers for running the reference git (located by `rgit-difftest`) with
//! stdin and a clean environment.

#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use rgit_hash::{HashKind, ObjectId};

/// The reference git, or `None` (tests are then skipped).
pub fn reference() -> Option<PathBuf> {
    let r = rgit_difftest::env::reference_git();
    if r.is_none() {
        eprintln!("skipped: no reference git");
    }
    r
}

pub struct Git {
    pub bin: PathBuf,
    pub dir: PathBuf,
}

impl Git {
    pub fn cmd(&self) -> Command {
        let mut c = Command::new(&self.bin);
        c.current_dir(&self.dir)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "@1112911993 -0700")
            .env("GIT_COMMITTER_DATE", "@1112912053 -0700")
            .env("LC_ALL", "C");
        c
    }

    pub fn raw(&self, args: &[&str], stdin: &[u8]) -> Output {
        let mut child = self
            .cmd()
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

    pub fn run(&self, args: &[&str], stdin: &[u8]) -> Vec<u8> {
        let out = self.raw(args, stdin);
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    pub fn oid(&self, args: &[&str], stdin: &[u8]) -> ObjectId {
        String::from_utf8(self.run(args, stdin))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    pub fn objects(&self) -> PathBuf {
        self.dir.join(".git/objects")
    }
}

/// A fresh repository in the temp dir.
pub fn repo(bin: &Path, name: &str, kind: HashKind) -> Git {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "rgit-odb-{name}-{}-{}-{}",
        std::process::id(),
        kind.name(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let g = Git {
        bin: bin.to_owned(),
        dir,
    };
    g.run(
        &["init", "-q", &format!("--object-format={}", kind.name())],
        b"",
    );
    g
}

impl Drop for Git {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            make_writable(&self.dir);
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

fn make_writable(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755));
                    make_writable(&p);
                }
            }
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Deterministic pseudo-random bytes (xorshift64*).
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

    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}
