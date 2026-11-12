// SPDX-License-Identifier: MIT
//! `check-ref-format` against the reference git (output and exit code).

use rgit_difftest::{Check, Fixture};

#[test]
fn check_ref_format_same_as_git() {
    let mut fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"));
    let cases: &[&[&str]] = &[
        &[],
        &["-h"],
        &["--foo", "bar"],
        &["refs/heads/x", "extra"],
        &["refs/heads/x"],
        &["heads"],
        &["--allow-onelevel", "heads"],
        &["--allow-onelevel", "--no-allow-onelevel", "heads"],
        &["refs/heads/a..b"],
        &["refs/heads/*"],
        &["--refspec-pattern", "refs/heads/*"],
        &["--refspec-pattern", "refs/*/*"],
        &["--normalize", "//refs//heads/x"],
        &["--print", "/refs/heads/x"],
        &["--normalize", "refs/heads/x/"],
        &["--normalize", "refs/heads/x", "--allow-onelevel"],
        &["--", "refs/heads/x"],
        &["--branch"],
        &["--branch", "main"],
        &["--branch", "-main"],
        &["--branch", "HEAD"],
        &["--branch", "a..b"],
        &["--branch", "a", "b"],
        &["--normalize", "--branch", "x"],
    ];
    for args in cases {
        let mut argv = vec!["check-ref-format"];
        argv.extend_from_slice(args);
        fx.compare(&argv).checks(Check::OUTPUT).run();
    }
}
