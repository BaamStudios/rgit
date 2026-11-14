// SPDX-License-Identifier: MIT

//! Unit tests on an in-memory object store. The expected results were
//! taken from `git merge-tree --write-tree -X no-renames` on the same
//! histories (git 2.56).

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

use rgit_hash::{HashKind, ObjectId};
use rgit_object::{EntryMode, ObjectKind, RawObject, Tree, TreeEntry};

use crate::{
    CommitInfo, Commits, Error, MergeResult, MessageKind, Objects, Options, Version, merge_bases,
    merge_commits, merge_trees,
};

#[derive(Default)]
struct Mem {
    objects: RefCell<HashMap<ObjectId, RawObject>>,
    commits: RefCell<HashMap<ObjectId, (CommitInfo, &'static str)>>,
}

impl Objects for Mem {
    fn hash_kind(&self) -> HashKind {
        HashKind::Sha1
    }

    fn read(&self, id: &ObjectId) -> Result<Option<RawObject>, String> {
        Ok(self.objects.borrow().get(id).cloned())
    }

    fn write(&self, kind: ObjectKind, data: &[u8]) -> Result<ObjectId, String> {
        let id = rgit_hash::hash_object(HashKind::Sha1, kind.as_str(), data).unwrap();
        self.objects.borrow_mut().insert(
            id,
            RawObject {
                kind,
                data: data.to_vec(),
            },
        );
        Ok(id)
    }
}

impl Commits for Mem {
    fn commit(&self, id: &ObjectId) -> Result<CommitInfo, Error> {
        self.commits
            .borrow()
            .get(id)
            .map(|c| c.0.clone())
            .ok_or(Error::MissingObject(*id))
    }

