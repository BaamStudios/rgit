// SPDX-License-Identifier: MIT

//! Differential harness: run the same `test-tool` call with the reference
//! binary and with `rgit-test-tool`, compare stdout, stderr and exit code.
//!
//! The reference `test-tool` is found as
//! 1. `$RGIT_REFERENCE_TEST_TOOL`,
//! 2. `t/helper/test-tool` in `$RGIT_REFERENCE_BUILD`,
//! 3. `<cache>/git-v2.56.1/t/helper/test-tool`, where `<cache>` is
//!    `$RGIT_CONFORMANCE_CACHE` or `~/.cache/rgit-conformance` (the build that
//!    rgit-conformance's `run` script makes),
//! 4. `upstream/git/t/helper/test-tool` in the workspace.
//!
//! Without one the tests are skipped, unless `RGIT_REQUIRE_REFERENCE=1`.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

pub use rgit_difftest::Fixture;

/// Reference git version whose `test-tool` defines the behaviour.
const REFERENCE_VERSION: &str = "v2.56.1";

fn executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// The reference `test-tool`, if one is available.
pub fn reference_test_tool() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("RGIT_REFERENCE_TEST_TOOL").filter(|v| !v.is_empty()) {
        let p = PathBuf::from(p);
        assert!(
            executable(&p),
            "RGIT_REFERENCE_TEST_TOOL={} is not executable",
            p.display()
        );
        return Some(p);
    }
    let mut candidates = Vec::new();
    if let Some(b) = std::env::var_os("RGIT_REFERENCE_BUILD").filter(|v| !v.is_empty()) {
        candidates.push(PathBuf::from(b).join("t/helper/test-tool"));
    }
    let cache = std::env::var_os("RGIT_CONFORMANCE_CACHE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache/rgit-conformance"))
        });
    if let Some(cache) = cache {
        candidates.push(cache.join(format!("git-{REFERENCE_VERSION}/t/helper/test-tool")));
    }
    candidates
        .push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../upstream/git/t/helper/test-tool"));
    candidates.into_iter().find(|p| executable(p))
}

/// A fixture whose "reference git" is the reference `test-tool` and whose
/// binary under test is `rgit-test-tool`, or `None` (test skipped).
pub fn fixture() -> Option<Fixture> {
    match reference_test_tool() {
        Some(reference) => {
            Some(Fixture::with_reference(reference).rgit(env!("CARGO_BIN_EXE_rgit-test-tool")))
        }
        None => {
            assert!(
                std::env::var_os("RGIT_REQUIRE_REFERENCE").is_none_or(|v| v != "1"),
                "no reference test-tool found and RGIT_REQUIRE_REFERENCE=1"
            );
            eprintln!("skipping: no reference test-tool (set RGIT_REFERENCE_TEST_TOOL)");
            None
        }
    }
}

/// Compares one call (stdout, stderr, exit code).
pub fn same(fx: &mut Fixture, args: &[&str]) {
    fx.compare(args).output_only().run();
}

/// Compares one call with `stdin`.
pub fn same_stdin(fx: &mut Fixture, args: &[&str], stdin: &[u8]) {
    fx.compare(args).output_only().stdin(stdin.to_vec()).run();
}

/// Splits a case line into words: whitespace separates, double quotes group
/// (and are removed), a backslash keeps the next character, `""` is an
/// empty word.
pub fn words(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quoted = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
                in_word = true;
            }
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

/// Replays every call of a case file (one call per line: optional
/// `VAR=value` words, then the arguments; `#` starts a comment line)
/// against both binaries. `defaults` is the environment of every call,
/// which the `VAR=value` words override. Fails if fewer than `min` calls
/// ran (a guard against a truncated file).
pub fn run_cases(cases: &str, defaults: &[(&str, &str)], min: usize) {
    let Some(mut fx) = fixture() else { return };
    let mut n = 0;
    for line in cases.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut w = words(line);
        let mut env: Vec<(String, String)> = defaults
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        while w
            .first()
            .is_some_and(|a| !a.starts_with('-') && a.contains('='))
        {
            let word = w.remove(0);
            let (k, v) = word.split_once('=').expect("contains '='");
            env.retain(|(ek, _)| ek != k);
            env.push((k.to_string(), v.to_string()));
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
    assert!(n >= min, "only {n} cases");
}

#[test]
fn word_splitting() {
    assert_eq!(
        words(r#"a --expect="x: 1" -s "" b\ c"#),
        ["a", "--expect=x: 1", "-s", "", "b c"]
    );
}
