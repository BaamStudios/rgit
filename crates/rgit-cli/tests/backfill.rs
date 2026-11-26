// SPDX-License-Identifier: MIT
//! Differential tests of `git backfill` (#640): the blobs a backfill
//! downloads into a blobless clone, the batches it asks for, and its
//! errors.
//!
//! The server side (`upload-pack` and the `pack-objects` it runs) is the
//! reference git on both sides: `PATH` starts with its directory.

use rgit_difftest::{Check, Fixture, Normalizer};

mod common;
use common::servers::server_path;

/// `src`: two rounds of commits changing four files in each of three
/// directories, served with filters allowed; `bf`: its blobless clone
/// without checkout.
fn fixture() -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main", "src"]);
    if fx.is_skipped() {
        return fx;
    }
    for round in 1..=2 {
        for n in 1..=4 {
            for dir in ["", "a/", "a/b/"] {
                fx = fx.write(
                    format!("src/{dir}file.{n}.txt"),
                    format!("round {round} of {dir}{n}\n"),
                );
            }
            fx = fx
                .git(["-C", "src", "add", "."])
                .git(["-C", "src", "commit", "-q", "-m", &format!("{round}.{n}")])
                .tick();
        }
    }
    let fx = fx
        .git(["-C", "src", "config", "uploadpack.allowfilter", "1"])
        .git(["-C", "src", "config", "uploadpack.allowanysha1inwant", "1"]);
    let url = format!("file://{}/src", fx.work_dir().display());
    fx.git([
        "clone",
        "-q",
        "--no-checkout",
        "--filter=blob:none",
        &url,
        "bf",
    ])
}

/// What a backfill into a new blobless clone `dir` leaves behind: the
/// objects (pack names differ with the batches only), and what it says.
fn backfill(fx: Fixture, dir: &str, args: &[&str]) -> Fixture {
    let url = format!("file://{}/src", fx.work_dir().display());
    let mut fx = fx.git([
        "clone",
        "-q",
        "--no-checkout",
        "--filter=blob:none",
        &url,
        dir,
    ]);
    let path = server_path(&fx);
    let mut argv = vec!["-C", dir, "backfill"];
    argv.extend_from_slice(args);
    fx.compare(&argv)
        .env("PATH", &path)
        .repo(dir)
        .normalize_state(Normalizer::pack_names())
        .checks([
            Check::ExitCode,
            Check::Stdout,
            Check::Stderr,
            Check::Objects,
        ])
        .run();
    fx
}

#[test]
fn backfill_downloads_same_blobs_as_git() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    for (i, args) in [
        &[][..],
        &["--min-batch-size=5"],
        &["HEAD~2..HEAD"],
        &["--no-include-edges", "HEAD~2..HEAD"],
        &["HEAD~1", "^HEAD~3"],
        &["-n2", "HEAD"],
        &["--first-parent", "HEAD~5"],
        &["HEAD", "--", "a/b"],
        &["--", "a/file.*"],
        &["--objects", "--filter=blob:none"],
        &["--objects", "--filter=object:type=blob", "HEAD~6"],
    ]
    .into_iter()
    .enumerate()
    {
        fx = backfill(fx, &format!("bf{i}"), args);
    }
}

#[test]
fn backfill_errors_same_as_git() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["-C", "bf", "backfill", "unexpected-arg"][..],
        &["-C", "bf", "backfill", "--all", "--bogus", "--first-parent"],
        &["-C", "bf", "backfill", "--objects", "--filter=tree:1"],
        &[
            "-C",
            "bf",
            "backfill",
            "--objects",
            "--filter=blob:limit=1m",
        ],
        &["-C", "bf", "backfill", "--filter=blob:none"],
        &[
            "-C",
            "bf",
            "backfill",
            "--objects",
            "--filter=sparse:oid=HEAD:x",
        ],
        &["-C", "bf", "backfill", "--min-batch-size=x"],
        &["-C", "bf", "backfill", "--sparse"],
        &["-C", "src", "backfill"],
        &["-C", "bf", "backfill", "-h"],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
}

/// The batches: one fetch per batch of at least `--min-batch-size` blobs,
/// counted in trace2 like git (`promisor/fetch_count`), and the walk's
/// `path-walk/paths`.
#[test]
fn backfill_batches_like_git() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let path = server_path(&fx);
    let trace = |program: &std::path::Path, dir: &str| -> Vec<String> {
        let work = fx.work_dir();
        let url = format!("file://{}/src", work.display());
        let clone = std::process::Command::new(fx.reference_git())
            .args([
                "clone",
                "-q",
                "--no-checkout",
                "--filter=blob:none",
                &url,
                dir,
            ])
            .current_dir(&work)
            .env("HOME", fx.home_dir())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("XDG_CONFIG_HOME", fx.home_dir().join(".config"))
            .status()
            .unwrap();
        assert!(clone.success());
        let file = work.join(format!("{dir}.trace"));
        let out = std::process::Command::new(program)
            .args(["-C", dir, "backfill", "--min-batch-size=10"])
            .current_dir(&work)
            .env("PATH", &path)
            .env("HOME", fx.home_dir())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("XDG_CONFIG_HOME", fx.home_dir().join(".config"))
            .env("GIT_TRACE2_EVENT", &file)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = std::fs::read_to_string(&file).unwrap();
        text.lines()
            .filter(|l| l.contains("\"event\":\"data\""))
            .filter_map(|l| {
                let key = l.split("\"key\":\"").nth(1)?.split('"').next()?;
                let value = l.split("\"value\":\"").nth(1)?.split('"').next()?;
                matches!(key, "fetch_count" | "paths" | "commits").then(|| format!("{key}={value}"))
            })
            .collect()
    };
    let git = trace(fx.reference_git(), "with-git");
    let ours = trace(
        std::path::Path::new(env!("CARGO_BIN_EXE_rgit")),
        "with-rgit",
    );
    assert_eq!(git, ours);
}
