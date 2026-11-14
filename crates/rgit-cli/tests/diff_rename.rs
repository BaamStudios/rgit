// SPDX-License-Identifier: MIT
//! Differential tests of diffcore (rename, copy and break detection,
//! pickaxe, `--diff-filter`) in `diff-tree`, `diff`, `diff-index` and
//! `status` against the reference git.

use rgit_difftest::Fixture;

fn lines(n: usize, prefix: &str) -> String {
    (0..n).map(|i| format!("{prefix} {i:04}\n")).collect()
}

/// `n` lines of `prefix`, with the lines in `changed` replaced.
fn edited(n: usize, prefix: &str, changed: &[usize]) -> String {
    (0..n)
        .map(|i| {
            if changed.contains(&i) {
                format!("changed {i:04}\n")
            } else {
                format!("{prefix} {i:04}\n")
            }
        })
        .collect()
}

/// Two commits with exact and inexact renames, copies (from modified and
/// unmodified files), a complete rewrite, a swap, a rename with mode
/// change, a symlink, a moved file with the same name, odd names and a
/// binary file.
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("exact", lines(30, "exact"))
        .write("docs/ext.txt", lines(60, "ext"))
        .write("inexact", lines(40, "inexact"))
        .write("modified-src", lines(50, "msrc"))
        .write("unchanged-src", lines(50, "usrc"))
        .write("rewrite", lines(80, "rewrite"))
        .write("swap-a", lines(60, "swapa"))
        .write("swap-b", lines(60, "swapb"))
        .write("tool", lines(20, "tool"))
        .write("sp ace", lines(25, "space"))
        .write("bin", "\0binary\0content\n".repeat(30).as_str())
        .write("empty", "")
        .symlink("target-x", "link")
        .write("small", "a\nb\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "first"])
        .git(["tag", "v1"])
        .tick()
        .remove("exact")
        .write("moved/exact", lines(30, "exact"))
        .remove("docs/ext.txt")
        .write("docs/config/ext.txt", edited(60, "ext", &[3, 9, 17]))
        .git(["mv", "inexact", "renamed"])
        .write("renamed", edited(40, "inexact", &[1, 2, 3, 30]))
        .write("copy-of-modified", edited(50, "msrc", &[0]))
        .write("modified-src", edited(50, "msrc", &[10, 11]))
        .write("copy-of-unchanged", lines(50, "usrc"))
        .write(
            "rewrite",
            edited(80, "rewrite", &(0..60).collect::<Vec<_>>()),
        )
        .write("swap-a", lines(60, "swapb"))
        .write("swap-b", lines(60, "swapa"))
        .git(["rm", "-q", "tool"])
        .write_executable("tools/tool", lines(20, "tool"))
        .remove("sp ace")
        .write("d\u{e4}r/new \"name\"", lines(25, "space"))
        .git(["mv", "bin", "bin2"])
        .git(["mv", "empty", "empty2"])
        .git(["mv", "link", "link2"])
        .git(["rm", "-q", "small"])
        .write("small2", "a\nb\nc\n")
        .git(["add", "-A"])
        .git(["commit", "-q", "-m", "second"])
}

#[test]
fn diff_tree_renames_match_git() {
    let mut fx = fixture();
    for opts in [
        &["-M"][..],
        &["-M", "--abbrev"],
        &["-M90%"],
        &["-M5"],
        &["-M100%"],
        &["--find-renames=30%"],
        &["-C"],
        &["-C", "-C"],
        &["--find-copies-harder"],
        &["-C", "--find-copies-harder", "-l3"],
        &["-B"],
        &["-B", "-M"],
        &["-B/70%", "-M"],
        &["-B20%/20%", "-C"],
        &["-M", "--no-renames"],
        &["-M", "--no-rename-empty"],
        &["-M", "-l1"],
        &["-M", "-l2"],
        &["-M", "-R"],
        &["-M", "--name-status"],
        &["-M", "--name-only"],
        &["-C", "--name-status", "-z"],
        &["-M", "-z"],
        &["-M", "--stat"],
        &["-M", "--numstat"],
        &["-C", "--numstat", "-z"],
        &["-M", "--summary"],
        &["-B", "-M", "--stat", "--summary"],
        &["-M", "-p"],
        &["-C", "-B", "-p"],
        &["-B", "-p", "-D"],
        &["-M", "-p", "-D"],
        &["-M", "--diff-filter=R"],
        &["-M", "--diff-filter=rd"],
        &["-C", "--diff-filter=C*"],
        &["-C", "--diff-filter=a*"],
        &["-B", "--diff-filter=B"],
    ] {
        let mut argv = vec!["diff-tree", "-r"];
        argv.extend_from_slice(opts);
        argv.extend_from_slice(&["v1", "HEAD"]);
        fx.compare(&argv).output_only().run();
    }
}

