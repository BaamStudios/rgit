// SPDX-License-Identifier: MIT
//! Differential tests of `rgit fast-export` against the reference git: the
//! stream must be byte for byte the same for a branchy history with
//! renames, copies, tags (annotated, lightweight, nested, of blobs and
//! trees), path limits, marks and the options that change the output.

use rgit_difftest::{Check, Fixture};

fn commit(fx: Fixture, msg: &str) -> Fixture {
    fx.tick().git(["commit", "-q", "-m", msg])
}

fn lines(n: std::ops::Range<u32>) -> String {
    n.map(|i| format!("line {i}\n")).collect()
}

fn history() -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"]);
    if fx.is_skipped() {
        return fx;
    }
    fx = fx
        .write("file0", "break it\n")
        .write("file", "Wohlauf\n")
        .write("big", lines(0..40))
        .write("sp ace", "x\n")
        .git(["add", "."]);
    fx = commit(fx, "initial");
    fx = fx
        .write("file", "die Luft\n")
        .write("d/e/f", "deep\n")
        .write("tab\tname", "t\n")
        .git(["add", "."]);
    fx = commit(fx, "second");
    fx = fx
        .git(["mv", "big", "big2"])
        .write("big2", lines(0..39))
        .git(["add", "big2"]);
    fx = commit(fx, "rename");
    fx = fx.tick().git(["tag", "light"]);
    fx = fx.git(["checkout", "-q", "-b", "side", "main~1"]);
    fx = fx.write("file", "lange\n").git(["add", "file"]);
    fx = commit(fx, "side");
    fx = fx.tick().git(["tag", "-a", "-m", "annotated", "anno"]);
    fx = fx
        .tick()
        .git(["merge", "-q", "--no-ff", "-m", "merge main", "main"]);
    fx = fx.git(["checkout", "-q", "main"]);
    fx = fx.write("copy", lines(0..39)).git(["add", "copy"]);
    fx = commit(fx, "copy");
    fx = fx
        .git(["rm", "-q", "-r", "d"])
        .write("d", "now a file\n")
        .git(["add", "d"]);
    fx = commit(fx, "dir to file");
    fx = fx
        .tick()
        .git(["tag", "-a", "-m", "of a blob", "blobtag", "main:file"])
        .git(["tag", "-a", "-m", "nested", "nested", "anno"])
        .git(["tag", "-a", "-m", "of a tree", "treetag", "main^{tree}"])
        .git(["branch", "dup", "main"]);
    fx
}

#[test]
fn export_options() {
    let mut fx = history();
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["main"],
        &["--all", "--tag-of-filtered-object=drop"],
        &["--all", "--mark-tags"],
        &["--all", "--mark-tags", "-M"],
        &["-C", "-C", "main"],
        &["--find-copies-harder", "-C", "main"],
        &["main~2..main"],
        &["--reference-excluded-parents", "main~2..main"],
        &["--show-original-ids", "main", "anno"],
        &["--no-data", "side"],
        &["--full-tree", "main~1..main"],
        &["--use-done-feature", "--progress=2", "main"],
        &[
            "--anonymize",
            "--all",
            "--mark-tags",
            "--anonymize-map=file:renamed",
        ],
        &[
            "--refspec",
            "refs/heads/*:refs/heads/copy/*",
            "main",
            "side",
        ],
        &["--refspec", ":refs/heads/gone"],
        &["main", "--", "file"],
        &["--tag-of-filtered-object=rewrite", "anno", "--", "big2"],
        &["--tag-of-filtered-object=rewrite", "^anno^{commit}", "anno"],
        &["--tag-of-filtered-object=drop", "side", "--", "d"],
        &["side", "^side"],
        &["--first-parent", "side"],
        &["HEAD", "dup", "light"],
        &["main~0"],
        &["blobtag"],
        &["treetag"],
        &["nested"],
        &["--end-of-options", "main"],
        &[],
        &["--anonymize-map=x:y", "main"],
        &["--signed-tags=bogus", "main"],
        &["--reencode=bogus", "main"],
        &["--tag-of-filtered-object=bogus", "main"],
    ];
    for opts in cases {
        let mut argv = vec!["fast-export"];
        argv.extend_from_slice(opts);
        fx.compare(&argv).checks(Check::OUTPUT).run();
    }
}

#[test]
fn export_marks() {
    let mut fx = history();
    if fx.is_skipped() {
        return;
    }
    fx.compare(["fast-export", "--export-marks=marks", "main~2"])
        .run();
    fx = fx.git(["fast-export", "--export-marks=marks", "main~2"]);
    fx.compare([
        "fast-export",
        "--import-marks=marks",
        "--export-marks=marks",
        "main",
    ])
    .run();
    fx.compare([
        "fast-export",
        "--import-marks=marks",
        "main",
        "--",
        "file",
        "file0",
    ])
    .checks(Check::OUTPUT)
    .run();
    fx.compare(["fast-export", "--import-marks-if-exists=none", "side"])
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn export_round_trip() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    // The stream of each tool, imported by rgit, gives the original ids.
    let stream = fx.git_stdout(["fast-export", "--all", "--mark-tags", "--signed-tags=strip"]);
    let mut fx = fx.git(["init", "-q", "--bare", "copy.git"]);
    fx.compare(["fast-import", "--quiet"])
        .repo("copy.git")
        .stdin(stream)
        .run();
}
