// SPDX-License-Identifier: MIT
//! Differential tests of `--[no-]expand-tabs[=<n>]` (#405).

use rgit_difftest::{Check, Fixture};

#[test]
fn expand_tabs() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "repo"])
        .git([
            "-C",
            "repo",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "sub\tject\n\nab\tc\td\n\tx",
        ]);
    if fx.is_skipped() {
        return;
    }
    for opts in [
        &[][..],
        &["--expand-tabs"],
        &["--no-expand-tabs"],
        &["--expand-tabs=4"],
        &["--expand-tabs=0"],
        &["--pretty=full", "--expand-tabs=3"],
        &["--pretty=raw"],
    ] {
        let mut args = vec!["log"];
        args.extend_from_slice(opts);
        fx.compare(&args).in_dir("repo").checks(Check::OUTPUT).run();
    }
}
