// SPDX-License-Identifier: MIT
//! Differential tests of `rgit init` against the reference git: output,
//! exit code, and the created files with their modes and contents.
//!
//! The default template differs on purpose (rgit ships its own, see
//! CLEANROOM.md), so every test that looks at the layout passes the same
//! template directory to both sides. Bare repositories and
//! `--separate-git-dir` put the whole repository outside a `.git`
//! directory, where the `Worktree` check compares every file.

use rgit_difftest::{Check, Fixture, Normalizer};

/// A fixture with a template directory `tpl` (regular, executable and
/// nested files, a symbolic link and a dot file that must not be copied),
/// not used by default.
fn base() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .write("tpl/description", "template description\n")
        .write("tpl/info/exclude", "*.o\n")
        .write_executable("tpl/hooks/pre-commit.sample", "#!/bin/sh\nexit 0\n")
        .write("tpl/a/b/c", "nested\n")
        .write("tpl/.hidden", "not copied\n")
        .symlink("description", "tpl/link")
}

/// [`base`] with `GIT_TEMPLATE_DIR` pointing to `tpl`.
fn fixture() -> Fixture {
    let fx = base();
    if fx.is_skipped() {
        return fx;
    }
    let t = tpl(&fx);
    fx.env("GIT_TEMPLATE_DIR", t)
}

/// The template directory, absolute.
fn tpl(fx: &Fixture) -> String {
    fx.work_dir().join("tpl").display().to_string()
}

#[test]
fn bare_layout() {
    let mut fx = fixture();
    fx.compare(["init", "--bare", "b.git"]).run();
    fx.compare(["init", "--bare", "b.git"]).run();
}

#[test]
fn work_tree_layout() {
    let mut fx = fixture();
    fx.compare(["init", "repo"]).repo("repo").run();
    fx.compare(["init", "--separate-git-dir", "sep", "wt"])
        .repo("wt")
        .run();
    // Reinitialising moves the repository.
    fx.compare(["-C", "wt", "init", "--separate-git-dir", "../moved"])
        .repo("wt")
        .run();
    fx.compare(["-C", "repo", "init", "--separate-git-dir", "../moved2"])
        .repo("repo")
        .run();
}

#[test]
fn shared_layout() {
    let mut fx = fixture();
    for (i, v) in [
        "", "=group", "=all", "=0640", "=0660", "=0777", "=umask", "=1",
    ]
    .iter()
    .enumerate()
    {
        let dir = format!("s{i}.git");
        fx.compare(["init", "--bare", &format!("--shared{v}"), &dir])
            .run();
    }
    fx.compare([
        "-c",
        "core.sharedRepository=0660",
        "init",
        "--bare",
        "c.git",
    ])
    .run();
    for bad in ["--shared=bogus", "--shared=0444", "--shared=3"] {
        fx.compare(["init", bad, "bad"]).run();
    }
    fx.compare(["-c", "core.sharedRepository=bogus", "init", "bad"])
        .run();
}

#[test]
fn reinit_keeps_files() {
    let mut fx = fixture()
        .git(["init", "-q", "--bare", "r.git"])
        .write(
            "r.git/config",
            "[core]\n  repositoryFormatVersion=0\n#c\n\tFileMode = false\n[foo]\n\tbar = 1\n",
        )
        .write("r.git/description", "mine\n")
        .remove("r.git/refs/tags");
    fx.compare(["init", "--bare", "-b", "other", "r.git"]).run();
    fx.compare(["init", "--bare", "--shared", "r.git"]).run();
}

#[test]
fn template_options() {
    let mut fx = fixture();
    let t = tpl(&fx);
    fx.compare(["init", "--bare", "--template=", "empty.git"])
        .run();
    fx.compare(["init", "--bare", "--template=tpl", "t.git"])
        .run();
    fx.compare(["init", "--bare", "--template=missing", "m.git"])
        .run();
    let mut fx = base();
    fx.compare([
        "-c",
        &format!("init.templateDir={t}"),
        "init",
        "--bare",
        "c.git",
    ])
    .run();
    fx.compare(["init", "--bare", "e.git"])
        .env("GIT_TEMPLATE_DIR", "")
        .run();
}

