// SPDX-License-Identifier: MIT

//! Merges with rename detection. The expected results were taken from
//! `git merge-tree --write-tree --messages` on the same histories (git
//! 2.56).

use super::{F, Mem, conflicts, messages};
use crate::{DirectoryRenames, MergeResult, Options, merge_trees};

const A: &str = "a 1\na 2\na 3\na 4\na 5\na 6\na 7\na 8\na 9\na 10\n";

fn with(a: &str, from: &str, to: &str) -> String {
    a.replace(from, to)
}

fn opts() -> Options {
    Options {
        branch1: b"main".to_vec(),
        branch2: b"side".to_vec(),
        ancestor: b"base".to_vec(),
        ..Options::default()
    }
}

fn merge_with(
    m: &Mem,
    opts: &Options,
    base: &[(&str, F)],
    ours: &[(&str, F)],
    theirs: &[(&str, F)],
) -> MergeResult {
    let (b, o, t) = (m.tree(base), m.tree(ours), m.tree(theirs));
    merge_trees(m, &b, &o, &t, opts).unwrap()
}

fn paths(m: &Mem, r: &MergeResult) -> Vec<String> {
    m.files(&r.tree).into_iter().map(|f| f.0).collect()
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

#[test]
fn renamed_file_takes_the_other_sides_changes() {
    let m = Mem::default();
    let changed = leak(format!("{A}x\n"));
    let r = merge_with(
        &m,
        &opts(),
        &[("a", F::File(A))],
        &[("b", F::File(A))],
        &[("a", F::File(changed))],
    );
    assert!(r.clean);
    assert_eq!(paths(&m, &r), ["b"]);
    assert_eq!(m.content(&r, "b"), changed);
    assert!(r.messages.is_empty());

    // Without rename detection: modify/delete.
    let no = Options {
        renames: false,
        ..opts()
    };
    let r = merge_with(
        &m,
        &no,
        &[("a", F::File(A))],
        &[("b", F::File(A))],
        &[("a", F::File(changed))],
    );
    assert!(!r.clean);
    assert_eq!(conflicts(&r), ["a 1 3"]);
}

#[test]
fn conflict_markers_name_the_paths() {
    let m = Mem::default();
    let r = merge_with(
        &m,
        &Options {
            style: crate::ConflictStyle::Diff3,
            ..opts()
        },
        &[("a", F::File(A))],
        &[("b", F::File(leak(with(A, "a 5", "O"))))],
        &[("a", F::File(leak(with(A, "a 5", "T"))))],
    );
    assert_eq!(conflicts(&r), ["b 1 2 3"]);
    let content = m.content(&r, "b");
    assert!(
        content.contains("<<<<<<< main:b\nO\n||||||| base:a\na 5\n=======\nT\n>>>>>>> side:a\n")
    );
    assert_eq!(
        messages(&r),
        [
            ("Auto-merging", "Auto-merging b".into()),
            (
                "CONFLICT (contents)",
                "CONFLICT (content): Merge conflict in b".into()
            ),
        ]
    );
}

#[test]
fn rename_delete() {
    let m = Mem::default();
    let r = merge_with(&m, &opts(), &[("a", F::File(A))], &[("b", F::File(A))], &[]);
    assert!(!r.clean);
    assert_eq!(paths(&m, &r), ["b"]);
    assert_eq!(conflicts(&r), ["b 1 2"]);
    assert_eq!(
        messages(&r),
        [(
            "CONFLICT (rename/delete)",
            "CONFLICT (rename/delete): a renamed to b in main, but deleted in side.".into()
        )]
    );
    // Renamed and changed: a modify/delete as well.
    let changed = leak(format!("{A}x\n"));
    let r = merge_with(
        &m,
        &opts(),
        &[("a", F::File(A))],
        &[],
        &[("b", F::File(changed))],
    );
    assert_eq!(conflicts(&r), ["b 1 3"]);
    assert_eq!(messages(&r).len(), 2);
    assert_eq!(
        messages(&r)[1].1,
        "CONFLICT (modify/delete): b deleted in main and modified in side.  Version side of b left in tree."
    );
}

#[test]
fn rename_rename_one_to_two() {
    let m = Mem::default();
    let r = merge_with(
        &m,
        &opts(),
        &[("a", F::File(A))],
        &[("b", F::File(leak(with(A, "a 2", "O"))))],
        &[("c", F::File(leak(with(A, "a 2", "T"))))],
    );
    assert_eq!(paths(&m, &r), ["b", "c"]);
    assert_eq!(conflicts(&r), ["a 1", "b 2", "c 3"]);
    let content = m.content(&r, "b");
    assert_eq!(content, m.content(&r, "c"));
    assert!(content.starts_with("a 1\n<<<<<<<< main:b\nO\n========\nT\n>>>>>>>> side:c\na 3\n"));
    assert_eq!(
        messages(&r),
        [
            ("Auto-merging", "Auto-merging a".into()),
            (
                "CONFLICT (rename/rename)",
                "CONFLICT (rename/rename): a renamed to b in main and to c in side.".into()
            ),
        ]
    );
}

#[test]
fn two_renames_to_one_path_are_an_add_add() {
    let m = Mem::default();
    let b = leak(A.replace('a', "b"));
    let r = merge_with(
        &m,
        &opts(),
        &[("a", F::File(A)), ("b", F::File(b))],
        &[("b", F::File(b)), ("c", F::File(A))],
        &[("a", F::File(A)), ("c", F::File(b))],
    );
    assert_eq!(paths(&m, &r), ["c"]);
    assert_eq!(conflicts(&r), ["c 2 3"]);
    assert_eq!(messages(&r)[1].1, "CONFLICT (add/add): Merge conflict in c");
}

#[test]
fn empty_files_are_not_renamed() {
    let m = Mem::default();
    let r = merge_with(
        &m,
        &opts(),
        &[("e1", F::File(""))],
        &[("e2", F::File(""))],
        &[("e1", F::File("content\n"))],
    );
    assert_eq!(conflicts(&r), ["e1 1 3"]);
    assert_eq!(m.content(&r, "e2"), "");
}

fn dir_rename(m: &Mem, mode: DirectoryRenames) -> MergeResult {
    let b = leak(A.replace('a', "b"));
    let c = leak(A.replace('a', "c"));
    merge_with(
        m,
        &Options {
            directory_renames: mode,
            ..opts()
        },
        &[("x/a", F::File(A)), ("x/b", F::File(b))],
        &[("z/a", F::File(A)), ("z/b", F::File(b))],
        &[
            ("x/a", F::File(A)),
            ("x/b", F::File(b)),
            ("x/c", F::File(c)),
        ],
    )
}

#[test]
fn directory_renames() {
    let m = Mem::default();
    let r = dir_rename(&m, DirectoryRenames::Conflict);
    assert!(!r.clean);
    assert_eq!(paths(&m, &r), ["z/a", "z/b", "z/c"]);
    assert_eq!(conflicts(&r), ["z/c 3"]);
    assert_eq!(
        messages(&r),
        [(
            "CONFLICT (directory rename suggested)",
            "CONFLICT (file location): x/c added in side inside a directory that was renamed in main, suggesting it should perhaps be moved to z/c.".into()
        )]
    );

    let r = dir_rename(&m, DirectoryRenames::True);
    assert!(r.clean);
    assert_eq!(paths(&m, &r), ["z/a", "z/b", "z/c"]);
    assert_eq!(
        messages(&r),
        [(
            "Path updated due to directory rename",
            "Path updated: x/c added in side inside a directory that was renamed in main; moving it to z/c.".into()
        )]
    );

    let r = dir_rename(&m, DirectoryRenames::False);
    assert!(r.clean);
    assert_eq!(paths(&m, &r), ["x/c", "z/a", "z/b"]);
}

#[test]
fn directory_rename_split_and_file_in_the_way() {
    let m = Mem::default();
    let b = leak(A.replace('a', "b"));
    let c = leak(A.replace('a', "c"));
    let r = merge_with(
        &m,
        &opts(),
        &[("x/a", F::File(A)), ("x/b", F::File(b))],
        &[("y/a", F::File(A)), ("z/b", F::File(b))],
        &[
            ("x/a", F::File(A)),
            ("x/b", F::File(b)),
            ("x/c", F::File(c)),
        ],
    );
    assert!(!r.clean);
    assert!(r.conflicts.is_empty());
    assert_eq!(paths(&m, &r), ["x/c", "y/a", "z/b"]);
    assert_eq!(
        messages(&r),
        [(
            "CONFLICT(directory rename unclear split)",
            "CONFLICT (directory rename split): Unclear where to rename x to; it was renamed to multiple other directories, with no destination getting a majority of the files.".into()
        )]
    );

    let w = leak(A.replace('a', "w"));
    let r = merge_with(
        &m,
        &opts(),
        &[("x/a", F::File(A)), ("x/b", F::File(b))],
        &[("z/a", F::File(A)), ("z/b", F::File(b))],
        &[
            ("x/a", F::File(A)),
            ("x/b", F::File(b)),
            ("x/c", F::File(c)),
            ("z/c", F::File(w)),
        ],
    );
    assert!(!r.clean);
    assert_eq!(paths(&m, &r), ["x/c", "z/a", "z/b", "z/c"]);
    assert_eq!(
        messages(&r),
        [(
            "CONFLICT (file in way of directory rename)",
            "CONFLICT (implicit dir rename): Existing file/dir at z/c in the way of implicit directory rename(s) putting the following path(s) there: x/c.".into()
        )]
    );
}

fn true_dirs() -> Options {
    Options {
        directory_renames: DirectoryRenames::True,
        ..opts()
    }
}

#[test]
fn both_sides_move_a_file_to_the_same_place() {
    // Each side renames the other's directory to x/ and adds a file
    // there: the add/add conflict names the path each side gave it.
    let m = Mem::default();
    let r = merge_with(
        &m,
        &true_dirs(),
        &[("z/b", F::File("b\n")), ("y/d", F::File("d\n"))],
        &[
            ("x/b", F::File("b\n")),
            ("y/d", F::File("d\n")),
            ("y/wham", F::File("one\n")),
        ],
        &[
            ("z/b", F::File("b\n")),
            ("x/d", F::File("d\n")),
            ("z/wham", F::File("two\n")),
        ],
    );
    assert!(!r.clean);
    assert_eq!(conflicts(&r), ["x/wham 2 3"]);
    assert_eq!(
        m.content(&r, "x/wham"),
        "<<<<<<< main:y/wham\none\n=======\ntwo\n>>>>>>> side:z/wham\n"
    );
    // Our paths moved by their directory renames come first.
    let updated: Vec<String> = messages(&r)
        .into_iter()
        .filter(|(k, _)| *k == "Path updated due to directory rename")
        .map(|(_, t)| t)
        .collect();
    assert_eq!(
        updated,
        [
            "Path updated: y/wham added in main inside a directory that was renamed in side; moving it to x/wham.",
            "Path updated: z/wham added in side inside a directory that was renamed in main; moving it to x/wham.",
        ]
    );
}

#[test]
fn a_path_the_other_sides_file_moves_to_stays() {
    // main: sub1/ -> sub3/, adds sub2/new. side: sub2/ -> sub1/sub2/,
    // adds sub1/sub2/new. Our sub2/new moves to sub1/sub2/new, so their
    // file there does not move on to sub3/: an add/add (t6423 12l).
    let m = Mem::default();
    let r = merge_with(
        &m,
        &true_dirs(),
        &[
            ("sub1/file", F::File("file\n")),
            ("sub2/other", F::File("other\n")),
        ],
        &[
            ("sub3/file", F::File("file\n")),
            ("sub2/other", F::File("other\n")),
            ("sub2/new", F::File("ours\n")),
        ],
        &[
            ("sub1/file", F::File("file\n")),
            ("sub1/newfile", F::File("brand\n")),
            ("sub1/sub2/other", F::File("other\n")),
            ("sub1/sub2/new", F::File("theirs\n")),
        ],
    );
    assert!(!r.clean);
    assert_eq!(conflicts(&r), ["sub1/sub2/new 2 3"]);
    assert_eq!(
        paths(&m, &r),
        [
            "sub1/sub2/new",
            "sub3/file",
            "sub3/newfile",
            "sub3/sub2/other"
        ]
    );
}

#[test]
fn rename_moved_back_onto_a_deleted_file() {
    // side: world -> tools/world. main: tools/ -> top level, deletes
    // world. The directory rename takes the rename back; git reports a
    // modify/delete (t6423 12n2).
    let m = Mem::default();
    let r = merge_with(
        &m,
        &true_dirs(),
        &[
            ("tools/hello", F::File("hello\n")),
            ("world", F::File("world\n")),
        ],
        &[("hello", F::File("hello\n"))],
        &[
            ("tools/hello", F::File("hello\n")),
            ("tools/world", F::File("world\n")),
        ],
    );
    assert!(!r.clean);
    assert_eq!(conflicts(&r), ["world 1 3"]);
    assert_eq!(paths(&m, &r), ["hello", "world"]);
    assert_eq!(
        messages(&r)[1],
        (
            "CONFLICT (modify/delete)",
            "CONFLICT (modify/delete): world deleted in main and modified in side.  Version side of world left in tree.".into()
        )
    );
}

#[test]
fn binary_rename_rename_keeps_both_versions() {
    let m = Mem::default();
    let r = merge_with(
        &m,
        &opts(),
        &[("orig", F::Binary("1\n2\n3\n"))],
        &[("orig-A", F::Binary("1\n2\n3\n4\n"))],
        &[("orig-B", F::Binary("0\n1\n2\n3\n"))],
    );
    assert!(!r.clean);
    assert_eq!(conflicts(&r), ["orig 1", "orig-A 2", "orig-B 3"]);
    assert_eq!(m.content(&r, "orig-A"), "1\n2\n3\n4\n\0");
    assert_eq!(m.content(&r, "orig-B"), "0\n1\n2\n3\n\0");
    assert_eq!(
        messages(&r)[0].1,
        "warning: Cannot merge binary files: orig (main:orig-A vs. side:orig-B)"
    );
}

#[test]
fn duplicate_tree_entries_fail_the_merge() {
    use crate::Objects;
    use rgit_object::{EntryMode, ObjectKind, Tree, TreeEntry};
    let m = Mem::default();
    let base = m.tree(&[("docs", F::Gitlink(1)), ("file", F::File("base\n"))]);
    let ours = m.tree(&[
        ("docs/req", F::File("hello\n")),
        ("file", F::File("base\n")),
    ]);
    let sub = m.tree(&[("req", F::File("world\n"))]);
    let blob = m.write(ObjectKind::Blob, b"base\n").unwrap();
    let gitlink = rgit_hash::ObjectId::from_bytes(rgit_hash::HashKind::Sha1, &[1; 20]).unwrap();
    let raw = Tree {
        entries: vec![
            TreeEntry::new(EntryMode::from_bits(0o160000), "docs", gitlink),
            TreeEntry::new(EntryMode::TREE, "docs", sub),
            TreeEntry::new(EntryMode::from_bits(0o100644), "file", blob),
        ],
    };
    let theirs = m.write(ObjectKind::Tree, &raw.serialize()).unwrap();
    let e = merge_trees(&m, &base, &ours, &theirs, &opts()).unwrap_err();
    assert_eq!(
        e.collect_errors().unwrap()[0],
        "tree has duplicate entries for 'docs'"
    );
}
