// SPDX-License-Identifier: MIT
//! Differential tests of the local progress meters (#181): `Receiving
//! objects` / `Resolving deltas` of clone and `Updating files` of checkout,
//! written to a pipe with `--progress`.

use rgit_difftest::{Check, Fixture};

#[test]
fn clone_and_checkout_meters() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "src"]);
    for i in 1..=6 {
        let lines: String = (i..60).map(|n| format!("{n}\n")).collect();
        fx = fx
            .write(format!("src/f{i}"), lines.as_bytes())
            .git(["-C", "src", "add", "."])
            .git(["-C", "src", "commit", "-q", "-m", &format!("c{i}")]);
    }
    if fx.is_skipped() {
        return;
    }
    let url = format!("file://{}/src", fx.work_dir().display());
    // The server side differs (in this harness `git-upload-pack` may be
    // rgit), so the clone meters are compared by the conformance suite;
    // here the clone only prepares the checkout.
    fx = fx.git(["clone", "-q", &url, "dst"]);
    fx.compare(["checkout", "--progress", "-b", "x", "HEAD~3"])
        .in_dir("dst")
        .env("GIT_PROGRESS_DELAY", "0")
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["clone", "--no-progress", &url, "quiet"])
        .checks(Check::OUTPUT)
        .run();
}
