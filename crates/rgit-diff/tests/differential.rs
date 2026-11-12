// SPDX-License-Identifier: MIT

//! Randomized differential tests: rgit's hunks must equal the hunks printed by
//! `git diff --no-index` for generated file pairs.

mod support;

use rgit_diff::DiffOptions;
use support::{Gen, have_git, run_cases};

fn check(name: &str, kind: Gen, seed: u64, count: usize, heuristic: bool) {
    if !have_git() {
        eprintln!("git not found, skipping");
        return;
    }
    let opts = DiffOptions {
        indent_heuristic: heuristic,
        ..DiffOptions::default()
    };
    let extra: &[&str] = if heuristic {
        &[]
    } else {
        &["--no-indent-heuristic"]
    };
    let r = run_cases(name, kind, seed, count, &opts, extra, None);
    if let Some((s, want, got)) = r.mismatches.first() {
        panic!(
            "{} of {} cases differ; first: seed {s}\n--- git\n{}\n--- rgit\n{}",
            r.mismatches.len(),
            r.cases,
            String::from_utf8_lossy(want),
            String::from_utf8_lossy(got)
        );
    }
}

#[test]
fn tiny_no_heuristic() {
    check("tiny-nh", Gen::Tiny, 1, 1500, false);
}

#[test]
fn tiny() {
    check("tiny", Gen::Tiny, 100_000, 1500, true);
}

#[test]
fn edit() {
    check("edit", Gen::Edit, 200_000, 600, true);
}

#[test]
fn code() {
    check("code", Gen::Code, 300_000, 1500, true);
}

/// Exploration entry point, configured through environment variables:
/// `FUZZ_GEN` (tiny|edit|code), `FUZZ_SEED`, `FUZZ_COUNT`, `FUZZ_NH=1`
/// (no indent heuristic), `FUZZ_KEEP` (directory for mismatching cases).
#[test]
#[ignore]
fn explore() {
    let var = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
    let kind = Gen::parse(&var("FUZZ_GEN", "tiny"));
    let seed: u64 = var("FUZZ_SEED", "1").parse().unwrap();
    let count: usize = var("FUZZ_COUNT", "1000").parse().unwrap();
    let nh = var("FUZZ_NH", "0") == "1";
    let keep = std::env::var("FUZZ_KEEP")
        .ok()
        .map(std::path::PathBuf::from);
    let opts = DiffOptions {
        indent_heuristic: !nh,
        context: var("FUZZ_CTX", "3").parse().unwrap(),
        ..DiffOptions::default()
    };
    let mut extra: Vec<&str> = if nh {
        vec!["--no-indent-heuristic"]
    } else {
        vec![]
    };
    let more = var("FUZZ_ARGS", "");
    extra.extend(more.split_whitespace());
    let r = run_cases("explore", kind, seed, count, &opts, &extra, keep.as_deref());
    let seeds: Vec<u64> = r.mismatches.iter().map(|m| m.0).take(20).collect();
    eprintln!(
        "RESULT {} / {} mismatches; seeds {:?}",
        r.mismatches.len(),
        r.cases,
        seeds
    );
}
