// SPDX-License-Identifier: MIT
//! Differential tests of the `rgit` binary against the reference git.
//! See the `rgit-difftest` crate documentation for the harness.

use rgit_difftest::{Check, Fixture, Normalizer};

fn fixture() -> Fixture {
    Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"))
}

#[test]
fn version_exit_code() {
    fixture().compare(["--version"]).only_exit_code().run();
}

#[test]
fn version_subcommand_exit_code() {
    fixture().compare(["version"]).only_exit_code().run();
}

/// Full output of `--version`: the version number itself is an accepted
/// difference (CLEANROOM.md, "Version string").
#[test]
#[ignore = "rgit does not print the `git version ...rgit...` line yet"]
fn version_output() {
    fixture()
        .compare(["--version"])
        .checks(Check::OUTPUT)
        .normalize_stdout(Normalizer::version_line())
        .run();
}
