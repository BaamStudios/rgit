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
