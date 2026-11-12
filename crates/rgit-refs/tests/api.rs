// SPDX-License-Identifier: MIT
//! Transaction API, hooks and error cases that need no reference git.

use std::path::{Path, PathBuf};

use rgit_hash::{HashKind, ObjectId};
use rgit_refs::files::FilesStore;
use rgit_refs::{
    HookState, NoHooks, RefHooks, RefStore, RefUpdate, ReflogUpdate, Target, Transaction,
    TransactionError, TransactionState,
};

const A: &str = "b2e6e911d0750ef5d57039fc461069c4a5277897";
const B: &str = "8b21c372ce20790e9d010c31775b2617e31c4bd1";

fn oid(hex: &str) -> ObjectId {
    ObjectId::from_hex(HashKind::Sha1, hex.as_bytes()).unwrap()
}

struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("rgit-refs-api-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(p.join("refs/heads")).unwrap();
        std::fs::create_dir_all(p.join("refs/tags")).unwrap();
        std::fs::write(p.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        Self(p)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn lock_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.to_string_lossy().ends_with(".lock") {
                out.push(p);
            }
        }
    }
    out
}

#[derive(Default)]
struct Recorder {
    reflogs: Vec<ReflogUpdate>,
    deleted_logs: Vec<String>,
    states: Vec<HookState>,
    reject_prepared: bool,
}

impl RefHooks for Recorder {
    fn reflog_append(&mut self, update: &ReflogUpdate) -> Result<(), String> {
        self.reflogs.push(update.clone());
        Ok(())
    }
    fn reflog_delete(&mut self, name: &str) {
        self.deleted_logs.push(name.to_owned());
    }
    fn reference_transaction(
        &mut self,
        state: HookState,
        _updates: &[(ObjectId, ObjectId, String)],
    ) -> Result<(), String> {
        self.states.push(state);
        if self.reject_prepared && state == HookState::Prepared {
            return Err("hook declined".into());
        }
        Ok(())
    }
}

#[test]
fn commit_writes_loose_files_and_logs_head() {
    let d = Dir::new("commit");
    let store = FilesStore::new(&d.0, HashKind::Sha1);
    let mut hooks = Recorder::default();
    let mut tx = Transaction::new(&store);
    tx.push(RefUpdate::create("refs/heads/main", oid(A)).message("first"))
        .unwrap();
    tx.push(RefUpdate::create("refs/tags/v1", oid(B))).unwrap();
    tx.commit(&mut hooks).unwrap();
    assert_eq!(
        std::fs::read_to_string(d.0.join("refs/heads/main")).unwrap(),
        format!("{A}\n")
    );
    assert_eq!(store.resolve_oid("HEAD").unwrap(), Some(oid(A)));
    let names: Vec<_> = hooks.reflogs.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["refs/heads/main", "refs/tags/v1", "HEAD"]);
    assert!(hooks.reflogs[0].create && hooks.reflogs[2].create);
    // refs/tags/ gets no reflog by default (core.logAllRefUpdates=true).
    assert!(!hooks.reflogs[1].create);
    assert_eq!(hooks.reflogs[0].message, "first");
    assert!(hooks.reflogs[0].old.is_null());
    assert_eq!(hooks.states, [HookState::Prepared, HookState::Committed]);
    assert!(lock_files(&d.0).is_empty());
}

#[test]
fn failed_transaction_changes_nothing() {
    let d = Dir::new("fail");
    let store = FilesStore::new(&d.0, HashKind::Sha1);
    let mut tx = Transaction::new(&store);
    tx.push(RefUpdate::create("refs/heads/main", oid(A)))
        .unwrap();
    tx.commit(&mut NoHooks).unwrap();

    let mut tx = Transaction::new(&store);
    tx.push(RefUpdate::create("refs/heads/new/deep", oid(A)))
        .unwrap();
    tx.push(RefUpdate::update("refs/heads/main", oid(B), Some(oid(B))))
        .unwrap();
    let err = tx.commit(&mut NoHooks).unwrap_err();
    assert_eq!(
        err.to_string(),
        format!("cannot lock ref 'refs/heads/main': is at {A} but expected {B}")
    );
    assert!(lock_files(&d.0).is_empty());
    assert!(!d.0.join("refs/heads/new").exists());
    assert_eq!(store.resolve_oid("refs/heads/main").unwrap(), Some(oid(A)));
}

#[test]
fn prepare_then_abort_prunes_directories() {
    let d = Dir::new("abort");
    let store = FilesStore::new(&d.0, HashKind::Sha1);
    let mut hooks = Recorder::default();
    let mut tx = Transaction::new(&store);
    tx.push(RefUpdate::create("refs/heads/nested/something", oid(A)))
        .unwrap();
    tx.prepare(&mut hooks).unwrap();
    assert_eq!(tx.state(), TransactionState::Prepared);
    assert_eq!(lock_files(&d.0).len(), 1);
    assert!(matches!(
        tx.push(RefUpdate::create("refs/heads/x", oid(A))),
        Err(TransactionError::Closed)
    ));
    tx.abort(&mut hooks);
    assert!(lock_files(&d.0).is_empty());
    assert!(!d.0.join("refs/heads/nested").exists());
    assert_eq!(hooks.states, [HookState::Prepared, HookState::Aborted]);
}

