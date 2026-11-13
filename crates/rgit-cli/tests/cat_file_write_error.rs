// SPDX-License-Identifier: MIT
//! `cat-file --batch` must fail when stdout cannot be written (e.g. a full
//! disk) instead of carrying on and silently dropping bytes from the middle
//! of the stream.

#![cfg(target_os = "linux")]

use std::fs::File;
use std::process::{Command, Stdio};

#[test]
fn batch_output_to_full_device_is_fatal() {
    let rgit = env!("CARGO_BIN_EXE_rgit");
    let dir = tempfile::tempdir().unwrap();
    // Never read or write the real user configuration.
    let isolated = |c: &mut Command| {
        c.env("HOME", dir.path())
            .env("XDG_CONFIG_HOME", dir.path().join(".config"))
            .env("GIT_CONFIG_NOSYSTEM", "1");
    };
    let mut c = Command::new(rgit);
    isolated(&mut c);
    let st = c
        .args(["init", "-q", "r"])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(st.success());
    let repo = dir.path().join("r");
    // A blob larger than the 64 KiB output buffer.
    let big = repo.join("big");
    std::fs::write(&big, vec![b'x'; 200_000]).unwrap();
    let mut c = Command::new(rgit);
    isolated(&mut c);
    let st = c
        .args(["hash-object", "-w", "big"])
        .current_dir(&repo)
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(st.success());

    for args in [
        &["cat-file", "--batch-all-objects", "--batch"][..],
        &["cat-file", "--batch-all-objects", "--batch-check"],
    ] {
        let mut c = Command::new(rgit);
        isolated(&mut c);
        let out = c
            .args(args)
            .current_dir(&repo)
            .stdout(File::create("/dev/full").unwrap())
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(128), "{args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.starts_with("fatal: unable to write to stdout: "),
            "{args:?}: {err}"
        );
    }
}
