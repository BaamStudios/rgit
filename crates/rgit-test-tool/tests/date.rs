// SPDX-License-Identifier: MIT

//! `date` against the reference `test-tool`: every call in `data/date.txt`.

mod common;

#[test]
fn calls_match_reference() {
    common::run_cases(
        include_str!("data/date.txt"),
        &[("GIT_TEST_DATE_NOW", "1251660000"), ("TZ", "UTC")],
        80,
    );
}