#[test]
fn diff_options_errors_match_git() {
    let mut fx = fixture();
    for opts in [
        &["-Mx"][..],
        &["-M5x"],
        &["-Cfoo"],
        &["--find-copies=x"],
        &["-Bx"],
        &["-B/x"],
        &["--diff-filter=Q"],
        &["-lx"],
        &["-G(", "--pickaxe-regex"],
        &["-Sa(", "--pickaxe-regex"],
        &["--find-object=nope"],
    ] {
        let mut argv = vec!["diff-tree", "-r"];
        argv.extend_from_slice(opts);
        argv.extend_from_slice(&["v1", "HEAD"]);
        fx.compare(&argv).output_only().run();
    }
}

#[test]
fn diff_porcelain_renames_match_git() {
    let mut fx = fixture();
    for opts in [
        &[][..],
        &["--stat"],
        &["--summary"],
        &["--name-status"],
        &["--no-renames", "--name-status"],
        &["-C", "--name-status"],
        &["-l1", "--name-status"],
        &["--raw"],
    ] {
        let mut argv = vec!["diff"];
        argv.extend_from_slice(opts);
        argv.extend_from_slice(&["v1", "HEAD"]);
        fx.compare(&argv).output_only().run();
    }
    for config in [
        "diff.renames=false",
        "diff.renames=copies",
        "diff.renames=copy",
        "diff.renames=true",
        "diff.renameLimit=1",
        "diff.renames=bogus",
    ] {
        fx.compare(["-c", config, "diff", "--name-status", "v1", "HEAD"])
            .output_only()
            .run();
    }
    // Plumbing ignores diff.renames.
    fx.compare(["-c", "diff.renames=true", "diff-tree", "-r", "v1", "HEAD"])
        .output_only()
        .run();
}

#[test]
fn diff_cached_and_index_renames_match_git() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a", lines(30, "a"))
        .write("b", lines(30, "b"))
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .git(["mv", "a", "a2"])
        .write("a2", edited(30, "a", &[4]))
        .git(["add", "a2"])
        .write("b-copy", lines(30, "b"))
        .git(["add", "b-copy"]);
    for argv in [
        &["diff", "--cached"][..],
        &["diff", "--cached", "--stat"],
        &["diff", "--cached", "-C", "-C", "--name-status"],
        &["diff-index", "--cached", "-M", "HEAD"],
        &["diff-index", "--cached", "--find-copies-harder", "HEAD"],
        &["diff", "HEAD", "--name-status"],
    ] {
        fx.compare(argv).output_only().run();
    }
}

#[test]
fn pickaxe_matches_git() {
    let mut fx = fixture();
    for opts in [
        &["-Sexact"][..],
        &["-M", "-Sexact"],
        &["-Schanged"],
        &["-Schanged", "--pickaxe-all"],
        &["-Snothing-like-this", "--pickaxe-all"],
        &["-Schan.ed 00[0-9]", "--pickaxe-regex"],
        &["-Gchanged 001"],
        &["-M", "-Gchanged"],
        &["-Gbinary"],
        &["-Sbinary"],
        &["-S", "changed"],
        &["-M", "-Schanged", "-p"],
    ] {
        let mut argv = vec!["diff-tree", "-r"];
        argv.extend_from_slice(opts);
        argv.extend_from_slice(&["v1", "HEAD"]);
        fx.compare(&argv).output_only().run();
    }
    let blob = String::from_utf8(fx.git_stdout(["rev-parse", "v1:tool"])).unwrap();
    let blob = blob.trim();
    for opts in [&["-M"][..], &[]] {
        let find = format!("--find-object={blob}");
        let mut argv = vec!["diff-tree", "-r", find.as_str()];
        argv.extend_from_slice(opts);
        argv.extend_from_slice(&["v1", "HEAD"]);
        fx.compare(&argv).output_only().run();
    }
}

