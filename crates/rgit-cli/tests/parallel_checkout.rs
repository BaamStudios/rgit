// SPDX-License-Identifier: MIT
//! Differential tests of parallel checkout (`checkout.workers`,
//! `checkout.thresholdForParallelism`) against the reference git: the work
//! tree, the index, the messages and the number of `checkout--worker`
//! children reported to trace2 must match.

use rgit_difftest::{Check, Fixture, Normalizer};

const ALL: [Check; 5] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Index,
    Check::Worktree,
];

/// Keeps the plain stderr lines and, of the trace2 events, only the
/// `checkout--worker` children (git's worker processes write their own
/// events to the same stream).
fn workers_only() -> Normalizer {
    Normalizer::new("trace2 checkout--worker children", |text| {
        let mut out = String::new();
        for line in text.lines() {
            if line.starts_with("{\"event\":") {
                if line.contains("\"event\":\"child_start\"")
                    && line.contains("\"argv\":[\"git\",\"checkout--worker\"]")
                {
                    out.push_str("child_start git checkout--worker\n");
                }
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
        out
    })
}

fn base() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
}

/// Runs `args` with `workers` and `threshold`, comparing everything
/// including the number of workers started.
fn run(mut fx: Fixture, workers: &str, threshold: &str, args: &[&str]) {
    let w = format!("checkout.workers={workers}");
    let t = format!("checkout.thresholdForParallelism={threshold}");
    let mut argv = vec!["-c", w.as_str(), "-c", t.as_str()];
    argv.extend_from_slice(args);
    fx.compare(&argv)
        .env("GIT_TRACE2_EVENT", "2")
        .checks(ALL)
        .normalize_stderr(workers_only())
        .run();
}

/// Two branches with files, directories and symbolic links swapping
/// places (after t2080's layout, without submodules).
fn branches() -> Fixture {
    let fx = base()
        .git(["checkout", "-q", "-b", "B1"])
        .write("a/a", "a/a\n")
        .write("b", "b\n")
        .write("c/c", "c/c\n")
        .write("i", "B1 i\n")
        .write("l/l", "l/l\n");
    #[cfg(unix)]
    let fx = fx.symlink("c", "d").symlink("c", "j");
    let fx = fx
        .git(["add", "."])
        .git(["commit", "-q", "-m", "B1"])
        .git(["checkout", "-q", "-b", "B2"])
        .git(["rm", "-rq", "."])
        .write("a", "a\n")
        .write("b/b", "b/b\n")
        .write("d/d", "d/d\n")
        .write("i", "B2 i\n")
        .write("m/m", "m/m\n");
    #[cfg(unix)]
    let fx = fx.symlink("b", "c").symlink("b", "j");
    fx.git(["add", "."])
        .git(["commit", "-q", "-m", "B2"])
        .git(["checkout", "-q", "B1"])
        .tick()
}

#[test]
fn switch_branches() {
    for (workers, threshold) in [("1", "0"), ("2", "0"), ("2", "100"), ("8", "0"), ("0", "0")] {
        run(branches(), workers, threshold, &["checkout", "-q", "B2"]);
        run(
            branches(),
            workers,
            threshold,
            &["reset", "-q", "--hard", "B2"],
        );
    }
}

fn five_files() -> Fixture {
    base()
        .write("f1", "1\n")
        .write("f2", "2\n")
        .write("f3", "3\n")
        .write("d/f4", "4\n")
        .write_executable("d/f5", "5\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "files"])
        .tick()
}

#[test]
fn checkout_paths_worker_count() {
    // Workers: min(checkout.workers, queued entries), none below two or
    // below the threshold.
    run(
        five_files()
            .remove("f1")
            .remove("f2")
            .remove("f3")
            .remove("d"),
        "4",
        "0",
        &["checkout", "."],
    );
    run(
        five_files().remove("f1").remove("f2"),
        "4",
        "0",
        &["checkout", "."],
    );
    run(five_files().remove("f1"), "4", "0", &["checkout", "."]);
    run(
        five_files().remove("f1").remove("f2").remove("f3"),
        "4",
        "3",
        &["checkout", "."],
    );
    run(
        five_files().remove("f1").remove("f2"),
        "4",
        "3",
        &["checkout", "."],
    );
    run(
        five_files().remove("f1").remove("f2"),
        "1",
        "0",
        &["checkout", "."],
    );
    // Nothing to do: no workers.
    run(five_files(), "2", "0", &["checkout", "HEAD"]);
}

#[test]
fn force_replaces_dirty_paths() {
    // t2080 "parallel checkout respects --[no]-force".
    let fx = || {
        five_files()
            .remove("d")
            .write("d", "changed\n")
            .write("f1", "changed\n")
    };
    run(fx(), "2", "0", &["checkout", "HEAD"]);
    run(fx(), "2", "0", &["checkout", "--force", "HEAD"]);
}

#[cfg(unix)]
#[test]
fn symlink_in_leading_dir() {
    // t2080 "parallel checkout checks for symlinks in leading dirs".
    let fx = five_files()
        .remove("d")
        .write("untracked/keep", "keep\n")
        .symlink("untracked", "d");
    run(fx, "2", "0", &["checkout", "--force", "HEAD"]);
}

#[test]
fn missing_blob_report() {
    // Failed entries are not counted in "Updated N paths".
    for workers in ["1", "2"] {
        let fx = base()
            .write("x.a", "a\n")
            .write("y.a", "a\n")
            .write("z.b", "b\n")
            .write("w.b", "b\n")
            .git(["add", "."])
            .git(["commit", "-q", "-m", "files"])
            .tick();
        let blob = String::from_utf8(fx.git_stdout(["rev-parse", ":x.a"])).expect("hex");
        let blob = blob.trim();
        let fx = fx
            .remove(format!(".git/objects/{}/{}", &blob[..2], &blob[2..]))
            .remove("x.a")
            .remove("y.a")
            .remove("z.b")
            .remove("w.b");
        let mut fx = fx;
        let w = format!("checkout.workers={workers}");
        fx.compare([
            "-c",
            w.as_str(),
            "-c",
            "checkout.thresholdForParallelism=0",
            "checkout",
            ".",
        ])
        .checks([Check::Stderr, Check::Worktree])
        // git's worker processes report their failures concurrently.
        .normalize_stderr(Normalizer::new("sorted lines", |text| {
            let mut lines: Vec<&str> = text.lines().collect();
            lines.sort_unstable();
            lines.iter().map(|l| format!("{l}\n")).collect()
        }))
        .run();
    }
}

#[test]
fn conversions_in_workers() {
    // t2082: ident, eol and an external filter (not parallel-eligible)
    // in one checkout.
    let fx = || {
        base()
            .git(["config", "core.autocrlf", "false"])
            .git(["config", "filter.up.smudge", "tr a-z A-Z"])
            .git(["config", "filter.up.clean", "tr A-Z a-z"])
            .write(".gitattributes", "I ident\nE eol=crlf\nF filter=up\n")
            .write("I", "$Id$\n")
            .write("J", "$Id$\n")
            .write("E", "multi\nline\n")
            .write("F", "abc\n")
            .write("G", "plain\n")
            .git(["add", "."])
            .git(["commit", "-q", "-m", "conv"])
            .tick()
            .remove("I")
            .remove("J")
            .remove("E")
            .remove("F")
            .remove("G")
    };
    run(fx(), "2", "0", &["checkout", "I", "J", "E", "F", "G"]);
    run(fx(), "2", "0", &["reset", "-q", "--hard"]);
}

#[test]
fn clone_with_workers() {
    let fx = five_files();
    run(fx, "2", "0", &["clone", "-q", ".", "copy"]);
    run(five_files(), "2", "100", &["clone", "-q", ".", "copy"]);
}
