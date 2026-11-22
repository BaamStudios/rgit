// SPDX-License-Identifier: MIT
//! Differential tests of `diff <blob> <blob>`, `diff <blob> <file>`,
//! `--merge-base` for `diff`, `diff-index` and `diff-tree`, the argument
//! checks of the diff front ends and `diff.autoRefreshIndex`.

use rgit_difftest::{Check, Fixture};

fn compare_all(fx: &mut Fixture, dir: &str, cmds: &[&[&str]]) {
    let mut failures = Vec::new();
    for argv in cmds {
        if let Err(m) = fx
            .compare(*argv)
            .in_dir(dir)
            .checks(Check::OUTPUT)
            .try_run()
        {
            failures.push(format!("{argv:?}\n{}", m.report()));
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

fn blobs() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a", "one\n")
        .write_executable("x", "run\n")
        .write("d/q", "q\n")
        .symlink("a", "l")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "first"])
        .tick()
        .write("a", "two\n")
        .git(["commit", "-q", "-a", "-m", "second"])
        .write("work", "two\nthree\n")
}

#[test]
fn blob_against_blob() {
    let mut fx = blobs();
    let old = String::from_utf8(fx.git_stdout(["rev-parse", "HEAD^:a"])).unwrap();
    let old = old.trim();
    let short = &old[..7];
    let cmds: &[&[&str]] = &[
        &["diff", "HEAD^:a", "HEAD:a"],
        &["diff", old, "HEAD:a"],
        &["diff", short, "HEAD:a"],
        &["diff", "HEAD:a", "HEAD:x"],
        &["diff", "HEAD:d/q", "HEAD:a"],
        &["diff", "HEAD:l", "HEAD:a"],
        &["diff", ":a", "HEAD^:a"],
        &["diff", "--raw", "HEAD:d/q", "HEAD:a"],
        &["diff", "--name-status", "HEAD:d/q", "HEAD:a"],
        &["diff", "--name-only", "HEAD:d/q", "HEAD:a"],
        &["diff", "--stat", "HEAD:d/q", "HEAD:a"],
        &["diff", "--numstat", "HEAD:d/q", "HEAD:a"],
        &["diff", "-R", "HEAD:d/q", "HEAD:a"],
        &["diff", "-R", "--name-only", "HEAD:d/q", "HEAD:a"],
        &["diff", "--exit-code", "HEAD^:a", "HEAD:a"],
        &["diff", "--exit-code", "HEAD:a", "HEAD:a"],
        &["diff", "--full-index", "HEAD^:a..HEAD:x"],
        &["diff", "HEAD:a", "HEAD^:a", "--"],
        // Usage errors and refusals.
        &["diff", "HEAD:a"],
        &["diff", "HEAD:a", "HEAD"],
        &["diff", "HEAD", "HEAD:a"],
        &["diff", "HEAD:a", "HEAD:a", "HEAD:x"],
        &["diff", "HEAD^:a", "HEAD:a", "--", "a"],
        &["diff", "--cached", "HEAD^:a", "HEAD:a"],
        &["diff", "HEAD:nope", "HEAD:a"],
    ];
    compare_all(&mut fx, "", cmds);
    compare_all(&mut fx, "d", &[&["diff", "HEAD:./q", "HEAD:../a"]]);
}

#[test]
fn blob_against_file() {
    let mut fx = blobs();
    let old = String::from_utf8(fx.git_stdout(["rev-parse", "HEAD:a"])).unwrap();
    let cmds: &[&[&str]] = &[
        &["diff", "HEAD:a", "work"],
        &["diff", "--full-index", "HEAD:a", "x"],
        &["diff", old.trim(), "work"],
        &["diff", "--stat", "HEAD:a", "work"],
    ];
    compare_all(&mut fx, "", cmds);
}

/// main: A - C (- dirty work tree), side: A - B, lone: an unrelated root.
fn branches() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a", "a\n")
        .git(["add", "a"])
        .git(["commit", "-q", "-m", "A"])
        .git(["checkout", "-q", "-b", "side"])
        .tick()
        .write("b", "b\n")
        .git(["add", "b"])
        .git(["commit", "-q", "-m", "B"])
        .git(["tag", "-m", "t", "annotated"])
        .git(["checkout", "-q", "main"])
        .tick()
        .write("c", "c\n")
        .git(["add", "c"])
        .git(["commit", "-q", "-m", "C"])
        .git(["checkout", "-q", "--orphan", "lone"])
        .git(["commit", "-q", "-m", "L"])
        .git(["checkout", "-q", "main"])
        .git(["branch", "crossA", "side"])
        .git(["branch", "crossB", "main"])
        .git(["checkout", "-q", "crossA"])
        .git(["merge", "-q", "--no-edit", "main"])
        .git(["checkout", "-q", "crossB"])
        .git(["merge", "-q", "--no-edit", "side"])
        .git(["checkout", "-q", "main"])
        .write("a", "a\ndirty\n")
}

