// SPDX-License-Identifier: MIT
//! Behaviour of LockFile and TempFile on their own.

mod common;

use std::io::Write;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use common::Scratch;
use rgit_fs::{LockError, LockFile, LockHolder, LockOptions, TempFile, Timeout};

#[test]
fn commit_replaces_target() {
    let s = Scratch::new("commit");
    let target = s.path().join("config");
    std::fs::write(&target, "old").unwrap();
    let mut lock = LockFile::acquire(&target, &LockOptions::new()).unwrap();
    assert_eq!(lock.lock_path(), s.path().join("config.lock"));
    lock.write_all(b"new").unwrap();
    // Readers still see the old content until commit.
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old");
    lock.commit().unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
    assert!(!s.path().join("config.lock").exists());
}

#[test]
fn close_keeps_the_lock() {
    let s = Scratch::new("close");
    let target = s.path().join("ref");
    let mut lock = LockFile::acquire(&target, &LockOptions::new()).unwrap();
    lock.write_all(b"new").unwrap();
    lock.close().unwrap();
    assert!(s.path().join("ref.lock").exists());
    assert!(LockFile::acquire(&target, &LockOptions::new()).is_err());
    lock.commit().unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");

    let mut lock = LockFile::acquire(&target, &LockOptions::new()).unwrap();
    lock.close().unwrap();
    lock.rollback();
    assert!(!s.path().join("ref.lock").exists());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
}

#[test]
fn rollback_and_drop_remove_lock() {
    let s = Scratch::new("rollback");
    let target = s.path().join("index");
    std::fs::write(&target, "old").unwrap();
    let mut lock = LockFile::acquire(&target, &LockOptions::new()).unwrap();
    lock.write_all(b"new").unwrap();
    lock.rollback();
    assert!(!s.path().join("index.lock").exists());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old");

    {
        let _lock = LockFile::acquire(&target, &LockOptions::new()).unwrap();
        assert!(s.path().join("index.lock").exists());
    }
    assert!(!s.path().join("index.lock").exists());
}

#[test]
fn existing_lock_fails_with_git_message() {
    let s = Scratch::new("exists");
    let target = s.path().join("index");
    std::fs::write(s.path().join("index.lock"), "").unwrap();
    let err = LockFile::acquire(&target, &LockOptions::new()).unwrap_err();
    assert!(matches!(
        err,
        LockError::Exists {
            holder: LockHolder::Unknown,
            ..
        }
    ));
    assert_eq!(
        err.to_string(),
        format!(
            "Unable to create '{}': File exists.\n\n\
             Another git process seems to be running in this repository, or the lock file may be stale",
            s.path().join("index.lock").display()
        )
    );
    // A failed attempt must not delete the other process's lock.
    assert!(s.path().join("index.lock").exists());
}

#[cfg(unix)]
#[test]
fn other_errors_use_strerror() {
    let s = Scratch::new("enoent");
    let err =
        LockFile::acquire(s.path().join("missing/dir/file"), &LockOptions::new()).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "Unable to create '{}': No such file or directory",
            s.path().join("missing/dir/file.lock").display()
        )
    );
}

#[test]
fn exactly_one_concurrent_writer_wins() {
    let s = Scratch::new("race");
    let target = Arc::new(s.path().join("packed-refs"));
    let n = 16;
    let barrier = Arc::new(Barrier::new(n));
    let done = Arc::new(Barrier::new(n));
    let handles: Vec<_> = (0..n)
        .map(|_| {
            let (target, barrier, done) = (target.clone(), barrier.clone(), done.clone());
            std::thread::spawn(move || {
                barrier.wait();
                let res = LockFile::acquire(&*target, &LockOptions::new());
                // Hold the lock until everybody has tried.
                done.wait();
                match res {
                    Ok(lock) => {
                        lock.commit().unwrap();
                        true
                    }
                    Err(LockError::Exists { .. }) => false,
                    Err(e) => panic!("{e}"),
                }
            })
        })
        .collect();
    let winners = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .filter(|w| *w)
        .count();
    assert_eq!(winners, 1);
}

#[test]
fn timeout_retries_until_lock_is_released() {
    let s = Scratch::new("retry");
    let target = s.path().join("HEAD");
    let held = LockFile::acquire(&target, &LockOptions::new()).unwrap();
    let t2 = target.clone();
    let waiter = std::thread::spawn(move || {
        LockFile::acquire(
            &t2,
            &LockOptions::new().timeout(Timeout::After(Duration::from_secs(5))),
        )
        .map(|l| l.rollback())
        .is_ok()
    });
    std::thread::sleep(Duration::from_millis(150));
    held.rollback();
    assert!(waiter.join().unwrap());
}

