// SPDX-License-Identifier: MIT
//! Split index (`link` extension, `sharedindex.<hash>`): rgit reads git's
//! split indexes and writes the same files for the same operations. Each
//! step runs with both binaries and starts from git's result of the step
//! before; entries come from `--cacheinfo` so that no stat data differs.

use rgit_difftest::{Check, Fixture};

const A: &str = "1111111111111111111111111111111111111111";
const B: &str = "2222222222222222222222222222222222222222";

/// Output, the index file (bytes; it names its shared index by checksum)
/// and the index as git lists it.
const CHECKS: [Check; 5] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Index,
    Check::GitFiles,
];

fn fixture() -> Option<Fixture> {
    let fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"));
    if fx.is_skipped() {
        return None;
    }
    let mut fx = fx.git(["init", "-q"]);
    for (p, id) in [("one", A), ("two", A), ("d/three", B)] {
        fx = fx.git([
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{id},{p}"),
        ]);
    }
    Some(fx)
}

fn step(fx: &mut Fixture, args: &[&str]) {
    fx.compare(args).checks(CHECKS).git_file("index").run();
}

#[test]
fn split_and_edit() {
    let Some(mut fx) = fixture() else { return };
    let info = |id: &str, p: &str| format!("100644,{id},{p}");
    step(
        &mut fx,
        &[
            "-c",
            "splitIndex.maxPercentChange=100",
            "update-index",
            "--split-index",
        ],
    );
    for args in [
        vec!["update-index", "--cacheinfo", &info(B, "two")],
        vec!["update-index", "--add", "--cacheinfo", &info(A, "new")],
        vec!["update-index", "--force-remove", "one"],
        vec!["update-index", "--add", "--cacheinfo", &info(A, "one")],
        vec!["ls-files", "-s"],
        vec!["rev-parse", "--shared-index-path"],
    ] {
        let mut argv = vec!["-c", "splitIndex.maxPercentChange=100"];
        argv.extend(args.iter().copied());
        step(&mut fx, &argv);
    }
    // Pushing everything into a new shared index, then back to whole.
    step(&mut fx, &["update-index", "--split-index"]);
    step(
        &mut fx,
        &[
            "-c",
            "core.splitIndex=true",
            "update-index",
            "--no-split-index",
        ],
    );
    step(&mut fx, &["rev-parse", "--shared-index-path"]);
}

#[test]
fn split_config() {
    let Some(mut fx) = fixture() else { return };
    let info = |id: &str, p: &str| format!("100644,{id},{p}");
    for (config, path) in [
        ("core.splitIndex=true", "x1"),
        ("core.splitIndex=true", "x2"),
        ("splitIndex.maxPercentChange=0", "x3"),
        ("core.splitIndex=false", "x4"),
    ] {
        let mut argv = vec!["-c", "core.splitIndex=true", "-c", config];
        if config.starts_with("core.") {
            argv = vec!["-c", config];
        }
        let i = info(A, path);
        argv.extend(["update-index", "--add", "--cacheinfo", &i]);
        step(&mut fx, &argv);
    }
    step(
        &mut fx,
        &[
            "-c",
            "core.splitIndex=false",
            "update-index",
            "--split-index",
        ],
    );
}