    fn label(&self, id: &ObjectId) -> Vec<u8> {
        self.commits.borrow()[id].1.as_bytes().to_vec()
    }
}

/// A file of a test tree.
#[derive(Clone, Copy)]
enum F {
    File(&'static str),
    Exec(&'static str),
    Link(&'static str),
    Gitlink(u8),
    Binary(&'static str),
}

impl Mem {
    fn tree(&self, files: &[(&str, F)]) -> ObjectId {
        let mut flat = BTreeMap::new();
        for (path, f) in files {
            let (mode, oid) = match *f {
                F::File(c) => (
                    0o100644,
                    self.write(ObjectKind::Blob, c.as_bytes()).unwrap(),
                ),
                F::Exec(c) => (
                    0o100755,
                    self.write(ObjectKind::Blob, c.as_bytes()).unwrap(),
                ),
                F::Link(c) => (
                    0o120000,
                    self.write(ObjectKind::Blob, c.as_bytes()).unwrap(),
                ),
                F::Binary(c) => {
                    let data = [c.as_bytes(), b"\0"].concat();
                    (0o100644, self.write(ObjectKind::Blob, &data).unwrap())
                }
                F::Gitlink(n) => (
                    0o160000,
                    ObjectId::from_bytes(HashKind::Sha1, &[n; 20]).unwrap(),
                ),
            };
            flat.insert(path.to_string(), (mode, oid));
        }
        self.write_dir(&flat, "")
    }

    fn write_dir(&self, flat: &BTreeMap<String, (u32, ObjectId)>, prefix: &str) -> ObjectId {
        let mut tree = Tree::new();
        let mut subdirs = Vec::new();
        for (path, (mode, oid)) in flat.range(prefix.to_string()..) {
            let Some(rest) = path.strip_prefix(prefix) else {
                break;
            };
            match rest.split_once('/') {
                Some((dir, _)) => {
                    if !subdirs.contains(&dir.to_string()) {
                        subdirs.push(dir.to_string());
                    }
                }
                None => tree
                    .entries
                    .push(TreeEntry::new(EntryMode::from_bits(*mode), rest, *oid)),
            }
        }
        for d in subdirs {
            let id = self.write_dir(flat, &format!("{prefix}{d}/"));
            tree.entries.push(TreeEntry::new(EntryMode::TREE, d, id));
        }
        tree.sort();
        self.write(ObjectKind::Tree, &tree.serialize()).unwrap()
    }

    fn commit(
        &self,
        files: &[(&str, F)],
        parents: &[ObjectId],
        time: i64,
        label: &'static str,
    ) -> ObjectId {
        let tree = self.tree(files);
        let id = self
            .write(ObjectKind::Commit, format!("{label} {time}").as_bytes())
            .unwrap();
        let info = CommitInfo {
            tree,
            parents: parents.to_vec(),
            time,
        };
        self.commits.borrow_mut().insert(id, (info, label));
        id
    }

    /// The files of a tree: path, mode, content.
    fn files(&self, id: &ObjectId) -> Vec<(String, u32, String)> {
        let mut out = Vec::new();
        crate::engine::flatten_tree(self, id, &mut Vec::new(), &mut |p, v| {
            let content = if v.mode == 0o160000 {
                v.oid.to_hex()
            } else {
                String::from_utf8_lossy(&self.blob(&v.oid)).into_owned()
            };
            out.push((String::from_utf8_lossy(p).into_owned(), v.mode, content));
        })
        .unwrap();
        out
    }

    fn blob(&self, id: &ObjectId) -> Vec<u8> {
        self.objects.borrow()[id].data.clone()
    }

    fn content(&self, result: &MergeResult, path: &str) -> String {
        self.files(&result.tree)
            .into_iter()
            .find(|f| f.0 == path)
            .map(|f| f.2)
            .unwrap_or_else(|| panic!("{path} not in result"))
    }
}

fn opts() -> Options {
    Options {
        branch1: b"main".to_vec(),
        branch2: b"side".to_vec(),
        ancestor: b"base".to_vec(),
        ..Options::default()
    }
}

fn merge(m: &Mem, base: &[(&str, F)], ours: &[(&str, F)], theirs: &[(&str, F)]) -> MergeResult {
    let (b, o, t) = (m.tree(base), m.tree(ours), m.tree(theirs));
    merge_trees(m, &b, &o, &t, &opts()).unwrap()
}

/// Messages as `(type, text)`.
fn messages(r: &MergeResult) -> Vec<(&'static str, String)> {
    r.messages
        .iter()
        .map(|m| {
            (
                m.kind.as_str(),
                String::from_utf8_lossy(&m.text).into_owned(),
            )
        })
        .collect()
}

/// Conflicts as `path stage...`.
fn conflicts(r: &MergeResult) -> Vec<String> {
    r.conflicts
        .iter()
        .map(|c| {
            let stages: String = c
                .stages
                .iter()
                .enumerate()
                .filter(|(_, v)| v.is_some())
                .map(|(i, _)| format!(" {}", i + 1))
                .collect();
            format!("{}{stages}", String::from_utf8_lossy(&c.path))
        })
        .collect()
}

const LINES: &str = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";

#[test]
fn clean_content_merge() {
    let m = Mem::default();
    let r = merge(
        &m,
        &[("f", F::File(LINES)), ("same", F::File("x\n"))],
        &[
            ("f", F::File("O\n2\n3\n4\n5\n6\n7\n8\n9\n")),
            ("same", F::File("x\n")),
        ],
        &[
            ("f", F::File("1\n2\n3\n4\n5\n6\n7\n8\nT\n")),
            ("same", F::File("x\n")),
        ],
    );
    assert!(r.clean);
    assert_eq!(m.content(&r, "f"), "O\n2\n3\n4\n5\n6\n7\n8\nT\n");
    assert_eq!(messages(&r), [("Auto-merging", "Auto-merging f".into())]);
    assert!(r.conflicts.is_empty());
}

#[test]
fn unchanged_side_takes_other_side_silently() {
    let m = Mem::default();
    let r = merge(
        &m,
        &[("a/f", F::File("x\n")), ("g", F::File("g\n"))],
        &[("a/f", F::File("x\n")), ("g", F::File("G\n"))],
        &[("a/f", F::Exec("y\n")), ("g", F::File("g\n"))],
    );
    assert!(r.clean);
    assert!(r.messages.is_empty());
    let files = m.files(&r.tree);
    assert_eq!(files[0], ("a/f".into(), 0o100755, "y\n".into()));
    assert_eq!(files[1], ("g".into(), 0o100644, "G\n".into()));
}

#[test]
fn content_and_add_add_conflicts() {
    let m = Mem::default();
    let r = merge(
        &m,
        &[("c", F::File("a\n"))],
        &[("c", F::File("b\n")), ("n", F::File("ours\n"))],
        &[("c", F::File("c\n")), ("n", F::File("theirs\n"))],
    );
    assert!(!r.clean);
    assert_eq!(
        m.content(&r, "c"),
        "<<<<<<< main\nb\n=======\nc\n>>>>>>> side\n"
    );
    assert_eq!(conflicts(&r), ["c 1 2 3", "n 2 3"]);
    assert_eq!(
        messages(&r),
        [
            ("Auto-merging", "Auto-merging c".into()),
            (
                "CONFLICT (contents)",
                "CONFLICT (content): Merge conflict in c".into()
            ),
            ("Auto-merging", "Auto-merging n".into()),
            (
                "CONFLICT (contents)",
                "CONFLICT (add/add): Merge conflict in n".into()
            ),
        ]
    );
}

#[test]
fn diff3_style_shows_the_ancestor_label() {
    let m = Mem::default();
    let (b, o, t) = (
        m.tree(&[("f", F::File("a\n"))]),
        m.tree(&[("f", F::File("b\n"))]),
        m.tree(&[("f", F::File("c\n"))]),
    );
    let opts = Options {
        style: crate::ConflictStyle::Diff3,
        ..opts()
    };
    let r = merge_trees(&m, &b, &o, &t, &opts).unwrap();
    assert_eq!(
        m.content(&r, "f"),
        "<<<<<<< main\nb\n||||||| base\na\n=======\nc\n>>>>>>> side\n"
    );
}

#[test]
fn modify_delete_keeps_the_modified_version() {
    let m = Mem::default();
    let r = merge(
        &m,
        &[("md", F::File("a\n")), ("dm", F::File("a\n"))],
        &[("dm", F::File("c\n"))],
        &[("md", F::File("b\n"))],
    );
    assert!(!r.clean);
    assert_eq!(m.content(&r, "dm"), "c\n");
    assert_eq!(m.content(&r, "md"), "b\n");
    assert_eq!(conflicts(&r), ["dm 1 2", "md 1 3"]);
    assert_eq!(
        messages(&r),
        [
            (
                "CONFLICT (modify/delete)",
                "CONFLICT (modify/delete): dm deleted in side and modified in main.  Version main of dm left in tree.".into()
            ),
            (
                "CONFLICT (modify/delete)",
                "CONFLICT (modify/delete): md deleted in main and modified in side.  Version side of md left in tree.".into()
            ),
        ]
    );
}

#[test]
fn file_moves_out_of_the_way_of_a_directory() {
    let m = Mem::default();
    // Added file against added directory.
    let r = merge(
        &m,
        &[],
        &[("df", F::File("x\n"))],
        &[("df/f", F::File("in\n"))],
    );
    assert_eq!(conflicts(&r), ["df~main 2"]);
    assert_eq!(m.content(&r, "df/f"), "in\n");
    assert_eq!(m.content(&r, "df~main"), "x\n");
    assert_eq!(r.messages[0].paths, [b"df~main".to_vec(), b"df".to_vec()]);
    assert_eq!(
        messages(&r),
        [(
            "CONFLICT (file/directory)",
            "CONFLICT (file/directory): directory in the way of df from main; moving it to df~main instead.".into()
        )]
    );

    // Modified file against a directory: modify/delete at the new path.
    let r = merge(
        &m,
        &[("df", F::File("x\n"))],
        &[("df", F::File("y\n"))],
        &[("df/f", F::File("in\n"))],
    );
    assert_eq!(conflicts(&r), ["df~main 1 2"]);
    assert_eq!(
        messages(&r).iter().map(|(k, _)| *k).collect::<Vec<_>>(),
        ["CONFLICT (file/directory)", "CONFLICT (modify/delete)"]
    );

    // A directory that ends up empty does not push the file away.
    let r = merge(
        &m,
        &[("df/f", F::File("in\n"))],
        &[("df", F::File("x\n"))],
        &[("df/f", F::File("in\n"))],
    );
    assert!(r.clean);
    assert_eq!(m.files(&r.tree), [("df".into(), 0o100644, "x\n".into())]);
}

#[test]
fn moved_file_names_are_unique() {
    let m = Mem::default();
    let r = merge(
        &m,
        &[("df", F::File("x\n")), ("df~main", F::File("y\n"))],
        &[("df", F::File("y\n")), ("df~main", F::File("y\n"))],
        &[("df/f", F::File("in\n")), ("df~main", F::File("y\n"))],
    );
    assert_eq!(conflicts(&r), ["df~main_0 1 2"]);

    // Slashes in the label become underscores.
    let opts = Options {
        branch1: b"topic/one".to_vec(),
        ..opts()
    };
    let (b, o, t) = (
        m.tree(&[]),
        m.tree(&[("p", F::File("x\n"))]),
        m.tree(&[("p/a", F::File("y\n"))]),
    );
    let r = merge_trees(&m, &b, &o, &t, &opts).unwrap();
    assert_eq!(conflicts(&r), ["p~topic_one 2"]);
}

#[test]
fn deferred_deletion_reports_a_move_but_stays_clean() {
    // The side that replaced `c` by a directory also deleted `q`, which
    // the other side deleted too.
    let m = Mem::default();
    let r = merge(
        &m,
        &[("c", F::File("x\n")), ("q", F::File("y\n"))],
        &[("c/b", F::File("z\n"))],
        &[("c", F::File("x\n"))],
    );
    assert!(r.clean);
    assert_eq!(m.files(&r.tree), [("c/b".into(), 0o100644, "z\n".into())]);
    assert_eq!(
        messages(&r),
        [(
            "CONFLICT (file/directory)",
            "CONFLICT (file/directory): directory in the way of c from side; moving it to c~side instead.".into()
        )]
    );
    // Without such a deletion the merge is silent.
    let r = merge(
        &m,
        &[("c", F::File("x\n")), ("q", F::File("y\n"))],
        &[("c/b", F::File("z\n")), ("q", F::File("y\n"))],
        &[("c", F::File("x\n")), ("q", F::File("y\n"))],
    );
    assert!(r.clean && r.messages.is_empty());
}

#[test]
fn distinct_types_move_the_regular_file() {
    let m = Mem::default();
    let r = merge(
        &m,
        &[("f", F::File("x\n"))],
        &[("f", F::Link("tgt"))],
        &[("f", F::File("y\n"))],
    );
    assert_eq!(conflicts(&r), ["f 2", "f~side 1 3"]);
    assert_eq!(
        m.files(&r.tree),
        [
            ("f".into(), 0o120000, "tgt".into()),
            ("f~side".into(), 0o100644, "y\n".into())
        ]
    );
    assert_eq!(r.messages[0].kind, MessageKind::DistinctModes);
    assert_eq!(r.messages[0].paths, [b"f".to_vec(), b"f~side".to_vec()]);
    assert_eq!(
        String::from_utf8_lossy(&r.messages[0].text),
        "CONFLICT (distinct types): f had different types on each side; renamed one of them so each can be recorded somewhere."
    );

    // Neither is a regular file: both move.
    let r = merge(&m, &[], &[("s", F::Link("q"))], &[("s", F::Gitlink(2))]);
    assert_eq!(conflicts(&r), ["s~main 2", "s~side 3"]);
    assert!(String::from_utf8_lossy(&r.messages[0].text).contains("renamed both of them"));
}

#[test]
fn links_submodules_and_binaries_keep_our_version() {
    let m = Mem::default();
    let r = merge(
        &m,
        &[
            ("l", F::Link("a")),
            ("s", F::Gitlink(1)),
            ("b", F::Binary("b")),
        ],
        &[
            ("l", F::Link("b")),
            ("s", F::Gitlink(2)),
            ("b", F::Binary("o")),
        ],
        &[
            ("l", F::Link("c")),
            ("s", F::Gitlink(3)),
            ("b", F::Binary("t")),
        ],
    );
    assert_eq!(conflicts(&r), ["b 1 2 3", "l 1 2 3", "s 1 2 3"]);
    assert_eq!(m.content(&r, "l"), "b");
    assert_eq!(m.content(&r, "b"), "o\0");
    assert_eq!(m.content(&r, "s"), "02".repeat(20));
    assert_eq!(
        messages(&r),
        [
            (
                "CONFLICT (binary)",
                "warning: Cannot merge binary files: b (main vs. side)".into()
            ),
            ("Auto-merging", "Auto-merging b".into()),
            (
                "CONFLICT (contents)",
                "CONFLICT (content): Merge conflict in b".into()
            ),
            (
                "CONFLICT (contents)",
                "CONFLICT (content): Merge conflict in l".into()
            ),
            (
                "CONFLICT (submodule not initialized)",
                "Failed to merge submodule s (not checked out)".into()
            ),
            (
                "CONFLICT (contents)",
                "CONFLICT (submodule): Merge conflict in s".into()
            ),
        ]
    );
}

#[test]
fn executable_bit_merges_on_its_own() {
    let m = Mem::default();
    let r = merge(
        &m,
        &[("f", F::File("1\n2\n3\n"))],
        &[("f", F::Exec("O\n2\n3\n"))],
        &[("f", F::File("1\n2\nT\n"))],
    );
    assert!(r.clean);
    assert_eq!(
        m.files(&r.tree),
        [("f".into(), 0o100755, "O\n2\nT\n".into())]
    );

    // Added twice with the same content but different modes.
    let r = merge(&m, &[], &[("f", F::File("x\n"))], &[("f", F::Exec("x\n"))]);
    assert_eq!(conflicts(&r), ["f 2 3"]);
    assert_eq!(m.files(&r.tree), [("f".into(), 0o100644, "x\n".into())]);
    assert_eq!(
        messages(&r),
        [(
            "CONFLICT (contents)",
            "CONFLICT (add/add): Merge conflict in f".into()
        )]
    );
}

#[test]
fn index_entries_replace_conflicts_by_stages() {
    let m = Mem::default();
    let r = merge(
        &m,
        &[("a", F::File("a\n")), ("c", F::File("x\n"))],
        &[("a", F::File("a\n")), ("c", F::File("y\n"))],
        &[("a", F::File("a\n")), ("c", F::File("z\n"))],
    );
    let entries: Vec<(String, u8)> = r
        .index_entries(&m)
        .unwrap()
        .iter()
        .map(|e| {
            (
                String::from_utf8_lossy(e.path()).into_owned(),
                e.stage().get(),
            )
        })
        .collect();
    assert_eq!(
        entries,
        [
            ("a".into(), 0),
            ("c".into(), 1),
            ("c".into(), 2),
            ("c".into(), 3)
        ]
    );
}

/// A criss-cross history: both tips merged the other side's first commit
/// and resolved the conflict differently.
#[test]
fn criss_cross_merges_the_bases_first() {
    let m = Mem::default();
    let lines = |second: &'static str, last: &'static str| -> String {
        format!("1\n{second}\n3\n4\n5\n6\n7\n8\n{last}\n")
    };
    let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
    let base = m.commit(&[("f", F::File(leak(lines("2", "9"))))], &[], 1, "1a5c660");
    let s1 = m.commit(&[("f", F::File(leak(lines("S", "9"))))], &[base], 2, "s1");
    let m1 = m.commit(&[("f", F::File(leak(lines("M", "9"))))], &[base], 3, "m1");
    let mres = m.commit(
        &[("f", F::File(leak(lines("MM", "9"))))],
        &[m1, s1],
        4,
        "mres",
    );
    let sres = m.commit(
        &[("f", F::File(leak(lines("SS", "9"))))],
        &[s1, m1],
        5,
        "sres",
    );
    let main = m.commit(
        &[("f", F::File(leak(lines("MM", "m9"))))],
        &[mres],
        6,
        "main",
    );

    assert_eq!(merge_bases(&m, &main, &sres).unwrap(), [m1, s1]);
    let opts = Options {
        style: crate::ConflictStyle::Diff3,
        ..opts()
    };
    let r = merge_commits(&m, &m, &main, &sres, &opts).unwrap();
    assert!(!r.clean);
    assert_eq!(
        m.content(&r, "f"),
        "1\n<<<<<<< main\nMM\n||||||| merged common ancestors\n\
         <<<<<<<<< Temporary merge branch 1\nS\n||||||||| 1a5c660\n2\n\
         =========\nM\n>>>>>>>>> Temporary merge branch 2\n=======\nSS\n\
         >>>>>>> side\n3\n4\n5\n6\n7\n8\nm9\n"
    );
    // Only the outer merge reports.
    assert_eq!(
        messages(&r),
        [
            ("Auto-merging", "Auto-merging f".into()),
            (
                "CONFLICT (contents)",
                "CONFLICT (content): Merge conflict in f".into()
            ),
        ]
    );
}

#[test]
fn virtual_base_resolves_unmergeable_conflicts_to_the_base() {
    // The bases deleted and modified `f`: the virtual base keeps the
    // original, which shows as stage 1 of the outer conflict.
    let m = Mem::default();
    let base = m.commit(&[("f", F::File("b\n"))], &[], 1, "base");
    let s1 = m.commit(&[], &[base], 2, "s1");
    let m1 = m.commit(&[("f", F::File("m\n"))], &[base], 3, "m1");
    let main = m.commit(&[("f", F::File("mm\n"))], &[m1, s1], 4, "main");
    let side = m.commit(&[("f", F::File("ss\n"))], &[s1, m1], 5, "side");
    let r = merge_commits(&m, &m, &main, &side, &opts()).unwrap();
    let stage1 = r.conflicts[0].stages[0].unwrap();
    assert_eq!(m.blob(&stage1.oid), b"b\n");
    assert_eq!(
        stage1,
        Version {
            mode: 0o100644,
            oid: stage1.oid
        }
    );
}

#[test]
fn unrelated_histories() {
    let m = Mem::default();
    let a = m.commit(&[("f", F::File("a\n"))], &[], 1, "a");
    let b = m.commit(&[("f", F::File("b\n"))], &[], 2, "b");
    assert_eq!(
        merge_commits(&m, &m, &a, &b, &opts()),
        Err(Error::UnrelatedHistories)
    );
    let opts = Options {
        allow_unrelated_histories: true,
        style: crate::ConflictStyle::Diff3,
        ..opts()
    };
    let r = merge_commits(&m, &m, &a, &b, &opts).unwrap();
    assert_eq!(
        m.content(&r, "f"),
        "<<<<<<< main\na\n||||||| empty tree\n=======\nb\n>>>>>>> side\n"
    );
}

#[test]
fn merge_bases_are_listed_newest_first() {
    let m = Mem::default();
    let root = m.commit(&[], &[], 1, "root");
    let b3 = m.commit(&[("3", F::File("3"))], &[root], 2, "b3");
    let b1 = m.commit(&[("1", F::File("1"))], &[root], 3, "b1");
    let b2 = m.commit(&[("2", F::File("2"))], &[root], 4, "b2");
    let x = m.commit(&[], &[b1, b2, b3], 5, "x");
    let y = m.commit(&[], &[b3, b2, b1], 6, "y");
    assert_eq!(merge_bases(&m, &x, &y).unwrap(), [b2, b1, b3]);
    assert_eq!(merge_bases(&m, &x, &b1).unwrap(), [b1]);
}
