// SPDX-License-Identifier: MIT
//! Differential tests of the `clone` options beyond the M2 core (#166,
//! #334): local clones that copy the object store, `--separate-git-dir`,
//! `GIT_WORK_TREE`, `--ref-format`, `-c`, `--revision`, guessing the
//! branch of a detached remote `HEAD`, and `branch.autoSetupRebase`.

use std::os::unix::fs::MetadataExt;

use rgit_difftest::{Check, Fixture};

fn source() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main", "src"])
        .write("src/a", "a\n")
        .git(["-C", "src", "add", "."])
        .git(["-C", "src", "commit", "-q", "-m", "one"])
        .git(["-C", "src", "tag", "-a", "-m", "t", "v1"])
        .git(["-C", "src", "repack", "-qad"])
        .tick()
        .write("src/b", "b\n")
        .git(["-C", "src", "add", "."])
        .git(["-C", "src", "commit", "-q", "-m", "two"])
        .git(["-C", "src", "branch", "side", "HEAD~1"])
}

#[test]
fn local_clone_links_the_object_store_like_git() {
    let mut fx = source().git(["init", "-q", "--bare", "alt.git"]).write(
        "src/.git/objects/info/alternates",
        "../../../alt.git/objects\n",
    );
    if fx.is_skipped() {
        return;
    }
    let cases: &[(&[&str], &str)] = &[
        (&["clone", "src", "c1"], "c1"),
        (&["clone", "--no-hardlinks", "src", "c2"], "c2"),
        (&["clone", "--bare", "src", "c3.git"], "c3.git"),
        (&["clone", "-l", "src", "c4"], "c4"),
    ];
    for (args, dir) in cases {
        let c = fx.compare(*args).repo(*dir);
        if dir.ends_with(".git") {
            c.skip(Check::Worktree).run();
        } else {
            c.run();
        }
    }
    // The object files: hard links unless --no-hardlinks; the source's
    // alternates are borrowed with an absolute path. (The comparisons run
    // on copies, so rgit runs once more in the fixture itself.)
    let work = fx.work_dir();
    let home = fx.home_dir();
    for (dir, extra) in [("linked", None), ("copied", Some("--no-hardlinks"))] {
        let status = std::process::Command::new(env!("CARGO_BIN_EXE_rgit"))
            .args(["clone", "-q"])
            .args(extra)
            .args(["src", dir])
            .current_dir(&work)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", &work)
            .status()
            .unwrap();
        assert!(status.success());
    }
    let objects = |dir: &str| {
        let mut files = Vec::new();
        let root = work.join(dir).join(".git/objects");
        let mut stack = vec![root.clone()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    let rel = p.strip_prefix(&root).unwrap().to_path_buf();
                    files.push((rel, std::fs::metadata(&p).unwrap().nlink()));
                }
            }
        }
        files.sort();
        files
    };
    for (rel, links) in objects("linked") {
        if rel.ends_with("alternates") {
            continue;
        }
        assert!(links >= 2, "{} is not hard-linked", rel.display());
    }
    assert!(objects("copied").iter().all(|(_, links)| *links == 1));
    let alternates =
        std::fs::read_to_string(work.join("linked/.git/objects/info/alternates")).unwrap();
    let alt = std::fs::canonicalize(work.join("alt.git/objects")).unwrap();
    assert_eq!(alternates, format!("{}\n", alt.display()));
}

#[test]
fn local_clone_refuses_symlinks_like_git() {
    let mut fx = source()
        .write("outside", "x\n")
        .symlink("../../../outside", "src/.git/objects/info/link");
    if fx.is_skipped() {
        return;
    }
    fx.compare(["clone", "src", "c1"])
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["clone", "--no-local", "src", "c2"])
        .repo("c2")
        .run();
}

