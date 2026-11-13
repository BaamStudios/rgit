// SPDX-License-Identifier: MIT
//! Differential tests of `rgit read-tree -m`/`--reset` (one- and two-way
//! merges, with and without `-u`) against the reference git: messages,
//! exit code, the resulting index and the work tree.

use std::path::Path;
use std::process::Command;

use rgit_difftest::{Check, Fixture};
use rgit_hash::HashKind;
use rgit_index::{Index, ReadOptions};

const ALL: [Check; 5] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Index,
    Check::Worktree,
];

/// Branch `old` has a, b, c, d/x, f, g/g; `main` changes a, removes b,
/// adds n, turns f into a directory (f/g), adds e/e and turns d into a
/// file. `old` is checked out.
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a", "a\n")
        .write("b", "b\n")
        .write("c", "c\n")
        .write("d/x", "x\n")
        .write("f", "f\n")
        .write("g/g", "g\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "old"])
        .git(["branch", "old"])
        .tick()
        .write("a", "a2\n")
        .remove("b")
        .write("n", "n\n")
        .remove("f")
        .write("f/g", "ff\n")
        .write("e/e", "e\n")
        .remove("d")
        .write("d", "d\n")
        .git(["add", "-A"])
        .git(["commit", "-q", "-m", "main"])
        .git(["checkout", "-q", "old"])
        .tick()
}

/// Compares `read-tree <args>`. The fixture runs each side in a copy of
/// the work tree, which changes inode numbers and ctimes; with
/// `core.checkStat=minimal` and `core.trustCtime=false` the copied files
/// still match their index entries, as in a real checkout.
fn run(fx: Fixture, args: &[&str]) {
    let mut fx = fx;
    let mut argv = vec![
        "-c",
        "core.checkStat=minimal",
        "-c",
        "core.trustCtime=false",
        "read-tree",
    ];
    argv.extend_from_slice(args);
    fx.compare(&argv).checks(ALL).run();
}

/// Like [`run`] with the default stat checks: every tracked file of the
/// copy looks touched, which plumbing treats as "not uptodate".
fn run_stat_dirty(fx: Fixture, args: &[&str]) {
    let mut fx = fx;
    let mut argv = vec!["read-tree"];
    argv.extend_from_slice(args);
    fx.compare(&argv).checks(ALL).run();
}

const TWO_WAY_U: &[&str] = &["-m", "-u", "old", "main"];
const TWO_WAY: &[&str] = &["-m", "old", "main"];

#[test]
fn clean_two_way() {
    run_stat_dirty(fixture(), TWO_WAY_U);
    run_stat_dirty(fixture(), &["-m", "-u", "main"]);
    run_stat_dirty(fixture(), &["--reset", "-u", "main"]);
    run(fixture(), TWO_WAY_U);
    run(fixture(), TWO_WAY);
    run(fixture(), &["-n", "-m", "-u", "old", "main"]);
    run(fixture(), &["--reset", "-u", "old", "main"]);
}

#[test]
fn local_changes() {
    for args in [
        TWO_WAY_U,
        TWO_WAY,
        &["-n", "-m", "-u", "old", "main"][..],
        &["-i", "-m", "old", "main"],
    ] {
        // case 21: dirty file that the merge changes
        run(fixture().write("a", "dirty\n"), args);
        // case 15: dirty file both trees agree on
        run(fixture().write("c", "dirty\n"), args);
        // case 16: staged change
        run(fixture().write("a", "st\n").git(["add", "a"]), args);
        // case 12: staged change of a removed file
        run(fixture().write("b", "st\n").git(["add", "b"]), args);
        // case 8: staged addition differing from the new tree
        run(fixture().write("n", "st\n").git(["add", "n"]), args);
        // cases 6 and 18: staged state equals the new tree
        run(fixture().write("n", "n\n").git(["add", "n"]), args);
        run(fixture().write("a", "a2\n").git(["add", "a"]), args);
        // case 4: staged addition outside both trees
        run(fixture().write("q", "q\n").git(["add", "q"]), args);
        // case 3: staged removal, trees agree / differ
        run(fixture().git(["rm", "-q", "--cached", "c"]), args);
        run(fixture().git(["rm", "-q", "--cached", "a"]), args);
        // a deleted work tree file is not a local change
        run(fixture().remove("a"), args);
        run(fixture().remove("c"), args);
    }
}

#[test]
fn reset_two_way() {
    let args = &["--reset", "-u", "old", "main"];
    run(fixture().write("a", "st\n").git(["add", "a"]), args);
    run(fixture().write("a", "dirty\n"), args);
    run(
        fixture().write("c", "dirty\n").write("n", "untracked\n"),
        args,
    );
}

