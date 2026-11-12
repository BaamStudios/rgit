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

/// `getnanos` reads the clock, so only its form is compared: seconds with
/// six decimals, and a duration when given a start.
#[test]
fn getnanos_form() {
    let run = |args: &[&str]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_rgit-test-tool"))
            .args(args)
            .output()
            .expect("run rgit-test-tool");
        assert!(out.status.success());
        String::from_utf8(out.stdout).expect("utf-8")
    };
    let form = |s: &str| {
        let s = s.strip_suffix('\n').expect("newline");
        let (int, frac) = s.split_once('.').expect("decimal point");
        assert!(
            int.trim_start_matches('-')
                .bytes()
                .all(|b| b.is_ascii_digit()),
            "{s}"
        );
        assert!(
            frac.len() == 6 && frac.bytes().all(|b| b.is_ascii_digit()),
            "{s}"
        );
        s.parse::<f64>().expect("number")
    };
    let start = run(&["date", "getnanos"]);
    assert!(form(&start) > 1.7e9);
    let dur = form(&run(&["date", "getnanos", start.trim()]));
    assert!((0.0..60.0).contains(&dur), "{dur}");
    assert!(form(&run(&["date", "getnanos", "abc"])) > 1.7e9);
}