#[test]
fn template_with_config() {
    let mut fx = fixture().write(
        "tpl2/config",
        "[core]\n\tbare = true\n\tfilemode = false\n[init]\n\tdefaultBranch = tplbranch\n",
    );
    fx.compare(["init", "--bare", "--template=tpl2", "a.git"])
        .run();
    let mut fx = fixture().write(
        "tpl3/config",
        "[core]\n\trepositoryformatversion = 1\n[extensions]\n\tfoo = 1\n",
    );
    fx.compare(["init", "--bare", "--template=tpl3", "b.git"])
        .run();
    let mut fx = fixture().write("tpl4/config", "[core\n");
    fx.compare(["init", "--bare", "--template=tpl4", "c.git"])
        .run();
    let mut fx = fixture().write("tpl5/config", "[core]\n\tsharedRepository = 0660\n");
    fx.compare(["init", "--bare", "--template=tpl5", "d.git"])
        .run();
}

#[test]
fn initial_branch() {
    let mut fx = fixture();
    fx.compare(["init", "-q", "-b", "trunk", "a"])
        .repo("a")
        .run();
    fx.compare(["init", "-b", "again", "a"]).repo("a").run();
    fx.compare(["init", "-b", "with space", "b"])
        .output_only()
        .run();
    fx.compare(["init", "-b", "", "c"]).output_only().run();
    fx.compare(["-c", "init.defaultBranch=nmb", "init", "d"])
        .repo("d")
        .run();
    fx.compare(["-c", "init.defaultBranch=bad name", "init", "e"])
        .output_only()
        .run();
}

/// Without `init.defaultBranch` git prints an advice (colored on request).
#[test]
fn default_branch_advice() {
    let mut fx = fixture();
    for (i, args) in [
        &["init"][..],
        &["init", "-q"],
        &["-c", "color.advice=always", "init"],
        &["-c", "advice.defaultBranchName=false", "init"],
        &["--no-advice", "init"],
    ]
    .iter()
    .enumerate()
    {
        let mut argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        argv.push(format!("r{i}"));
        fx.compare(&argv)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output_only()
            .run();
    }
}

#[test]
fn git_dir_and_work_tree() {
    let mut fx = fixture().write("g/keep", "").write("h/keep", "");
    let wd = fx.work_dir().display().to_string();
    fx.compare(["init"]).env("GIT_DIR", "g1.git").run();
    fx.compare(["init"]).env("GIT_DIR", "g2/.git").run();
    fx.compare(["init"])
        .in_dir("g")
        .env("GIT_DIR", ".")
        .repo("g")
        .run();
    fx.compare(["init"])
        .env("GIT_DIR", "g3.git")
        .env("GIT_WORK_TREE", &wd)
        .run();
    fx.compare(["init"])
        .in_dir("h")
        .env("GIT_DIR", ".git")
        .env("GIT_WORK_TREE", ".")
        .repo("h")
        .run();
    fx.compare(["init", "x"])
        .env("GIT_WORK_TREE", &wd)
        .output_only()
        .run();
    fx.compare(["--bare", "init"])
        .env("GIT_DIR", "g4.git")
        .env("GIT_WORK_TREE", &wd)
        .output_only()
        .run();
    fx.compare(["--bare", "init", "gb"]).run();
    fx.compare(["init", "--separate-git-dir", "s"])
        .env("GIT_DIR", "foo")
        .output_only()
        .run();
}

#[test]
fn directory_argument_errors() {
    let mut fx = fixture().write("file", "").write("d/a", "");
    fx.compare(["init", "file"]).run();
    fx.compare(["init", "d/a/b"]).run();
    fx.compare(["init", "new/deep/dir"])
        .repo("new/deep/dir")
        .run();
    fx.compare(["init", "a", "b"]).run();
    fx.compare(["init", "--bare", "--separate-git-dir", "x", "y"])
        .run();
    fx.compare(["init", "--separate-git-dir", "no/such/dir", "z"])
        .run();
}

