// SPDX-License-Identifier: MIT
//! Differential tests of merges with rename detection (`merge-tree
//! --write-tree` and `merge`) against the reference git: renames with
//! changes on the other side, rename/delete, rename/rename, rename/add,
//! directory renames and the rename options.

use rgit_difftest::{Check, Fixture};

fn lines(tag: &str) -> String {
    (1..=10).map(|i| format!("{tag} {i}\n")).collect()
}

/// `main` and `side` both start from `base` and rename files in every way
/// that leads to a conflict once; `clean` renames without conflicts.
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("moved", lines("moved"))
        .write("deleted", lines("deleted"))
        .write("twice", lines("twice"))
        .write("both", lines("both"))
        .write("added", lines("added"))
        .write("x/a", lines("xa"))
        .write("x/b", lines("xb"))
        .write("empty", "")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "base"])
        .tick()
        .git(["checkout", "-q", "-b", "side"])
        .write("moved", lines("moved").replace("moved 5", "side"))
        .remove("deleted")
        .git(["mv", "twice", "twice-side"])
        .git(["mv", "both", "both-new"])
        .write("both-new", lines("both").replace("both 2", "side"))
        .write("added-new", lines("other"))
        .write("x/c", lines("xc"))
        .write("empty", "now full\n")
        .git(["add", "-A"])
        .git(["commit", "-q", "-m", "side"])
        .tick()
        .git(["checkout", "-q", "-b", "clean", "main"])
        .git(["mv", "moved", "moved-clean"])
        .git(["commit", "-q", "-m", "clean"])
        .tick()
        .git(["checkout", "-q", "main"])
        .git(["mv", "moved", "moved-main"])
        .write("moved-main", lines("moved").replace("moved 5", "main"))
        .git(["mv", "deleted", "deleted-main"])
        .git(["mv", "twice", "twice-main"])
        .git(["mv", "both", "both-new"])
        .write("both-new", lines("both").replace("both 9", "main"))
        .git(["mv", "added", "added-new"])
        .git(["mv", "x", "z"])
        .git(["mv", "empty", "empty2"])
        .git(["commit", "-q", "-m", "main"])
}

#[test]
fn merge_tree_with_renames() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["merge-tree", "main", "side"],
        &["merge-tree", "side", "main"],
        &["merge-tree", "-z", "main", "side"],
        &["merge-tree", "--name-only", "main", "side"],
        &["merge-tree", "main", "clean"],
        &["merge-tree", "side", "clean"],
        &["merge-tree", "-Xno-renames", "main", "side"],
        &["merge-tree", "-Xfind-renames=95%", "main", "side"],
        &["merge-tree", "-Xrename-threshold=20", "main", "side"],
        &["-c", "merge.renames=false", "merge-tree", "main", "side"],
        &["-c", "diff.renames=false", "merge-tree", "main", "side"],
        &[
            "-c",
            "merge.directoryRenames=true",
            "merge-tree",
            "main",
            "side",
        ],
        &[
            "-c",
            "merge.directoryRenames=false",
            "merge-tree",
            "side",
            "main",
        ],
        &["-c", "merge.renameLimit=1", "merge-tree", "main", "side"],
    ];
    for args in cases {
        fx.compare(*args).checks(Check::OUTPUT).run();
    }
    for style in ["merge", "diff3"] {
        let config = format!("merge.conflictStyle={style}");
        let argv = ["-c", config.as_str(), "merge-tree", "main", "side"];
        let tree = String::from_utf8(fx.git_output(argv).stdout).unwrap();
        let tree = tree.lines().next().unwrap().to_owned();
        for path in ["moved-main", "both-new", "twice-main"] {
            fx.compare(["cat-file", "-p", &format!("{tree}:{path}")])
                .checks(Check::OUTPUT)
                .run();
        }
        fx.compare(argv).checks(Check::OUTPUT).run();
    }
}

#[test]
fn merge_with_renames() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    // Everything but the object set: the rename detection writes the
    // rewritten input trees, which git keeps in memory.
    let checks: Vec<Check> = Check::ALL
        .into_iter()
        .filter(|c| *c != Check::Objects)
        .collect();
    fx.compare(["merge", "side"]).checks(checks).run();
    fx.compare(["ls-files", "-s"]).checks(Check::OUTPUT).run();
    fx.compare(["status", "--porcelain"])
        .checks(Check::OUTPUT)
        .run();
}
