// SPDX-License-Identifier: MIT

//! Compare `hash_object` with the reference `git hash-object` on varied
//! inputs, for both hash kinds. Skipped when no `git` binary is available.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rgit_hash::{HashKind, hash_object};

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn git(dir: &Path, args: &[&str], stdin: &[u8]) -> String {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_DEFAULT_HASH")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn git");
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
}

fn scratch_repo(kind: HashKind) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rgit-hash-test-{}-{}",
        std::process::id(),
        kind.name()
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

fn inputs() -> Vec<Vec<u8>> {
    let mut v: Vec<Vec<u8>> = vec![
        b"".to_vec(),
        b"Hello World".to_vec(),
        b"line\n".to_vec(),
        b"\0\0\0".to_vec(),
        (0..=255u8).collect(),
        "UTF-8: \u{e4}\u{f6}\u{fc} \u{1f600}\r\n"
            .as_bytes()
            .to_vec(),
    ];
    // Lengths around the 64 byte block and the 56 byte padding boundary,
    // taking the header length into account.
    for n in [
        40, 41, 47, 48, 49, 55, 56, 57, 63, 64, 65, 119, 120, 128, 1000,
    ] {
        v.push((0..n).map(|i| b'a' + (i % 26) as u8).collect());
    }
    // A larger pseudo-random input (1 MiB + 1).
    let mut x: u32 = 0x1234_5678;
    v.push(
        (0..(1 << 20) + 1)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect(),
    );
    v
}

#[test]
fn matches_git_hash_object() {
    if !git_available() {
        eprintln!("git not found, skipping");
        return;
    }
    for kind in HashKind::ALL {
        let repo = scratch_repo(kind);
        for data in inputs() {
            for ty in ["blob", "tree", "commit", "tag"] {
                // --literally: hash the bytes as given without validating
                // tree/commit/tag syntax.
                let expected = git(
                    &repo,
                    &["hash-object", "--stdin", "--literally", "-t", ty],
                    &data,
                );
                let got = hash_object(kind, ty, &data).unwrap().to_hex();
                assert_eq!(got, expected, "{kind} {ty} len {}", data.len());
            }
        }
        let _ = std::fs::remove_dir_all(&repo);
    }
}
