// SPDX-License-Identifier: MIT
//! Differential tests of the sparse index against the reference git: every
//! step runs with both binaries from the same state (git's result of the
//! step before), so rgit reads indexes git wrote; the index check reads
//! the index rgit wrote with git (version, entry count, entries), so git
//! reads rgit's sparse directory entries. `ls-files --sparse` shows the
//! sparse directory entries themselves.

use rgit_difftest::Fixture;

fn fixture() -> Fixture {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a", "a\n")
        .write("e", "e\n")
        .write("folder1/a", "a\n")
        .write("folder1/0/1", "1\n")
        .write("folder1/0/0/0", "0\n")
        .write("folder2/a", "a\n")
        .write("deep/a", "a\n")
        .write("deep/deeper1/a", "a\n")
        .write("deep/deeper1/deepest/a", "a\n")
        .write("deep/deeper2/a", "a\n")
        .write("x/a", "a\n");
    // Racily clean index entries would make git see the files as
    // modified; the index is written a second after them.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let fx = fx
        .git(["add", "."])
        .git(["commit", "-q", "-m", "initial"])
        .git(["checkout", "-q", "-b", "side"])
        .write("folder1/a", "side\n")
        .write("folder2/new", "new\n")
        .write("newdir/n", "n\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "side"]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let fx = fx.git(["checkout", "-q", "main"]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fx.git(["update-index", "--refresh"])
        .git(["config", "advice.sparseIndexExpanded", "false"])
        .tick()
}

const FILES: [&str; 3] = ["config.worktree", "info/sparse-checkout", "config"];

#[track_caller]
fn step(fx: &mut Fixture, args: &[&str]) {
    let mut c = fx.compare(args);
    for f in FILES {
        c = c.git_file(f);
    }
    c.run();
}

fn sparse_index(fx: &mut Fixture, cone: &[&str]) {
    step(fx, &["sparse-checkout", "init", "--cone", "--sparse-index"]);
    let mut args = vec!["sparse-checkout", "set"];
    args.extend_from_slice(cone);
    step(fx, &args);
    step(fx, &["ls-files", "--sparse", "--stage"]);
}

#[test]
fn collapse_and_expand() {
    let mut fx = fixture();
    sparse_index(&mut fx, &["deep"]);
    step(&mut fx, &["status"]);
    step(&mut fx, &["status", "--porcelain=v2"]);
    step(&mut fx, &["ls-files"]);
    step(&mut fx, &["sparse-checkout", "set", "deep/deeper1", "x"]);
    step(&mut fx, &["ls-files", "--sparse", "--stage"]);
    step(&mut fx, &["sparse-checkout", "set"]);
    step(&mut fx, &["ls-files", "--sparse"]);
    step(&mut fx, &["sparse-checkout", "add", "folder1"]);
    step(&mut fx, &["ls-files", "--sparse", "--stage"]);
    step(&mut fx, &["sparse-checkout", "init", "--no-sparse-index"]);
    step(&mut fx, &["ls-files", "--sparse", "--stage"]);
    step(&mut fx, &["sparse-checkout", "init", "--sparse-index"]);
    step(&mut fx, &["ls-files", "--sparse", "--stage"]);
    step(&mut fx, &["sparse-checkout", "disable"]);
    step(&mut fx, &["ls-files", "--sparse", "--stage"]);
}

#[test]
fn commands_on_a_sparse_index() {
    let mut fx = fixture();
    sparse_index(&mut fx, &["deep"]);
    // Directories appearing and changing outside the cone.
    step(&mut fx, &["checkout", "side"]);
    step(&mut fx, &["ls-files", "--sparse", "--stage"]);
    step(&mut fx, &["checkout", "main"]);
    step(&mut fx, &["ls-files", "--sparse", "--stage"]);
    step(&mut fx, &["reset", "--soft", "side"]);
    step(&mut fx, &["status", "--porcelain=v2"]);
    step(&mut fx, &["diff", "--cached", "--stat"]);
    step(&mut fx, &["reset", "--hard"]);
    step(&mut fx, &["reset", "side", "--", "folder1/a"]);
    step(&mut fx, &["ls-files", "--sparse", "--stage"]);
    step(&mut fx, &["reset", "--hard"]);
    // Changes inside the cone.
    fx = fx.write("deep/a", "changed\n");
    step(&mut fx, &["add", "deep/a"]);
    step(&mut fx, &["commit", "-q", "-m", "deep"]);
    step(&mut fx, &["ls-files", "--sparse", "--stage"]);
    step(&mut fx, &["cat-file", "-p", ":folder1/a"]);
    step(&mut fx, &["rev-parse", ":deep/a"]);
    // Whole sparse directories as one entry.
    step(&mut fx, &["rm", "--sparse", "-r", "-q", "folder2"]);
    step(&mut fx, &["ls-files", "--sparse", "--stage"]);
    step(&mut fx, &["reset", "--hard"]);
    step(&mut fx, &["rm", "--sparse", "folder1/*"]);
    step(&mut fx, &["status", "--porcelain=v2"]);
    step(&mut fx, &["reset", "--hard"]);
    step(&mut fx, &["checkout-index", "-f", "--", "folder1/"]);
    step(&mut fx, &["checkout-index", "--", "folder1/a"]);
    step(&mut fx, &["sparse-checkout", "reapply"]);
    step(&mut fx, &["ls-files", "--sparse", "--stage"]);
}

#[test]
fn files_present_outside_the_cone() {
    let mut fx = fixture();
    sparse_index(&mut fx, &["deep"]);
    step(
        &mut fx,
        &["config", "--unset", "advice.sparseIndexExpanded"],
    );
    fx = fx.write("folder1/a", "edited\n");
    step(&mut fx, &["status", "--porcelain=v2"]);
    step(&mut fx, &["ls-files", "--sparse", "--stage", "-t"]);
    step(&mut fx, &["add", "--refresh", "folder1/a"]);
    step(&mut fx, &["add", "--sparse", "folder1/a"]);
    step(&mut fx, &["ls-files", "--sparse", "--stage", "-t"]);
    step(&mut fx, &["commit", "-q", "-m", "folder1"]);
    step(&mut fx, &["sparse-checkout", "reapply"]);
    step(&mut fx, &["ls-files", "--sparse", "--stage"]);
}

#[test]
fn hot_paths_inside_the_cone() {
    let mut fx = fixture();
    sparse_index(&mut fx, &["deep"]);
    fx = fx.write("deep/a", "changed\n").write("deep/new", "new\n");
    step(&mut fx, &["status", "--porcelain=v2"]);
    step(&mut fx, &["diff"]);
    step(&mut fx, &["add", "-A"]);
    step(&mut fx, &["diff", "--cached", "--name-status"]);
    step(&mut fx, &["diff", "HEAD", "--stat"]);
    step(&mut fx, &["commit", "-q", "-m", "in cone"]);
    step(&mut fx, &["ls-files", "--sparse", "--stage"]);
    fx = fx.write("deep/deeper1/a", "more\n");
    step(&mut fx, &["commit", "-q", "-a", "-m", "all"]);
    step(&mut fx, &["reset", "-q", "HEAD~1"]);
    step(&mut fx, &["status", "--porcelain=v2"]);
    step(&mut fx, &["ls-files", "--sparse", "--stage"]);
    // Trees outside the cone that differ: expanded file by file.
    step(&mut fx, &["reset", "-q", "side"]);
    step(&mut fx, &["status", "--porcelain=v2"]);
    step(&mut fx, &["ls-files", "--sparse", "--stage"]);
}

/// The commands working on sparse directory entries do not read the
/// trees below them: they succeed with such a tree missing.
#[test]
fn sparse_directories_are_not_read() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let fx = fx
        .git(["sparse-checkout", "init", "--cone", "--sparse-index"])
        .git(["sparse-checkout", "set", "deep"]);
    let tree = String::from_utf8(fx.git_stdout(["rev-parse", "HEAD:folder1/0"])).unwrap();
    let tree = tree.trim();
    let fx = fx.remove(format!(".git/objects/{}/{}", &tree[..2], &tree[2..]));
    let home = fx.home_dir();
    let rgit = |args: &[&str]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_rgit"))
            .args(args)
            .current_dir(fx.work_dir())
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@example.com")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "rgit {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    std::fs::write(fx.work_dir().join("deep/a"), "changed\n").unwrap();
    rgit(&["status"]);
    rgit(&["diff"]);
    rgit(&["add", "deep/a"]);
    rgit(&["diff", "--cached"]);
    rgit(&["commit", "-q", "-m", "deep"]);
    rgit(&["reset", "-q", "HEAD~1"]);
    rgit(&["reset", "-q", "--hard", "HEAD"]);
    rgit(&["switch", "-q", "-c", "topic"]);
    rgit(&["switch", "-q", "main"]);
}