#[test]
fn untracked_files() {
    run(fixture().write("n", "untracked\n"), TWO_WAY_U);
    run(fixture().write("n", "n\n"), TWO_WAY_U);
    run(fixture().write("n", "untracked\n"), TWO_WAY);
    // a file where the new tree needs a directory
    run(fixture().write("e", "e\n"), TWO_WAY_U);
    // a directory that becomes a file still holds an untracked file
    run(fixture().write("d/u", "u\n"), TWO_WAY_U);
    run(fixture().write("d/x", "m\n"), TWO_WAY_U);
    // ignored files are overwritten
    run(
        fixture()
            .write(".git/info/exclude", "n\n")
            .write("n", "nn\n"),
        TWO_WAY_U,
    );
    run(
        fixture().write(".gitignore", "n\n").write("n", "nn\n"),
        TWO_WAY_U,
    );
    run(
        fixture()
            .write(".git/info/exclude", "u\n")
            .write("d/u", "u\n"),
        TWO_WAY_U,
    );
    run(
        fixture()
            .write(".git/info/exclude", "e\n")
            .write("e", "e\n"),
        TWO_WAY_U,
    );
    // staged changes below a directory that becomes a file (git drops them)
    run(
        fixture().write("d/x", "st\n").git(["add", "d/x"]),
        TWO_WAY_U,
    );
    run(fixture().write("d/x", "st\n").git(["add", "d/x"]), TWO_WAY);
    // a staged file where the new tree needs a directory
    run(fixture().write("e", "st\n").git(["add", "e"]), TWO_WAY_U);
}

#[cfg(unix)]
#[test]
fn symlink_in_the_way() {
    run(
        fixture()
            .write("elsewhere/k", "k\n")
            .symlink("elsewhere", "e"),
        TWO_WAY_U,
    );
}

#[test]
fn one_way() {
    for args in [
        &["-m", "-u", "main"][..],
        &["-m", "main"],
        &["--reset", "-u", "main"],
        &["--reset", "main"],
    ] {
        run(fixture(), args);
        run(fixture().write("a", "dirty\n"), args);
        run(fixture().write("c", "dirty\n"), args);
        run(fixture().write("n", "untracked\n"), args);
        run(fixture().write("d/u", "u\n"), args);
        run(fixture().write("q", "q\n").git(["add", "q"]), args);
        run(fixture().remove("c"), args);
    }
}

#[test]
fn initial_checkout() {
    run(fixture().remove(".git/index"), TWO_WAY);
    run(fixture().remove(".git/index"), TWO_WAY_U);
    run(fixture().git(["read-tree", "--empty"]), TWO_WAY);
}

/// `git update-index --index-info` with `info` on stdin, in the fixture.
fn index_info(fx: &Fixture, info: &str) {
    use std::io::Write;
    let mut child = Command::new(fx.reference_git())
        .args(["update-index", "--index-info"])
        .current_dir(fx.work_dir())
        .env("HOME", fx.home_dir())
        .env("XDG_CONFIG_HOME", fx.home_dir())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(info.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
}

#[test]
fn unmerged_index() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let blob = String::from_utf8(fx.git_stdout(["rev-parse", ":b"])).unwrap();
    let blob = blob.trim();
    let info = format!(
        "0 {zero}\ta\n100644 {blob} 1\ta\n100644 {blob} 3\ta\n",
        zero = "0".repeat(40)
    );
    index_info(&fx, &info);
    for args in [
        &["-m", "main"][..],
        &["-m", "old", "main"],
        &["--reset", "-u", "old", "main"],
        &["--reset", "-u", "main"],
        &["--reset", "-u", "old"],
    ] {
        let mut argv = vec!["read-tree"];
        argv.extend_from_slice(args);
        fx.compare(&argv).checks(ALL).run();
        // restore the conflict for the next case
        index_info(&fx, &info);
    }
}

#[test]
fn option_errors() {
    for args in [
        &["--exclude-per-directory=.gitignore", "-m", "old"][..],
        &["-u", "-i", "old"],
        &["-u", "old"],
        &["-i", "old"],
        &["-m", "--reset", "old"],
        &["--reset"],
        &["-m"],
        &["-m", "nosuch"],
        &["-u", "nosuch"],
        &["--empty", "old"],
        &["-u"],
        &["-m", "-u", "--exclude-per-directory=x", "old"],
        &[
            "-m",
            "-u",
            "--exclude-per-directory=.gitignore",
            "old",
            "main",
        ],
        &["--prefix=x/", "-m", "old"],
        &[
            "-m", "old", "old", "old", "old", "old", "old", "old", "old", "old",
        ],
        &["-v", "-m", "-u", "--aggressive", "old", "main"],
    ] {
        run(fixture(), args);
    }
}

