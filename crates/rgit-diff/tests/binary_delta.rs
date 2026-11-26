// SPDX-License-Identifier: MIT

//! Differential tests of `GIT binary patch` blocks: the hunks rgit writes
//! (literal or delta, and every delta instruction) must equal those of
//! `git diff --no-index --binary` byte for byte.
//!
//! The inputs cover the cases the encoder's rules were derived from:
//! random data with moved and edited pieces, runs and short periods (equal
//! windows, thinned hash buckets), copies longer than 64 KiB and the
//! choice between delta and literal near the break-even point.

mod support;

use std::process::Command;

use support::{Rng, Scratch, have_git};

fn git_binary_block(dir: &std::path::Path, a: &[u8], b: &[u8]) -> Vec<u8> {
    std::fs::write(dir.join("a"), a).unwrap();
    std::fs::write(dir.join("b"), b).unwrap();
    let out = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir.join(".config"))
        .args(["diff", "--no-index", "--binary", "a", "b"])
        .output()
        .unwrap();
    let text = out.stdout;
    let at = text
        .windows(16)
        .position(|w| w == b"GIT binary patch")
        .expect("binary patch");
    text[at..].to_vec()
}

/// The hunks of a binary patch block: kind line and inflated data.
fn hunks(block: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    let mut lines = block.split(|&c| c == b'\n').peekable();
    while let Some(l) = lines.next() {
        if l.starts_with(b"literal ") || l.starts_with(b"delta ") {
            let mut zipped = Vec::new();
            while let Some(d) = lines.next_if(|d| !d.is_empty()) {
                zipped.extend(rgit_diff::binary::decode_line(d).expect("data line"));
            }
            let data = rgit_diff::binary::inflate(&zipped).expect("zlib");
            out.push((l.to_vec(), data));
        }
    }
    out
}

/// Whether the `git` binary compresses like rgit (both on zlib-ng
/// output); with another zlib the compressed bytes and therefore the
/// literal/delta choice near the break-even point can differ.
fn same_zlib(dir: &std::path::Path) -> bool {
    let data: Vec<u8> = (0..5000u32).map(|i| (i * i % 251) as u8).collect();
    let want = git_binary_block(dir, b"\0", &data);
    let mut got = Vec::new();
    rgit_diff::binary::write_binary_patch(&mut got, b"\0", &data);
    want == got
}

/// Compares the blocks: exactly with the same zlib, else hunk by hunk
/// (inflated contents) wherever both chose the same kind of hunk.
fn same_block(exact: bool, want: &[u8], got: &[u8]) -> bool {
    if exact {
        return want == got;
    }
    hunks(want).iter().zip(hunks(got)).all(|(w, g)| {
        w.0.split(|&c| c == b' ').next() != g.0.split(|&c| c == b' ').next() || *w == g
    })
}

fn bytes(rng: &mut Rng, n: usize, runs: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    let alpha = if runs { 2 + rng.below(15) } else { 256 };
    while out.len() < n {
        if runs && rng.chance(4, 10) {
            let b = rng.below(alpha) as u8;
            out.extend(std::iter::repeat_n(b, 1 + rng.below(300)));
        } else if runs && rng.chance(5, 10) {
            let pat: Vec<u8> = (0..1 + rng.below(20))
                .map(|_| rng.below(alpha) as u8)
                .collect();
            for _ in 0..1 + rng.below(40) {
                out.extend_from_slice(&pat);
            }
        } else {
            out.extend((0..1 + rng.below(50)).map(|_| rng.below(alpha) as u8));
        }
    }
    out.truncate(n);
    out
}

fn pair(rng: &mut Rng, max: usize, max_piece: usize, runs: bool) -> (Vec<u8>, Vec<u8>) {
    let n = rng.below(max);
    let mut src = bytes(rng, n, runs);
    src.push(0);
    let mut tgt = Vec::new();
    while (tgt.len() as f64) < n as f64 * 1.2 && !rng.chance(1, 20) {
        let r = rng.below(10);
        if r < 6 {
            let o = rng.below(src.len());
            let l = 1 + rng.below(max_piece);
            tgt.extend_from_slice(&src[o..(o + l).min(src.len())]);
        } else if r < 8 {
            let l = 1 + rng.below(200);
            tgt.extend(bytes(rng, l, runs));
        } else {
            tgt.extend_from_slice(&src[..rng.below(src.len())]);
        }
    }
    tgt.push(0);
    (src, tgt)
}

fn check(name: &str, seed: u64, count: usize, max: usize, max_piece: usize, runs: bool) {
    if !have_git() {
        eprintln!("git not found, skipping");
        return;
    }
    let dir = Scratch::new(name);
    let exact = same_zlib(&dir.0);
    let mut rng = Rng::new(seed);
    let mut bad = 0;
    for i in 0..count {
        let (a, b) = pair(&mut rng, max, max_piece, runs);
        let want = git_binary_block(&dir.0, &a, &b);
        let mut got = Vec::new();
        rgit_diff::binary::write_binary_patch(&mut got, &a, &b);
        if !same_block(exact, &want, &got) {
            bad += 1;
            eprintln!(
                "case {i} differs\n--- git\n{}\n--- rgit\n{}",
                String::from_utf8_lossy(&want),
                String::from_utf8_lossy(&got)
            );
        }
    }
    assert_eq!(bad, 0, "{bad} of {count} cases differ");
}

#[test]
fn random_data() {
    check("bin-rand", 1, 150, 3000, 400, false);
}

#[test]
fn runs_and_periods() {
    check("bin-runs", 2, 150, 3000, 400, true);
}

#[test]
fn large_inputs() {
    check("bin-large", 3, 6, 300_000, 100_000, false);
    check("bin-large-runs", 4, 6, 100_000, 30_000, true);
}

#[test]
fn full_hash_bucket() {
    // 70 runs of one 16-byte block, each followed by a separator: the
    // block's bucket holds 70 entries and is thinned to 64.
    if !have_git() {
        return;
    }
    let dir = Scratch::new("bin-bucket");
    let exact = same_zlib(&dir.0);
    let x: Vec<u8> = (0..16u8).map(|i| i.wrapping_mul(37) ^ 0x5a).collect();
    let sep: Vec<u8> = (1..=16u8).collect();
    let mut src = vec![0xaa];
    for j in 0..70 {
        src.extend(std::iter::repeat_n(&x, 1 + j % 3).flatten());
        src.extend_from_slice(&sep);
    }
    src.push(0);
    for j in 0..6 {
        let mut tgt = vec![1u8];
        tgt.extend(std::iter::repeat_n(&x, 1 + j % 3).flatten());
        tgt.extend_from_slice(&sep);
        tgt.extend_from_slice(&x);
        tgt.extend_from_slice(&[0xff, 0]);
        let want = git_binary_block(&dir.0, &src, &tgt);
        let mut got = Vec::new();
        rgit_diff::binary::write_binary_patch(&mut got, &src, &tgt);
        assert!(
            same_block(exact, &want, &got),
            "--- git\n{}\n--- rgit\n{}",
            String::from_utf8_lossy(&want),
            String::from_utf8_lossy(&got)
        );
    }
}
