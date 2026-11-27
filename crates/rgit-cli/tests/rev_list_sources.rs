// SPDX-License-Identifier: MIT
//! Differential tests of the `rev-list` sources and output options
//! (`--reflog`, `--indexed-objects`, `--exclude-hidden`, `--unpacked`,
//! `--disk-usage`, `--maximal-only`, ...) against the reference git.

use rgit_difftest::{Check, Fixture};

fn commit(fx: Fixture, name: &str) -> Fixture {
    fx.write(name, name)
        .git(["add", name])
        .git(["commit", "-q", "-m", name])
        .tick()
}

/// A merge of a side branch, a rewritten commit (only in the reflog),
/// packed and loose objects, a hidden ref and a staged change.
fn history() -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"]);
    fx = commit(fx, "a");
    fx = commit(fx, "b");
    fx = fx.git(["checkout", "-q", "-b", "side"]);
    fx = commit(fx, "s1");
    fx = commit(fx, "s2");
    fx = fx.git(["checkout", "-q", "main"]);
    fx = commit(fx, "m1");
    fx = fx
        .git(["merge", "-q", "--no-ff", "-m", "merge", "side"])
        .tick();
    fx = commit(fx, "m2");
    fx = fx.git(["reset", "-q", "--hard", "HEAD~1"]);
    fx = commit(fx, "m3");
    fx = fx.git(["repack", "-q", "-a", "-d"]);
    fx = commit(fx, "loose");
    fx.git(["update-ref", "refs/hidden/x", "HEAD~2"])
        .git(["update-ref", "refs/tags/t", "HEAD~3"])
        .write("a", "changed")
        .git(["add", "a"])
}