#[test]
fn index_output() {
    let mut fx = fixture();
    fx.compare([
        "read-tree",
        "-m",
        "--index-output=.git/other",
        "old",
        "main",
    ])
    .checks(ALL)
    .git_file("other")
    .run();
}

/// A tree with one entry `name` (mode `mode`) holding blob `blob`, written
/// literally.
fn literal_tree(fx: &Fixture, name: &[u8], mode: &str, id_hex: &str) -> String {
    let mut raw = format!("{mode} ").into_bytes();
    raw.extend_from_slice(name);
    raw.push(0);
    raw.extend(
        (0..id_hex.len() / 2).map(|i| u8::from_str_radix(&id_hex[2 * i..2 * i + 2], 16).unwrap()),
    );
    std::fs::write(fx.work_dir().join(".git/raw-tree"), raw).unwrap();
    let out = fx.git_stdout([
        "hash-object",
        "-t",
        "tree",
        "--literally",
        "-w",
        ".git/raw-tree",
    ]);
    String::from_utf8(out).unwrap().trim().to_string()
}

#[test]
fn invalid_paths() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let blob = String::from_utf8(fx.git_stdout(["rev-parse", "old:a"]))
        .unwrap()
        .trim()
        .to_string();
    let config = literal_tree(&fx, b"config", "100644", &blob);
    let git = literal_tree(&fx, b".git", "40000", &config);
    let x = literal_tree(&fx, b"x", "100644", &blob);
    let upper = literal_tree(&fx, b".GIT", "40000", &x);
    let dots = literal_tree(&fx, b"..", "40000", &x);
    let dotdot = literal_tree(&fx, b"a", "40000", &dots);
    let short = literal_tree(&fx, b"GIT~1", "100644", &blob);
    let ntfs = literal_tree(&fx, b".git. ", "100644", &blob);
    let gitmodules = literal_tree(&fx, b".gitmodules", "120000", &blob);
    let mut fx = fx;
    for t in [&git, &upper, &dotdot, &short, &ntfs, &gitmodules] {
        for args in [
            &[t.as_str()][..],
            &["-m", t],
            &["-m", "-u", t],
            &["--reset", "-u", t],
        ] {
            let mut argv = vec!["read-tree"];
            argv.extend_from_slice(args);
            fx.compare(&argv).checks(ALL).run();
        }
    }
    // core.protectNTFS=false allows the NTFS-only names
    fx.compare(["-c", "core.protectNTFS=false", "read-tree", &short])
        .checks(ALL)
        .run();
}

/// Runs `args` with the reference git in one fixture and with rgit in a
/// second, identical one, and compares the cache trees of the resulting
/// index files.
fn same_cache_tree(setup: impl Fn() -> Fixture, args: &[&str]) {
    let git_fx = setup();
    if git_fx.is_skipped() {
        return;
    }
    let git_fx = git_fx.git(args);
    let rgit_fx = setup();
    let status = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(args)
        .current_dir(rgit_fx.work_dir())
        .env("HOME", rgit_fx.home_dir())
        .env("XDG_CONFIG_HOME", rgit_fx.home_dir())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success());
    let read = |dir: &Path| {
        Index::read_file(
            dir.join(".git/index"),
            HashKind::Sha1,
            &ReadOptions::default(),
        )
        .unwrap()
        .cache_tree()
        .cloned()
    };
    assert_eq!(
        read(&git_fx.work_dir()),
        read(&rgit_fx.work_dir()),
        "{args:?}"
    );
}

#[test]
fn cache_tree() {
    same_cache_tree(fixture, &["read-tree", "-m", "-u", "old", "main"]);
    same_cache_tree(
        || fixture().write("c", "st\n").git(["add", "c"]),
        &["read-tree", "-m", "-u", "old", "main"],
    );
    same_cache_tree(
        || fixture().write("g/g", "st\n").git(["add", "g/g"]),
        &["read-tree", "-m", "-u", "old", "main"],
    );
    same_cache_tree(
        || fixture().write("h/h", "h\n").git(["add", "h/h"]),
        &["read-tree", "-m", "old", "main"],
    );
    same_cache_tree(
        || fixture().write("q", "q\n").git(["add", "q"]),
        &["read-tree", "-m", "-u", "main"],
    );
    same_cache_tree(fixture, &["read-tree", "--reset", "-u", "main"]);
    same_cache_tree(fixture, &["read-tree", "main"]);
}
