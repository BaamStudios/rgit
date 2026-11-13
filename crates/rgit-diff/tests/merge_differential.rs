// SPDX-License-Identifier: MIT

//! Randomized differential tests of the three-way merge: rgit's result and
//! conflict count must equal `git merge-file -p` for generated triples, in
//! every conflict style and resolution mode.

mod support;

use std::path::Path;
use std::process::Command;

use rgit_diff::merge::{self, ConflictStyle, Favor, MergeOptions};
use support::{Gen, Rng, Scratch, generate_triple, have_git};

const STYLES: [(ConflictStyle, &str); 3] = [
    (ConflictStyle::Merge, "--no-diff3"),
    (ConflictStyle::Diff3, "--diff3"),
    (ConflictStyle::ZealousDiff3, "--zdiff3"),
];

fn git_merge_file(dir: &Path, args: &[&str]) -> (Vec<u8>, i32) {
    let out = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CEILING_DIRECTORIES", dir.parent().unwrap())
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir.join(".config"))
        .env("LC_ALL", "C")
        .args(["merge-file", "-p", "-L", "o", "-L", "b", "-L", "t"])
        .args(args)
        .args(["o", "b", "t"])
        .output()
        .expect("run git");
    (out.stdout, out.status.code().unwrap_or(-1))
}

/// A mismatching case: seed, options, git's and rgit's output.
type Mismatch = (u64, String, Vec<u8>, Vec<u8>);

fn run(name: &str, kind: Gen, seed: u64, count: usize) -> (usize, Vec<Mismatch>) {
    let scratch = Scratch::new(name);
    let dir = &scratch.0;
    let mut cases = 0;
    let mut bad = Vec::new();
    for s in seed..seed + count as u64 {
        let mut rng = Rng::new(s);
        let (b, o, t) = generate_triple(kind, &mut rng);
        std::fs::write(dir.join("b"), &b).unwrap();
        std::fs::write(dir.join("o"), &o).unwrap();
        std::fs::write(dir.join("t"), &t).unwrap();
        let favor = match rng.below(8) {
            0 => (Favor::Ours, Some("--ours")),
            1 => (Favor::Theirs, Some("--theirs")),
            2 => (Favor::Union, Some("--union")),
            _ => (Favor::None, None),
        };
        for (style, flag) in STYLES {
            let mut args = vec![flag];
            args.extend(favor.1);
            let (want, code) = git_merge_file(dir, &args);
            let opts = MergeOptions {
                style,
                favor: favor.0,
                ours_label: b"o",
                base_label: b"b",
                theirs_label: b"t",
                diff: merge::merge_diff_options(),
                ..MergeOptions::default()
            };
            let got = merge::merge(&b, &o, &t, &opts).unwrap();
            cases += 1;
            if got.content != want || got.conflicts.min(127) as i32 != code {
                bad.push((s, args.join(" "), want, got.content));
            }
        }
    }
    (cases, bad)
}

fn check(name: &str, kind: Gen, seed: u64, count: usize) {
    if !have_git() {
        eprintln!("git not found, skipping");
        return;
    }
    let (cases, bad) = run(name, kind, seed, count);
    if let Some((s, args, want, got)) = bad.first() {
        panic!(
            "{} of {cases} merges differ; first: seed {s} ({args})\n--- git\n{}\n--- rgit\n{}",
            bad.len(),
            String::from_utf8_lossy(want),
            String::from_utf8_lossy(got)
        );
    }
}

#[test]
fn tiny() {
    check("merge-tiny", Gen::Tiny, 1, 400);
}

#[test]
fn edit() {
    check("merge-edit", Gen::Edit, 10_000, 200);
}

#[test]
fn code() {
    check("merge-code", Gen::Code, 20_000, 300);
}

/// Exploration entry point: `FUZZ_GEN` (tiny|edit|code), `FUZZ_SEED`,
/// `FUZZ_COUNT`, `FUZZ_KEEP` (directory for mismatching cases).
#[test]
#[ignore]
fn explore() {
    let var = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
    let kind = Gen::parse(&var("FUZZ_GEN", "tiny"));
    let seed: u64 = var("FUZZ_SEED", "1").parse().unwrap();
    let count: usize = var("FUZZ_COUNT", "1000").parse().unwrap();
    let (cases, bad) = run("merge-explore", kind, seed, count);
    if let Ok(k) = std::env::var("FUZZ_KEEP") {
        let k = Path::new(&k);
        std::fs::create_dir_all(k).unwrap();
        for (s, args, want, got) in bad.iter().take(20) {
            let mut rng = Rng::new(*s);
            let (b, o, t) = generate_triple(kind, &mut rng);
            std::fs::write(k.join(format!("{s}.b")), &b).unwrap();
            std::fs::write(k.join(format!("{s}.o")), &o).unwrap();
            std::fs::write(k.join(format!("{s}.t")), &t).unwrap();
            std::fs::write(k.join(format!("{s}.args")), args).unwrap();
            std::fs::write(k.join(format!("{s}.want")), want).unwrap();
            std::fs::write(k.join(format!("{s}.got")), got).unwrap();
        }
    }
    let seeds: Vec<(u64, &str)> = bad.iter().map(|m| (m.0, m.1.as_str())).take(20).collect();
    eprintln!("RESULT {} / {cases} mismatches; {seeds:?}", bad.len());
}
