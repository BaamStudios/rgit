// SPDX-License-Identifier: MIT

//! `wildmatch` against the reference `test-tool`: every pair in
//! `data/wildmatch.txt` in all four modes, plus malformed calls.

mod common;

const MODES: &[&str] = &["wildmatch", "iwildmatch", "pathmatch", "ipathmatch"];

#[test]
fn pairs_match_reference() {
    let Some(mut fx) = common::fixture() else {
        return;
    };
    let mut n = 0;
    for line in include_str!("data/wildmatch.txt").lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let w = common::words(line);
        assert_eq!(w.len(), 2, "case `{line}`");
        for mode in MODES {
            let args = ["wildmatch", mode, &w[0], &w[1]];
            if let Err(mismatch) = fx.compare(args).output_only().try_run() {
                panic!("case `{mode} {line}`:\n{mismatch}");
            }
            n += 1;
        }
    }
    assert!(n >= 380, "only {n} cases");
}

#[test]
fn unknown_mode() {
    let Some(mut fx) = common::fixture() else {
        return;
    };
    common::same(&mut fx, &["wildmatch", "glob", "abc", "abc"]);
    common::same(&mut fx, &["wildmatch", "", "abc", "abc"]);
}