#[test]
fn dropping_a_prepared_transaction_releases_locks() {
    let d = Dir::new("drop");
    let store = FilesStore::new(&d.0, HashKind::Sha1);
    {
        let mut tx = Transaction::new(&store);
        tx.push(RefUpdate::create("refs/heads/x", oid(A))).unwrap();
        tx.prepare(&mut NoHooks).unwrap();
        assert_eq!(lock_files(&d.0).len(), 1);
    }
    assert!(lock_files(&d.0).is_empty());
    assert_eq!(store.read_raw("refs/heads/x").unwrap(), None);
}

#[test]
fn hook_can_reject_prepared() {
    let d = Dir::new("reject");
    let store = FilesStore::new(&d.0, HashKind::Sha1);
    let mut hooks = Recorder {
        reject_prepared: true,
        ..Recorder::default()
    };
    let mut tx = Transaction::new(&store);
    tx.push(RefUpdate::create("refs/heads/x", oid(A))).unwrap();
    let err = tx.commit(&mut hooks).unwrap_err();
    assert_eq!(err.to_string(), "hook declined");
    assert!(lock_files(&d.0).is_empty());
    assert_eq!(store.read_raw("refs/heads/x").unwrap(), None);
}

#[test]
fn delete_reports_reflog_removal_and_head_entry() {
    let d = Dir::new("delete");
    let store = FilesStore::new(&d.0, HashKind::Sha1);
    let mut tx = Transaction::new(&store);
    tx.push(RefUpdate::create("refs/heads/main", oid(A)))
        .unwrap();
    tx.commit(&mut NoHooks).unwrap();

    let mut hooks = Recorder::default();
    let mut tx = Transaction::new(&store);
    tx.push(RefUpdate::delete(
        "refs/heads/main",
        HashKind::Sha1,
        Some(oid(A)),
    ))
    .unwrap();
    tx.commit(&mut hooks).unwrap();
    assert_eq!(hooks.deleted_logs, ["refs/heads/main"]);
    let head: Vec<_> = hooks.reflogs.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(head, ["HEAD"]);
    assert!(hooks.reflogs[0].new.is_null());
    assert!(!d.0.join("refs/heads/main").exists());
    assert!(d.0.join("refs/heads").is_dir());
}

#[test]
fn symbolic_refs() {
    let d = Dir::new("symref");
    let store = FilesStore::new(&d.0, HashKind::Sha1);
    let mut tx = Transaction::new(&store);
    tx.push(RefUpdate::symref_create(
        "refs/remotes/o/HEAD",
        "refs/remotes/o/main",
    ))
    .unwrap();
    tx.push(RefUpdate::create("refs/remotes/o/main", oid(B)))
        .unwrap();
    tx.commit(&mut NoHooks).unwrap();
    assert_eq!(
        std::fs::read_to_string(d.0.join("refs/remotes/o/HEAD")).unwrap(),
        "ref: refs/remotes/o/main\n"
    );
    let r = store.resolve("refs/remotes/o/HEAD").unwrap().unwrap();
    assert_eq!(r.chain, ["refs/remotes/o/HEAD", "refs/remotes/o/main"]);
    assert_eq!(r.oid, Some(oid(B)));
    assert_eq!(
        store.read_raw("refs/remotes/o/HEAD").unwrap(),
        Some(Target::Symbolic("refs/remotes/o/main".into()))
    );
}

#[test]
fn bad_names_are_refused() {
    let d = Dir::new("badname");
    let store = FilesStore::new(&d.0, HashKind::Sha1);
    let mut tx = Transaction::new(&store);
    tx.push(RefUpdate::create("refs/heads/a..b", oid(A)))
        .unwrap();
    let err = tx.commit(&mut NoHooks).unwrap_err();
    assert_eq!(
        err.to_string(),
        "refusing to update ref with bad name 'refs/heads/a..b'"
    );
}

#[cfg(unix)]
#[test]
fn legacy_symlink_head_is_symbolic() {
    let d = Dir::new("symlink");
    std::fs::remove_file(d.0.join("HEAD")).unwrap();
    std::os::unix::fs::symlink("refs/heads/main", d.0.join("HEAD")).unwrap();
    let store = FilesStore::new(&d.0, HashKind::Sha1);
    assert_eq!(
        store.read_raw("HEAD").unwrap(),
        Some(Target::Symbolic("refs/heads/main".into()))
    );
}
