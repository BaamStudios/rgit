// SPDX-License-Identifier: MIT

//! Randomized differential tests: rgit's hunks must equal the hunks printed by
//! `git diff --no-index` for generated file pairs.

mod support;

use rgit_diff::{Algorithm, DiffOptions};
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

/// Like [`check`] for another algorithm (and anchors for patience). The
/// indent heuristic is off: it runs after every algorithm alike and has
/// its own tests (its few known differences, see #82, would otherwise show
/// up here too).
fn check_alg(name: &str, kind: Gen, seed: u64, count: usize, alg: &str, anchors: &[&str]) {
    if !have_git() {
        eprintln!("git not found, skipping");
        return;
    }
    let (algorithm, minimal) = Algorithm::parse(alg).unwrap();
    let opts = DiffOptions {
        algorithm,
        minimal,
        anchors: anchors.iter().map(|a| a.as_bytes().to_vec()).collect(),
        indent_heuristic: false,
        ..DiffOptions::default()
    };
    let mut extra = vec![
        "--no-indent-heuristic".to_string(),
        format!("--diff-algorithm={alg}"),
    ];
    extra.extend(anchors.iter().map(|a| format!("--anchored={a}")));
    let extra: Vec<&str> = extra.iter().map(String::as_str).collect();
    let r = run_cases(name, kind, seed, count, &opts, &extra, None);
    if let Some((s, want, got)) = r.mismatches.first() {
        panic!(
            "{alg}: {} of {} cases differ; first: seed {s}\n--- git\n{}\n--- rgit\n{}",
            r.mismatches.len(),
            r.cases,
            String::from_utf8_lossy(want),
            String::from_utf8_lossy(got)
        );
    }
}

#[test]
fn patience_tiny() {
    check_alg("p-tiny", Gen::Tiny, 400_000, 1500, "patience", &[]);
}

#[test]
fn patience_edit() {
    check_alg("p-edit", Gen::Edit, 410_000, 600, "patience", &[]);
}

#[test]
fn patience_code() {
    check_alg("p-code", Gen::Code, 420_000, 800, "patience", &[]);
}

/// Large inputs: stretches without unique lines fall back to Myers.
#[test]
fn patience_big() {
    check_alg("p-big", Gen::Big, 430_000, 60, "patience", &[]);
}

#[test]
fn anchored_code() {
    check_alg(
        "a-code",
        Gen::Code,
        440_000,
        800,
        "patience",
        &["fn", "  x"],
    );
}

#[test]
fn anchored_edit() {
    check_alg(
        "a-edit",
        Gen::Edit,
        450_000,
        400,
        "patience",
        &["line 1", "line"],
    );
}

#[test]
fn minimal_big() {
    check_alg("m-big", Gen::Big, 460_000, 60, "minimal", &[]);
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
    let alg = var("FUZZ_ALG", "myers");
    let (algorithm, minimal) = Algorithm::parse(&alg).expect("FUZZ_ALG");
    let anchors: Vec<String> = var("FUZZ_ANCHOR", "")
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let opts = DiffOptions {
        indent_heuristic: !nh,
        context: var("FUZZ_CTX", "3").parse().unwrap(),
        interhunk_context: var("FUZZ_IHC", "0").parse().unwrap(),
        algorithm: if anchors.is_empty() {
            algorithm
        } else {
            Algorithm::Patience
        },
        minimal,
        anchors: anchors.iter().map(|s| s.as_bytes().to_vec()).collect(),
    };
    let alg_arg = format!("--diff-algorithm={alg}");
    let anchor_args: Vec<String> = anchors.iter().map(|s| format!("--anchored={s}")).collect();
    let mut extra: Vec<&str> = if nh {
        vec!["--no-indent-heuristic"]
    } else {
        vec![]
    };
    extra.push(&alg_arg);
    extra.extend(anchor_args.iter().map(String::as_str));
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