#[test]
fn merge_base() {
    let mut fx = branches();
    let cmds: &[&[&str]] = &[
        &["diff", "--merge-base"],
        &["diff", "--stat", "--merge-base", "side"],
        &["diff", "--stat", "--merge-base", "annotated"],
        &["diff", "--stat", "--cached", "--merge-base", "side"],
        &["diff", "--stat", "--merge-base", "side", "main"],
        &["diff", "--merge-base", "main^{tree}"],
        &["diff", "--merge-base", "side", "main^{tree}"],
        &["diff", "--merge-base", "lone"],
        &["diff", "--merge-base", "side", "lone"],
        &["diff", "--merge-base", "crossA", "crossB"],
        &["diff", "--merge-base", "side..main"],
        &["diff", "--merge-base", "side", "main", "lone"],
        &["diff", "--merge-base", "HEAD:a", "HEAD:c"],
        &["diff", "--stat", "--merge-base", "side", "--", "a"],
        &["diff-index", "--merge-base"],
        &["diff-index", "--merge-base", "side"],
        &["diff-index", "--cached", "--merge-base", "side"],
        &["diff-index", "--merge-base", "side", "main"],
        &["diff-index", "--merge-base", "main^{tree}"],
        &["diff-index", "--merge-base", "lone"],
        &["diff-index", "--merge-base", "side..main"],
        &["diff-index", "--merge-base", "side", "a"],
        &["diff-tree", "--merge-base"],
        &["diff-tree", "--merge-base", "side"],
        &["diff-tree", "--merge-base", "side", "main"],
        &["diff-tree", "-r", "--merge-base", "main", "side"],
        &["diff-tree", "--merge-base", "side", "main", "a"],
        &["diff-tree", "--merge-base", "side", "main^{tree}"],
        &["diff-tree", "--merge-base", "side", "lone"],
        &["diff-tree", "--merge-base", "crossA", "crossB"],
        &["diff-tree", "--merge-base", "side", "main", "lone"],
        &["diff-tree", "--merge-base", "side..main"],
        &["diff-tree", "--merge-base", "--stdin"],
    ];
    compare_all(&mut fx, "", cmds);
}

#[test]
fn revision_and_path_arguments() {
    let mut fx = branches().write("d/q", "q\n").git(["add", "d"]);
    let cmds: &[&[&str]] = &[
        &["diff", "side...main", "crossA...crossB"],
        &["diff", "main", "side...main"],
        &["diff", "main", "side..main", "crossA..crossB"],
        &["diff", "side..main", "crossA"],
        &["diff", "--stat", "side..main", "--", "c"],
        &["diff", "--stat", "HEAD", "*.c"],
        &["diff", "--stat", "HEAD", ":!a"],
        &["diff-index", "HEAD", "nosuch"],
        &["diff-index", "HEAD", "a"],
        &["diff-index", "HEAD", ":!a"],
        &["diff-index", "HEAD", "d/*"],
        &["diff-index", "HEAD", "a", "nosuch"],
        &["diff-index", "HEAD", "--", "nosuch"],
        &["diff-index", "--cached", "HEAD", "nosuch"],
        &["diff-index", "side", "main"],
    ];
    compare_all(&mut fx, "", cmds);
    compare_all(
        &mut fx,
        "d",
        &[
            &["diff-index", "HEAD", "../a"],
            &["diff-index", "HEAD", "q"],
        ],
    );
}

/// `git diff` writes refreshed stat data back to the index; `diff HEAD`,
/// `diff --cached` and `diff.autoRefreshIndex=false` do not.
#[test]
fn auto_refresh_index() {
    use std::path::Path;
    use std::time::{Duration, SystemTime};
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a", "a\n")
        .write("b", "b\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "A"]);
    if fx.is_skipped() {
        return;
    }
    let work = fx.work_dir();
    let index = work.join(".git/index");
    let set = |p: &Path, t: SystemTime| {
        std::fs::File::options()
            .write(true)
            .open(p)
            .and_then(|f| f.set_modified(t))
            .expect("set mtime");
    };
    let mtime = |p: &Path| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .expect("mtime")
    };
    let run = |args: &[&str]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_rgit"))
            .args(args)
            .current_dir(&work)
            .env("HOME", fx.home_dir())
            .env("XDG_CONFIG_HOME", fx.home_dir().join(".config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("run rgit");
        assert!(
            out.status.code().is_some_and(|c| c <= 1),
            "{args:?}: {out:?}"
        );
    };
    let old = SystemTime::now() - Duration::from_secs(7200);
    let touched = SystemTime::now() - Duration::from_secs(3600);
    let cases: [(&[&str], bool); 5] = [
        (&["diff", "HEAD"], false),
        (&["diff", "--cached"], false),
        (&["-c", "diff.autoRefreshIndex=false", "diff"], false),
        (&["diff-files"], false),
        (&["diff"], true),
    ];
    for (args, writes) in cases {
        set(&index, old);
        set(&work.join("a"), touched);
        run(args);
        assert_eq!(mtime(&index) != old, writes, "{args:?}");
    }
    // Once everything is refreshed, nothing is written.
    set(&work.join("b"), touched);
    run(&["diff"]);
    let recent = SystemTime::now() - Duration::from_secs(60);
    set(&index, recent);
    run(&["diff"]);
    assert_eq!(mtime(&index), recent);
}
