// SPDX-License-Identifier: MIT
//! The empty tree exists in every repository without being stored, and
//! `hash-object` reads character devices such as `/dev/null` (t1060).

use rgit_difftest::{Check, Fixture};

const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

#[test]
fn empty_tree_is_always_there() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "r"]);
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["cat-file", "-t", EMPTY_TREE][..],
        &["cat-file", "-s", EMPTY_TREE],
        &["cat-file", "-e", EMPTY_TREE],
        &["cat-file", "-p", EMPTY_TREE],
        &["ls-tree", EMPTY_TREE],
        &["hash-object", "-t", "tree", "/dev/null"],
        &["hash-object", "/dev/null"],
    ] {
        fx.compare(args.iter().copied())
            .in_dir("r")
            .checks(Check::OUTPUT)
            .run();
    }
    fx.compare(["cat-file", "--batch-check"])
        .in_dir("r")
        .stdin(format!("{EMPTY_TREE}\n"))
        .checks(Check::OUTPUT)
        .run();
}
