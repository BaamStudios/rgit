// SPDX-License-Identifier: MIT
//! Differential tests of `submodule.alternateLocation` /
//! `submodule.alternateErrorStrategy` (#115): `clone --recursive
//! --reference[-if-able]` sets them, `submodule update` borrows from the
//! reference's `modules/<name>` or reports the missing one.

use rgit_difftest::{Check, Fixture};

const CHECKS: [Check; 3] = [Check::ExitCode, Check::Stdout, Check::Stderr];

/// `super2` is a clone of `super` without its submodule; `super3` has it.
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["config", "--global", "protocol.file.allow", "always"])
        .git(["init", "-q", "-b", "main", "sub"])
        .write("sub/file1", "f\n")
        .git(["-C", "sub", "add", "."])
        .git(["-C", "sub", "commit", "-q", "-m", "s"])
        .git(["init", "-q", "-b", "main", "super"])
        .git(["-C", "super", "submodule", "add", "-q", "../sub", "sub"])
        .git(["-C", "super", "commit", "-q", "-m", "add"])
        .git(["clone", "-q", "super", "super2"])
        .git(["clone", "-q", "--recurse-submodules", "super", "super3"])
}

#[test]
fn submodule_alternates_same_as_git() {
    let cases: &[(&[&str], &str)] = &[
        (
            &[
                "clone",
                "-q",
                "--recursive",
                "--reference",
                "super2",
                "super2",
                "c",
            ],
            "c",
        ),
        (
            &[
                "clone",
                "-q",
                "--reference-if-able",
                "super2",
                "--recursive",
                "super2",
                "c",
            ],
            "c",
        ),
        (
            &[
                "clone",
                "-q",
                "--recursive",
                "--reference",
                "super3",
                "super3",
                "c",
            ],
            "c",
        ),
        (
            &[
                "-c",
                "submodule.alternateLocation=bogus",
                "clone",
                "-q",
                "--recursive",
                "--reference",
                "super3",
                "super3",
                "c",
            ],
            "c",
        ),
    ];
    for (argv, repo) in cases {
        let fx = fixture();
        if fx.is_skipped() {
            return;
        }
        let mut fx = fx;
        fx.compare(*argv).repo(*repo).checks(CHECKS).run();
    }
    // `submodule update` in a clone made with --reference.
    for strategy in ["die", "info", "ignore", "bogus"] {
        let fx = fixture()
            .git(["clone", "-q", "--reference", "super2", "super2", "c"])
            .git([
                "-C",
                "c",
                "config",
                "submodule.alternateLocation",
                "superproject",
            ])
            .git([
                "-C",
                "c",
                "config",
                "submodule.alternateErrorStrategy",
                strategy,
            ]);
        if fx.is_skipped() {
            return;
        }
        let mut fx = fx;
        fx.compare(["submodule", "update", "--init"])
            .in_dir("c")
            .repo("c")
            .checks(CHECKS)
            .run();
    }
}
