// SPDX-License-Identifier: MIT
//! `ls-remote` over the local transport (path and `file://`) against the
//! reference git: output and exit code.

use rgit_difftest::{Check, Fixture};

#[test]
fn ls_remote_same_as_git() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main", "r"])
        .git(["-C", "r", "commit", "-q", "--allow-empty", "-m", "a"])
        .git(["-C", "r", "tag", "-a", "v1", "-m", "t"])
        .git(["-C", "r", "tag", "lw"])
        .git(["-C", "r", "branch", "dev"]);
    if fx.is_skipped() {
        return;
    }
    let url = format!("file://{}", fx.work_dir().join("r").display());
    let cases: &[&[&str]] = &[
        &["r"],
        &[&url],
        &["--symref", "r"],
        &["--refs", "r"],
        &["--symref", "--refs", "r"],
        &["--tags", "r"],
        &["--heads", "r"],
        &["-b", "r", "main"],
        &["r", "v1"],
        &["r", "refs/heads/*"],
        &["--exit-code", "r", "nope"],
        &["-q", "r"],
        &["--get-url", "r"],
    ];
    for args in cases {
        let mut argv = vec!["ls-remote"];
        argv.extend_from_slice(args);
        fx.compare(&argv).checks(Check::OUTPUT).run();
    }
}
