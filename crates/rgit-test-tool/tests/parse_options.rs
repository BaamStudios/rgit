// SPDX-License-Identifier: MIT

//! `parse-options`, `parse-options-flags`, `parse-subcommand` against the
//! reference `test-tool`: every call in `data/parse-options.txt`.

mod common;

use common::fixture;

const CASES: &str = include_str!("data/parse-options.txt");

/// Splits a case line into words: whitespace separates, double quotes group
/// (and are removed), `""` is an empty word.
fn words(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                in_word = true;
            }
            c if c.is_whitespace() && !quoted => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    assert!(!quoted, "unbalanced quotes in case: {line}");
    if in_word {
        out.push(cur);
    }
    out
}

#[test]
fn word_splitting() {
    assert_eq!(
        words(r#"a --expect="x: 1" -s "" b"#),
        ["a", "--expect=x: 1", "-s", "", "b"]
    );
}

#[test]
fn calls_match_reference() {
    let Some(mut fx) = fixture() else { return };
    let mut n = 0;
    for line in CASES.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut w = words(line);
        let mut env = vec![(
            "GIT_TEST_DISALLOW_ABBREVIATED_OPTIONS".to_string(),
            "true".to_string(),
        )];
        while w
            .first()
            .is_some_and(|a| !a.starts_with('-') && a.contains('='))
        {
            let (k, v) = w
                .remove(0)
                .split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .unwrap();
            env.retain(|(ek, _)| *ek != k);
            env.push((k, v));
        }
        let mut cmp = fx.compare(&w).output_only();
        for (k, v) in &env {
            cmp = cmp.env(k, v);
        }
        if let Err(mismatch) = cmp.try_run() {
            panic!("case `{line}`:\n{mismatch}");
        }
        n += 1;
    }
    assert!(n > 200, "only {n} cases");
}