#[test]
fn status_shows_staged_renames_like_git() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a", lines(30, "a"))
        .write("sp ace", lines(30, "space"))
        .write("c", lines(30, "c"))
        .write("e", lines(40, "e"))
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .git(["mv", "a", "b"])
        .write("b", edited(30, "a", &[29]))
        .git(["add", "b"])
        .remove("sp ace")
        .write("dir/x y", lines(30, "space"))
        .git(["add", "-A", "sp ace", "dir"])
        .write("c2", lines(30, "c"))
        .write("c", edited(30, "c", &[0]))
        .git(["add", "c", "c2"])
        .write("b", edited(30, "a", &[28, 29]))
        .write("a", "again\n")
        .git(["rm", "-q", "--cached", "e"])
        .write("e2", edited(40, "e", &[1, 2]))
        .git(["add", "e2"]);
    for argv in [
        &["status"][..],
        &["status", "-s"],
        &["status", "--porcelain", "-z"],
        &["status", "--porcelain=v2"],
        &["status", "--porcelain=v2", "-z"],
        &["-c", "status.renames=copies", "status"],
        &["-c", "status.renames=copies", "status", "-s"],
        &["-c", "status.renames=false", "status", "-s"],
        &["-c", "diff.renames=false", "status", "-s"],
        &["-c", "status.renameLimit=1", "status", "-s"],
        &["status", "--no-renames", "-s"],
        &["status", "--find-renames=99", "-s"],
        &["status", "-M50%", "--porcelain=v2"],
        &["-c", "status.renames=false", "status", "--renames", "-s"],
        &["commit", "--dry-run"],
        &["commit", "--dry-run", "--short"],
    ] {
        fx.compare(argv).output_only().run();
    }
}

/// `diff-tree -r -M -C` over the first-parent history of a clone of
/// git.git named by `RGIT_RENAME_WINDOW_REPO` (skipped without it), with
/// `RGIT_RENAME_WINDOW` commits (default 300).
///
/// Known deviations, not counted: git's similarity estimate merges
/// unrelated pieces of very large files (hash collisions in a table of
/// limited size), so scores of files of more than about 20 KB can be a few
/// points higher than rgit's; and equal-score candidates may be paired in
/// a different order when many of them tie.
#[test]
fn git_git_window_matches_git() {
    let Ok(repo) = std::env::var("RGIT_RENAME_WINDOW_REPO") else {
        return;
    };
    let count: usize = std::env::var("RGIT_RENAME_WINDOW")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    let known: Vec<String> = std::env::var("RGIT_RENAME_WINDOW_KNOWN")
        .unwrap_or_default()
        .split(',')
        .map(str::to_string)
        .collect();
    let home = tempfile::tempdir().unwrap();
    let run = |bin: &str, args: &[&str]| -> Vec<u8> {
        let out = std::process::Command::new(bin)
            .arg(format!("--git-dir={repo}"))
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        let mut v = out.stdout;
        v.extend_from_slice(&out.stderr);
        v
    };
    let list = run(
        "git",
        &[
            "rev-list",
            "--first-parent",
            "-n",
            &count.to_string(),
            "HEAD",
        ],
    );
    let mut bad = Vec::new();
    for c in String::from_utf8_lossy(&list).lines() {
        let parent = format!("{c}^");
        let args = ["diff-tree", "-r", "-M", "-C", parent.as_str(), c];
        if run("git", &["rev-parse", "-q", "--verify", &parent]).is_empty() {
            continue;
        }
        if run("git", &args) != run(env!("CARGO_BIN_EXE_rgit"), &args)
            && !known.iter().any(|k| k == c)
        {
            bad.push(c.to_string());
        }
    }
    assert!(bad.is_empty(), "differing commits: {bad:?}");
}