#[test]
fn clone_layout_options_same_as_git() {
    let mut fx = source().git(["clone", "-q", "--separate-git-dir", "real4", "src", "d4"]);
    if fx.is_skipped() {
        return;
    }
    // --separate-git-dir: the work tree's `.git` file names the real
    // directory (absolute); an existing non-empty one is refused.
    // (The work tree dump would include the templates of the git
    // directory, which are rgit's own.)
    fx.compare(["clone", "--separate-git-dir", "real1", "src", "d1"])
        .repo("d1")
        .skip(Check::Worktree)
        .run();
    let gitfile = std::fs::read_to_string(fx.work_dir().join("d1/.git")).unwrap();
    let real = std::fs::canonicalize(fx.work_dir().join("real1")).unwrap();
    assert_eq!(gitfile, format!("gitdir: {}\n", real.display()));
    fx.compare(["clone", "--separate-git-dir", "real4", "src", "d5"])
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["clone", "--bare", "--separate-git-dir", "r", "src", "d6"])
        .checks(Check::OUTPUT)
        .run();
    // A `.git` file, and the directory holding one, as the source.
    fx.compare(["clone", "d4/.git", "d7"]).repo("d7").run();
    fx.compare(["clone", "d4", "d8"]).repo("d8").run();
    // GIT_WORK_TREE: <dir> is the git directory, core.worktree is set.
    fx.compare(["clone", "src", "d9"])
        .env("GIT_WORK_TREE", "wt9")
        .repo("d9")
        .skip(Check::Worktree)
        .run();
    // --ref-format.
    fx.compare(["clone", "--ref-format=files", "src", "d10"])
        .repo("d10")
        .run();
    fx.compare(["clone", "--ref-format=garbage", "src", "d11"])
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn clone_config_options_same_as_git() {
    let mut fx = source()
        .git(["-C", "src", "update-ref", "refs/grab/it", "HEAD"])
        .git(["config", "--global", "branch.autoSetupRebase", "remote"]);
    if fx.is_skipped() {
        return;
    }
    let cases: &[(&[&str], &str)] = &[
        (
            &[
                "clone",
                "-c",
                "core.foo",
                "-c",
                "credential.helper=",
                "-c",
                "credential.helper=hi",
                "src",
                "e1",
            ],
            "e1",
        ),
        (
            &[
                "clone",
                "-c",
                "remote.origin.fetch=+refs/grab/*:refs/grab/*",
                "src",
                "e2",
            ],
            "e2",
        ),
        (
            &[
                "-c",
                "remote.origin.fetch=+refs/grab/*:refs/grab/*",
                "clone",
                "src",
                "e3",
            ],
            "e3",
        ),
        (
            &[
                "clone",
                "-c",
                "remote.origin.fetch=+refs/heads/main:refs/remotes/origin/main",
                "src",
                "e4",
            ],
            "e4",
        ),
        (&["clone", "-c", "bad key=1", "src", "e5"], "e5"),
    ];
    for (args, dir) in cases {
        fx.compare(*args).repo(*dir).run();
    }
}

#[test]
fn clone_detached_remote_head_and_revision_same_as_git() {
    let mut fx = source()
        .git(["-C", "src", "branch", "abc"])
        .git(["-C", "src", "checkout", "-q", "--detach"])
        .git(["init", "-q", "--bare", "empty.git"]);
    if fx.is_skipped() {
        return;
    }
    let tree = String::from_utf8(fx.git_stdout(["-C", "src", "rev-parse", "HEAD^{tree}"])).unwrap();
    let head = String::from_utf8(fx.git_stdout(["-C", "src", "rev-parse", "HEAD"])).unwrap();
    let (tree, head) = (tree.trim().to_owned(), head.trim().to_owned());
    let url = format!("file://{}/src", fx.work_dir().display());
    let rev_tree = format!("--revision={tree}");
    let rev_head = format!("--revision={head}");
    let rev_short = format!("--revision={}", &head[..7]);
    let cases: &[(&[&str], &str)] = &[
        // A detached HEAD: the branch at the same commit.
        (&["clone", &url, "f1"], "f1"),
        (
            &["-c", "init.defaultBranch=main", "clone", &url, "f2"],
            "f2",
        ),
        (&["-c", "protocol.version=0", "clone", &url, "f3"], "f3"),
        (&["clone", "--single-branch", "src", "f4"], "f4"),
        (&["clone", "-b", "nope", "empty.git", "f5"], "f5"),
        // --revision: no refs, no refspec, a detached HEAD.
        (&["clone", "--revision=refs/heads/side", "src", "g1"], "g1"),
        (&["clone", "--revision=refs/tags/v1", "src", "g2"], "g2"),
        (&["clone", &rev_head, "--bare", "src", "g3.git"], "g3.git"),
        (
            &["clone", "--no-local", "--revision=side", &url, "g4"],
            "g4",
        ),
        (&["clone", &rev_short, "src", "g5"], "g5"),
        (&["clone", &rev_tree, "src", "g6"], "g6"),
        (&["clone", "--revision=refs/heads/main^", "src", "g7"], "g7"),
        (
            &["clone", "--revision=main", "-b", "main", "src", "g8"],
            "g8",
        ),
        (&["clone", "--revision=main", "--mirror", "src", "g9"], "g9"),
    ];
    for (args, dir) in cases {
        let c = fx.compare(*args).repo(*dir);
        if dir.ends_with(".git") {
            c.skip(Check::Worktree).run();
        } else {
            c.run();
        }
    }
}