#[test]
fn timeout_gives_up_after_duration() {
    let s = Scratch::new("giveup");
    let target = s.path().join("HEAD");
    let _held = LockFile::acquire(&target, &LockOptions::new()).unwrap();
    let start = Instant::now();
    let err = LockFile::acquire(
        &target,
        &LockOptions::new().timeout(Timeout::FILES_REF_DEFAULT),
    )
    .unwrap_err();
    let took = start.elapsed();
    assert!(matches!(err, LockError::Exists { .. }));
    assert!(took >= Duration::from_millis(100), "{took:?}");
    assert!(took < Duration::from_millis(1000), "{took:?}");
}

#[cfg(unix)]
#[test]
fn follows_symlinks() {
    let s = Scratch::new("symlink");
    std::fs::write(s.path().join("real"), "old").unwrap();
    std::os::unix::fs::symlink("real", s.path().join("link")).unwrap();
    let mut lock = LockFile::acquire(s.path().join("link"), &LockOptions::new()).unwrap();
    assert_eq!(lock.lock_path(), s.path().join("real.lock"));
    lock.write_all(b"new").unwrap();
    lock.commit().unwrap();
    assert!(
        std::fs::symlink_metadata(s.path().join("link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_to_string(s.path().join("real")).unwrap(),
        "new"
    );

    let lock = LockFile::acquire(
        s.path().join("link"),
        &LockOptions::new().follow_symlinks(false),
    )
    .unwrap();
    assert_eq!(lock.lock_path(), s.path().join("link.lock"));
}

#[test]
fn pid_file_written_and_removed() {
    let s = Scratch::new("pid");
    let target = s.path().join("index");
    let pid_path = s.path().join("index~pid.lock");
    let lock = LockFile::acquire(&target, &LockOptions::new().pid_file(true)).unwrap();
    assert_eq!(
        std::fs::read_to_string(&pid_path).unwrap(),
        format!("pid {}\n", std::process::id())
    );
    // A second writer learns who holds the lock.
    let err = LockFile::acquire(&target, &LockOptions::new()).unwrap_err();
    assert!(
        matches!(err, LockError::Exists { holder: LockHolder::Alive { pid }, .. } if pid == u64::from(std::process::id()))
    );
    lock.commit().unwrap();
    assert!(!pid_path.exists());

    // Default: no PID file.
    let lock = LockFile::acquire(&target, &LockOptions::new()).unwrap();
    assert!(!pid_path.exists());
    lock.rollback();
}

#[test]
fn malformed_pid_file_warns() {
    let s = Scratch::new("malformed");
    std::fs::write(s.path().join("index.lock"), "").unwrap();
    std::fs::write(s.path().join("index~pid.lock"), "garbage").unwrap();
    let err = LockFile::acquire(s.path().join("index"), &LockOptions::new()).unwrap_err();
    assert_eq!(
        err.warning().unwrap(),
        format!(
            "malformed lock pid file '{}'",
            s.path().join("index~pid.lock").display()
        )
    );
    assert!(err.to_string().ends_with(
        "Another git process seems to be running in this repository, or the lock file may be stale"
    ));
}

#[cfg(unix)]
#[test]
fn shared_repository_adjusts_lock_mode() {
    use std::os::unix::fs::PermissionsExt;
    let s = Scratch::new("shared");
    let lock = LockFile::acquire(
        s.path().join("index"),
        &LockOptions::new().shared(rgit_fs::SharedRepository::Mode(0o640)),
    )
    .unwrap();
    let mode = std::fs::metadata(lock.lock_path())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o640);
}

#[test]
fn tempfile_persist_and_drop() {
    let s = Scratch::new("temp");
    let mut t = TempFile::new_in(s.path(), rgit_fs::OBJECT_PREFIX).unwrap();
    let name = t.path().file_name().unwrap().to_str().unwrap().to_owned();
    assert!(
        name.starts_with("tmp_obj_") && name.len() == "tmp_obj_".len() + 6,
        "{name}"
    );
    t.write_all(b"data").unwrap();
    let dest = s.path().join("final");
    t.persist(&dest).unwrap();
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "data");

    let t = TempFile::new_in(s.path(), rgit_fs::PACK_PREFIX).unwrap();
    let p = t.path().to_owned();
    assert!(p.exists());
    drop(t);
    assert!(!p.exists());

    let t = TempFile::new_in(s.path(), "keep_").unwrap();
    let (_f, p) = t.keep();
    assert!(p.exists());
}
