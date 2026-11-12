// SPDX-License-Identifier: MIT

//! `parse-options`, `parse-options-flags`, `parse-subcommand` against the
//! reference `test-tool`: every call in `data/parse-options.txt`.

mod common;

#[test]
fn calls_match_reference() {
    common::run_cases(
        include_str!("data/parse-options.txt"),
        &[("GIT_TEST_DISALLOW_ABBREVIATED_OPTIONS", "true")],
        200,
    );
}