#[test]
fn formats() {
    let mut fx = fixture();
    fx.compare(["init", "--object-format=sha1", "--ref-format=files", "a"])
        .repo("a")
        .run();
    fx.compare(["init", "--object-format=bogus", "b"]).run();
    fx.compare(["init", "--ref-format=bogus", "c"]).run();
    fx.compare(["init", "d"])
        .env("GIT_DEFAULT_HASH", "bogus")
        .output_only()
        .run();
    fx.compare(["init", "e"])
        .env("GIT_DEFAULT_REF_FORMAT", "bogus")
        .output_only()
        .run();
    fx.compare([
        "-c",
        "init.defaultObjectFormat=bogus",
        "-c",
        "init.defaultRefFormat=bogus",
        "init",
        "f",
    ])
    .repo("f")
    .run();
    // Reinitialising with a different format is refused.
    fx.compare(["init", "--object-format=sha256", "a"])
        .output_only()
        .run();
    fx.compare(["init", "--ref-format=reftable", "a"])
        .output_only()
        .run();
}

/// Creating SHA-256 or reftable repositories is refused until rgit supports
/// them (#12, #31); git creates them.
#[test]
fn unsupported_formats_are_refused() {
    let mut fx = fixture();
    let out = fx
        .compare(["init", "--object-format=sha256", "a"])
        .checks([])
        .run();
    if let Some(out) = out {
        assert_eq!(out.rgit.code, Some(128));
        let err = String::from_utf8_lossy(&out.rgit.stderr);
        assert!(err.contains("extensions.objectFormat=sha256"), "{err}");
    }
}

#[test]
fn help_and_usage() {
    let mut fx = fixture();
    fx.compare(["init", "-h"]).run();
    fx.compare(["init", "--bogus"]).run();
    fx.compare(["init", "--template"]).run();
    fx.compare(["init", "--no-shared"]).run();
    fx.compare(["init-db", "-q", "x"]).repo("x").run();
}

/// `git status` and `git fsck` work in a repository made by rgit.
#[test]
fn usable_by_git() {
    let mut fx = fixture();
    fx.compare(["init", "-q", "r"]).repo("r").run();
    let out = fx.git_output(["-C", "r", "status", "--porcelain"]);
    assert!(out.status.success() || fx.is_skipped());
    let out = fx.git_output(["-C", "r", "fsck", "--strict"]);
    assert!(out.status.success() || fx.is_skipped());
}

#[test]
fn reinit_quiet_and_messages() {
    let mut fx = fixture().git(["init", "-q", "r"]);
    fx.compare(["init", "r"])
        .repo("r")
        .normalize(Normalizer::replace("\r", ""))
        .run();
    fx.compare(["init", "-q", "r"]).repo("r").run();
    fx.compare(["init", "--shared", "r"])
        .checks(Check::OUTPUT)
        .run();
}

/// Permission bits (including set-group-id) of every file and directory
/// below `root`, relative to it.
#[cfg(unix)]
fn modes(root: &std::path::Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    fn walk(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<String>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            let m = std::fs::symlink_metadata(&p).unwrap();
            let rel = p.strip_prefix(root).unwrap().display().to_string();
            out.push(format!("{:o} {rel}", m.permissions().mode() & 0o7777));
            if m.is_dir() {
                walk(&p, root, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.join("\n")
}

/// `--shared` changes permission bits, which the `Worktree` check does not
/// show (it prints tree modes); compare them directly.
#[cfg(unix)]
#[test]
fn shared_permissions() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    for (i, v) in [
        "group", "all", "0640", "0660", "0600", "0606", "0777", "umask",
    ]
    .iter()
    .enumerate()
    {
        let args = |dir: &str| {
            vec![
                "init".to_string(),
                "-q".into(),
                format!("--shared={v}"),
                dir.to_string(),
            ]
        };
        let git_dir = format!("git{i}");
        let out = fx.git_output(args(&git_dir));
        assert!(out.status.success());
        let rgit_dir = format!("rgit{i}");
        let status = std::process::Command::new(env!("CARGO_BIN_EXE_rgit"))
            .args(args(&rgit_dir))
            .current_dir(fx.work_dir())
            .env_clear()
            .env("HOME", fx.home_dir())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TEMPLATE_DIR", tpl(&fx))
            .status()
            .unwrap();
        assert!(status.success());
        rgit_difftest::assert_same_text(
            &format!("modes with --shared={v}"),
            &modes(&fx.work_dir().join(&git_dir)),
            &modes(&fx.work_dir().join(&rgit_dir)),
        );
    }
}
