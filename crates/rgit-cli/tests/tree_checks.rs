// SPDX-License-Identifier: MIT
//! Differential tests of checks in `read-tree`, `write-tree` and
//! `commit-tree`: the cache tree `write-tree` records, null object ids,
//! trees with duplicate names and `read-tree --quiet`.

use rgit_difftest::{Check, Fixture};

const NULL: &str = "0000000000000000000000000000000000000000";

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a", "a\n")
        .write("d/c", "c\n")
        .write("d/e/f", "f\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "init"])
}

fn tree_from(fx: &Fixture, listing: &str) -> String {
    let path = fx.work_dir().join("listing");
    std::fs::write(&path, listing).expect("write listing");
    let out = std::process::Command::new(fx.reference_git())
        .args(["mktree"])
        .current_dir(fx.work_dir())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", fx.home_dir())
        .env("XDG_CONFIG_HOME", fx.home_dir().join(".config"))
        .stdin(std::fs::File::open(&path).expect("open listing"))
        .output()
        .expect("run git mktree");
    std::fs::remove_file(&path).expect("remove listing");
    String::from_utf8(out.stdout)
        .expect("utf-8")
        .trim()
        .to_owned()
}

/// `write-tree` records the trees it wrote in the index's cache tree.
#[test]
fn write_tree_records_cache_tree() {
    let mut fx = fixture()
        .tick()
        .write("d/e/g", "g\n")
        .write("b", "b\n")
        .git(["update-index", "--add", "d/e/g", "b"]);
    fx.compare(["write-tree"])
        .checks([
            Check::ExitCode,
            Check::Stdout,
            Check::Stderr,
            Check::Objects,
            Check::GitFiles,
        ])
        .git_file("index")
        .run();
    // A prefix leaves the index alone.
    fx.compare(["write-tree", "--prefix=d/"])
        .checks([
            Check::ExitCode,
            Check::Stdout,
            Check::Stderr,
            Check::GitFiles,
        ])
        .git_file("index")
        .run();
}

/// Index entries with a null object id: `read-tree` refuses to write
/// them unless `GIT_ALLOW_NULL_SHA1` is set, `write-tree` refuses them.
#[test]
fn null_object_ids() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let tree = tree_from(&fx, &format!("160000 commit {NULL}\tbroken\n"));
    fx.compare(["read-tree", &tree]).checks(Check::ALL).run();
    fx.compare(["read-tree", &tree])
        .env("GIT_ALLOW_NULL_SHA1", "1")
        .checks(Check::ALL)
        .run();
    let mut fx = fixture().env("GIT_ALLOW_NULL_SHA1", "1").git([
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{NULL},broken"),
    ]);
    fx.compare(["write-tree"]).checks(Check::OUTPUT).run();
}

/// A tree with a duplicate name: git's cache tree check (on in its test
/// suite) reports it when the tree is read into the index.
#[test]
fn duplicate_tree_entries() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let blob = String::from_utf8(fx.git_stdout(["rev-parse", "HEAD:a"])).unwrap();
    let blob = blob.trim();
    let tree = tree_from(
        &fx,
        &format!("100644 blob {blob}\tx\n100644 blob {blob}\tx\n"),
    );
    for args in [
        vec!["read-tree", tree.as_str()],
        vec!["read-tree", "--reset", tree.as_str()],
    ] {
        fx.compare(&args).checks(Check::ALL).run();
        fx.compare(&args)
            .env("GIT_TEST_CHECK_CACHE_TREE", "true")
            .checks(Check::ALL)
            .run();
    }
}

/// `read-tree --quiet` refuses without the messages.
#[test]
fn read_tree_quiet() {
    let mut fx = fixture()
        .git(["checkout", "-q", "-b", "other"])
        .tick()
        .write("a", "other\n")
        .git(["commit", "-q", "-a", "-m", "other"])
        .git(["checkout", "-q", "main"])
        .write("a", "dirty\n");
    fx.compare(["read-tree", "-m", "-u", "HEAD", "other"])
        .checks(Check::ALL)
        .run();
    fx.compare(["read-tree", "--quiet", "-m", "-u", "HEAD", "other"])
        .checks(Check::ALL)
        .run();
}
