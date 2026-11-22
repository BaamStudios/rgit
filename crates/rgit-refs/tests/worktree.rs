// SPDX-License-Identifier: MIT
//! The files store of a linked worktree (`FilesStore::with_worktree`):
//! per-worktree references live in the worktree's git directory, the
//! others in the common directory (gitrepository-layout(5),
//! git-worktree(1) REFS).

use std::fs;
use std::path::Path;

use rgit_hash::{HashKind, ObjectId};
use rgit_refs::files::FilesStore;
use rgit_refs::{IterItem, IterOptions, NoHooks, RefStore, RefUpdate, Target, Transaction};

const K: HashKind = HashKind::Sha1;

fn oid(n: u8) -> ObjectId {
    ObjectId::from_hex(K, format!("{n:02x}").repeat(20).as_bytes()).unwrap()
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn names(store: &FilesStore, prefix: &str) -> Vec<String> {
    store
        .iter(&IterOptions::prefix(prefix))
        .unwrap()
        .into_iter()
        .filter_map(|i| match i {
            IterItem::Ref(r) => Some(r.name),
            _ => None,
        })
        .collect()
}

#[test]
fn per_worktree_refs_are_split() {
    let tmp = tempfile::tempdir().unwrap();
    let common = tmp.path().join("repo.git");
    let wt = common.join("worktrees/wt");
    write(&common.join("HEAD"), "ref: refs/heads/main\n");
    write(&common.join("refs/heads/main"), &format!("{}\n", oid(1)));
    write(
        &common.join("refs/bisect/main-only"),
        &format!("{}\n", oid(1)),
    );
    write(&wt.join("HEAD"), "ref: refs/heads/side\n");
    write(&wt.join("refs/bisect/bad"), &format!("{}\n", oid(2)));
    write(&common.join("refs/heads/side"), &format!("{}\n", oid(3)));

    let main = FilesStore::new(&common, K);
    let linked = FilesStore::new(&common, K).with_worktree(&wt);

    // HEAD is the worktree's own.
    assert_eq!(
        linked.read_raw("HEAD").unwrap(),
        Some(Target::Symbolic("refs/heads/side".into()))
    );
    assert_eq!(linked.resolve_oid("HEAD").unwrap(), Some(oid(3)));
    assert_eq!(main.resolve_oid("HEAD").unwrap(), Some(oid(1)));

    // The other worktrees' refs by name.
    assert_eq!(
        linked.resolve_oid("main-worktree/HEAD").unwrap(),
        Some(oid(1))
    );
    assert_eq!(main.resolve_oid("worktrees/wt/HEAD").unwrap(), Some(oid(3)));
    assert_eq!(
        main.resolve_oid("worktrees/wt/refs/bisect/bad").unwrap(),
        Some(oid(2))
    );

    // Iteration: shared refs from the common directory, refs/bisect/ from
    // the worktree's own directory.
    assert_eq!(
        names(&linked, "refs/"),
        ["refs/bisect/bad", "refs/heads/main", "refs/heads/side"]
    );
    assert_eq!(
        names(&main, "refs/"),
        [
            "refs/bisect/main-only",
            "refs/heads/main",
            "refs/heads/side"
        ]
    );
    assert_eq!(names(&linked, "refs/bisect/"), ["refs/bisect/bad"]);

    // Writes go to the right directory.
    let mut tx = Transaction::new(&linked);
    tx.push(RefUpdate::create("refs/worktree/x", oid(4)))
        .unwrap();
    tx.push(RefUpdate::create("refs/heads/new", oid(4)))
        .unwrap();
    tx.commit(&mut NoHooks).unwrap();
    assert!(wt.join("refs/worktree/x").is_file());
    assert!(common.join("refs/heads/new").is_file());
    assert!(!common.join("refs/worktree/x").exists());

    // Updating HEAD through the branch it points to.
    let mut tx = Transaction::new(&linked);
    tx.push(RefUpdate::update("HEAD", oid(5), Some(oid(3))))
        .unwrap();
    tx.commit(&mut NoHooks).unwrap();
    assert_eq!(main.resolve_oid("refs/heads/side").unwrap(), Some(oid(5)));
    assert_eq!(main.resolve_oid("HEAD").unwrap(), Some(oid(1)));
}

#[test]
fn same_directory_is_not_linked() {
    let tmp = tempfile::tempdir().unwrap();
    let common = tmp.path().join("repo.git");
    write(&common.join("HEAD"), "ref: refs/heads/main\n");
    write(&common.join("refs/bisect/b"), &format!("{}\n", oid(1)));
    let store = FilesStore::new(&common, K).with_worktree(&common);
    assert_eq!(names(&store, "refs/"), ["refs/bisect/b"]);
}