#[test]
fn sources_and_output_options() {
    let mut fx = history();
    for args in [
        &["rev-list", "--reflog"][..],
        &["rev-list", "HEAD", "--not", "--reflog"],
        &["rev-list", "--all", "--single-worktree"],
        &["rev-list", "--ignore-missing", "nonexist", "HEAD~1"],
        &[
            "rev-list",
            "--ignore-missing",
            "0000000000000000000000000000000000000000",
        ],
        &["rev-list", "--objects", "--indexed-objects"],
        &[
            "rev-list",
            "-1",
            "--objects",
            "HEAD",
            "--not",
            "--indexed-objects",
        ],
        &["rev-list", "--objects", "--unpacked", "HEAD"],
        &["rev-list", "--unpacked", "--all"],
        &["rev-list", "--disk-usage", "HEAD"],
        &["rev-list", "--disk-usage", "--objects", "HEAD"],
        &["rev-list", "--objects", "HEAD", "--disk-usage=human"],
        &["rev-list", "--objects", "HEAD", "--disk-usage=typo"],
        &["rev-list", "--maximal-only", "HEAD~1", "HEAD~2", "side"],
        &["rev-list", "--maximal-only", "--boundary", "HEAD~1..HEAD"],
        &["rev-list", "--exclude-first-parent-only", "main", "^side~1"],
        &["rev-list", "--exclude-hidden=fetch", "--all"],
        &[
            "-c",
            "transfer.hideRefs=refs/hidden/",
            "rev-list",
            "--exclude-hidden=receive",
            "--all",
        ],
        &[
            "-c",
            "uploadpack.hideRefs=refs/",
            "-c",
            "uploadpack.hideRefs=!refs/tags",
            "rev-list",
            "--exclude-hidden=uploadpack",
            "--all",
        ],
        &[
            "-c",
            "transfer.hideRefs=refs/hidden",
            "rev-list",
            "--all",
            "--exclude-hidden=fetch",
            "--not",
            "--all",
        ],
        &["rev-list", "--exclude-hidden=bogus", "--all"],
        &[
            "rev-list",
            "--exclude-hidden=fetch",
            "--exclude-hidden=fetch",
            "--all",
        ],
        &["rev-list", "--exclude-hidden=fetch", "--branches"],
        &["rev-list", "--alternate-refs"],
        &["rev-list", "-z", "HEAD~2..HEAD"],
        &["rev-list", "-z", "--parents", "--boundary", "HEAD~2..HEAD"],
        &["rev-list", "-z", "--objects", "HEAD~1..HEAD"],
        &["rev-list", "-z", "--left-right", "HEAD...side"],
        &["rev-list", "-z", "--oneline", "HEAD"],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
}

/// `--alternate-refs` in a shared clone, limited by
/// `core.alternateRefsPrefixes` or replaced by `core.alternateRefsCommand`.
#[test]
fn alternate_refs_follow_the_configuration() {
    let mut fx = history().git(["clone", "-q", "-s", ".", "child"]);
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["-C", "child", "rev-list", "--alternate-refs"][..],
        &[
            "-C",
            "child",
            "-c",
            "core.alternateRefsPrefixes=refs/heads/side",
            "rev-list",
            "--alternate-refs",
        ],
        &[
            "-C",
            "child",
            "-c",
            "core.alternateRefsPrefixes=refs/heads/side refs/tags/",
            "rev-list",
            "--alternate-refs",
        ],
        &[
            "-C",
            "child",
            "-c",
            "core.alternateRefsCommand=git -C \"$1\" rev-parse side #",
            "rev-list",
            "--alternate-refs",
        ],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
}

/// A commit whose parent is missing: the walk fails with `Failed to
/// traverse parents` (the connectivity check of `receive-pack`), while a
/// missing parent of an excluded commit only ends the walk there.
#[test]
fn missing_parent_fails_the_traversal() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"]);
    fx = commit(fx, "a");
    fx = commit(fx, "b");
    fx = commit(fx, "c");
    if fx.is_skipped() {
        return;
    }
    let tip = String::from_utf8(fx.git_stdout(["rev-parse", "HEAD"])).unwrap();
    let tip = tip.trim().to_string();
    let parent = String::from_utf8(fx.git_stdout(["rev-parse", "HEAD~1"])).unwrap();
    let parent = parent.trim().to_string();
    fx = fx
        .git(["checkout", "-q", "--orphan", "other"])
        .git(["commit", "-q", "--allow-empty", "-m", "other"])
        .git(["branch", "-q", "-D", "main"])
        .remove(format!(".git/objects/{}/{}", &parent[..2], &parent[2..]));
    let stdin = format!("{tip}\n");
    for args in [
        &["rev-list", "--objects", "--stdin", "--not", "--all"][..],
        &[
            "rev-list",
            "--objects",
            "--stdin",
            "--not",
            "--all",
            "--quiet",
        ],
        &["rev-list", "--stdin"],
    ] {
        fx.compare(args)
            .stdin(stdin.clone())
            .checks(Check::OUTPUT)
            .run();
    }
    for args in [
        &["rev-list", tip.as_str()][..],
        &["log", "--oneline", tip.as_str()],
        &["rev-list", "--topo-order", tip.as_str()],
        &["rev-list", "--bisect", tip.as_str()],
        &["rev-list", tip.as_str(), "--", "a"],
        &["rev-list", "HEAD", "--not", tip.as_str()],
        &["rev-list", "--objects", "HEAD", "--not", tip.as_str()],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
}

/// `log --source` names what a commit was reached from: the ref without
/// its prefix for `--branches`, `--tags` and `--remotes`, the full name
/// for `--all`, `.alternate` for `--alternate-refs`.
#[test]
fn source_names_of_ref_sets() {
    let mut fx = history()
        .git(["tag", "-a", "-m", "tag", "annotated", "side"])
        .git(["clone", "-q", "-s", ".", "child"]);
    if fx.is_skipped() {
        return;
    }
    for set in [
        &["--branches"][..],
        &["--tags"],
        &["--remotes"],
        &["--remotes=origin"],
        &["--branches=ma*"],
        &["--all"],
        &["--glob=refs/remotes/origin/s*"],
        &["--alternate-refs"],
        &["--alternate-refs", "HEAD"],
        &["--branches", "--tags", "--remotes"],
    ] {
        let mut args = vec!["-C", "child", "log", "--oneline", "--source"];
        args.extend_from_slice(set);
        fx.compare(&args).checks(Check::OUTPUT).run();
        let mut args = vec!["-C", "child", "log", "--format=%h %S"];
        args.extend_from_slice(set);
        fx.compare(&args).checks(Check::OUTPUT).run();
    }
}

/// The loose file of object `id` (hex) below `work`.
fn loose_path(work: &std::path::Path, id: &str) -> std::path::PathBuf {
    work.join(".git/objects").join(&id[..2]).join(&id[2..])
}

/// The trimmed stdout of a reference git command.
fn git_line(fx: &Fixture, args: &[&str]) -> String {
    String::from_utf8(fx.git_stdout(args))
        .unwrap()
        .trim()
        .to_owned()
}

/// `--verify-objects`: a listed blob stored under a wrong name is reported
/// and listed; a tip that does not hash to its id (here a truncated loose
/// blob) is fatal before anything is listed; a missing blob is fatal.
#[test]
fn verify_objects_checks_blobs_and_tips() {
    let mut fx = history();
    if fx.is_skipped() {
        return;
    }
    let work = fx.work_dir();
    // A blob whose file is copied to another id (t1450's "bad sha1").
    fx = fx.write("fresh", "fresh\n");
    let real = git_line(&fx, &["hash-object", "-w", "fresh"]);
    let bad = format!("ff{}", &real[2..]);
    std::fs::create_dir_all(work.join(".git/objects/ff")).unwrap();
    std::fs::copy(loose_path(&work, &real), loose_path(&work, &bad)).unwrap();
    fx = fx.git([
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("100644,{bad},bad"),
    ]);
    let tree = git_line(&fx, &["write-tree"]);
    let bogus = git_line(&fx, &["commit-tree", "-m", "bogus", &tree]);
    // A blob missing from a tree.
    fx = fx.git([
        "update-index",
        "--add",
        "--cacheinfo",
        "100644,1234567812345678123456781234567812345678,gone",
    ]);
    let tree = git_line(&fx, &["write-tree", "--missing-ok"]);
    let gone = git_line(&fx, &["commit-tree", "-m", "gone", &tree]);
    fx = fx.git(["read-tree", "HEAD"]);
    // A large blob truncated so that its header still parses.
    fx = fx.write("big", "x".repeat(5000));
    let big = git_line(&fx, &["hash-object", "-w", "big"]);
    let path = loose_path(&work, &big);
    let data = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    // (How long the file is depends on the zlib of the platform.)
    std::fs::write(&path, &data[..data.len() * 3 / 4]).unwrap();
    for args in [
        &["rev-list", "--verify-objects", "HEAD"][..],
        &["rev-list", "--verify-objects", "--all"],
        &["rev-list", "--verify-objects", &bogus],
        &["rev-list", "--verify-objects", "--count", &bogus],
        &["rev-list", "--verify-objects", &big],
        &["rev-list", "--objects", &big],
        &["rev-list", "--verify-objects", &gone],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
}
