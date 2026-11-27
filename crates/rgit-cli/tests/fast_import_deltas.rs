// SPDX-License-Identifier: MIT
//! Differential tests of the packs `rgit fast-import` writes: which objects
//! become deltas against which base and at which depth (`--depth`,
//! `--big-file-threshold`, `core.bigFileThreshold`, checkpoints), and the
//! delta and branch-load counters of `--stats` (`--active-branches`).
//! Sizes and offsets are left out, they depend on the delta encoder.

use std::fmt::Write as _;

use rgit_difftest::Fixture;

/// Import `s` (the file `s` in the work directory) with `args`, then
/// list the pack entries as `oid type [depth base]` and the counters of
/// `--stats`.
fn check(config: &[(&str, &str)], args: &str, stream: &str) {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .git(["config", "fastimport.unpackLimit", "0"]);
    if fx.is_skipped() {
        return;
    }
    for (k, v) in config {
        fx = fx.git(["config", k, v]);
    }
    fx = fx.write("s", stream);
    let script = format!(
        "!git fast-import --stats {args} <s 2>.git/stats && \
         grep -E '^ +(blobs|trees|commits|tags) +:|^Total branches' .git/stats && \
         for i in .git/objects/pack/*.idx; do \
         git verify-pack -v $i | awk '/^[0-9a-f]{{40}} / {{ print $1, $2, $6, $7 }}'; done"
    );
    fx.compare(["-c", &format!("alias.t={script}"), "t"]).run();
}

fn data(s: &mut String, b: &str) {
    write!(s, "data {}\n{b}\n", b.len()).unwrap();
}

/// `n` lines of text with `tag` in each, then `extra`.
fn text(tag: &str, n: usize, extra: &str) -> String {
    let mut s = String::new();
    for i in 0..n {
        writeln!(s, "line {i} of the {tag} file with some text").unwrap();
    }
    s + extra
}

/// `blob` commands for versions of a text of `lines[i]` lines.
fn blobs(lines: &[usize]) -> String {
    let mut s = String::new();
    for (c, n) in lines.iter().enumerate() {
        s.push_str("blob\n");
        data(&mut s, &text("a", *n, &format!("change {c}\n")));
    }
    s
}

/// Letters from a seed (incompressible, no repeats).
fn noise(seed: u32, n: usize) -> String {
    let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(12345);
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (b'a' + (x % 26) as u8) as char
        })
        .collect()
}

/// Commits changing two files; every blob is based on the one before,
/// whatever its path.
#[test]
fn blobs_against_the_previous_blob() {
    let mut s = String::new();
    for c in 1..=4 {
        write!(
            s,
            "commit refs/heads/main\nmark :{c}\ncommitter A <a@b> {c} +0000\n"
        )
        .unwrap();
        data(&mut s, &format!("commit {c}\n"));
        if c > 1 {
            writeln!(s, "from :{}", c - 1).unwrap();
        }
        s.push_str("M 644 inline a.txt\n");
        data(&mut s, &text("aaa", 100, &format!("change {c}\n")));
        s.push_str("M 644 inline dir/b.txt\n");
        data(&mut s, &text("bbb", 100, &format!("change {c}\n")));
        s.push('\n');
    }
    check(&[], "", &s);
    check(&[], "--depth=2", &s);
    check(&[], "--depth=0", &s);
    // Duplicates are no base; tiny blobs are not tried.
    check(
        &[],
        "",
        &format!("{}{}", blobs(&[100, 100]), blobs(&[100, 100, 100])),
    );
    let mut s = String::new();
    for c in 1..=3 {
        s.push_str("blob\n");
        data(&mut s, &format!("tiny{c}\n"));
    }
    check(&[], "", &s);
}

/// A delta is kept when it is at most the object's size minus the length
/// of an object id.
#[test]
fn delta_size_limit() {
    let mut s = String::new();
    let base = noise(1, 100);
    for k in [60usize, 70, 79, 85] {
        s.push_str("blob\n");
        data(&mut s, &base);
        let t = format!("{}{}", &base[..100 - k], noise(k as u32 + 7, k));
        s.push_str("blob\n");
        data(&mut s, &t);
    }
    check(&[], "", &s);
}

/// Trees are based on the previous version of the same directory of the
/// same branch; `--active-branches` changes the loads only.
#[test]
fn trees_against_their_previous_version() {
    let mut s = String::new();
    for c in 1..=3 {
        for br in ["x", "y"] {
            write!(s, "commit refs/heads/{br}\ncommitter A <a@b> {c} +0000\n").unwrap();
            data(&mut s, "m\n");
            for d in ["d1", "d2"] {
                if c == 1 {
                    for i in 0..20 {
                        writeln!(s, "M 644 inline {d}/f{i:02}").unwrap();
                        data(&mut s, &format!("{br}{d}{i}"));
                    }
                } else {
                    writeln!(s, "M 644 inline {d}/f05").unwrap();
                    data(&mut s, &format!("{br}{c}"));
                }
            }
            s.push('\n');
        }
    }
    check(&[], "", &s);
    check(&[], "--active-branches=1", &s);
    // A branch made by `reset` is not loaded until a commit goes to it;
    // after `deleteall` the root has no previous version.
    let s = format!(
        "{s}reset refs/heads/r\nfrom refs/heads/x\n\n\
         commit refs/heads/r\ncommitter A <a@b> 9 +0000\ndata 0\ndeleteall\n\
         M 644 inline d1/f00\ndata 1\nz\n\n"
    );
    check(&[], "--active-branches=2", &s);
}

/// Blobs above the threshold are streamed whole and are no base. The
/// command-line threshold only governs until the first pack is started,
/// then `core.bigFileThreshold` does.
#[test]
fn big_file_threshold() {
    let b = blobs(&[100, 100, 100, 100]);
    check(&[], "--big-file-threshold=1k", &b);
    check(&[], "--big-file-threshold=3296", &b);
    check(&[], "--big-file-threshold=1k", &blobs(&[100, 20, 100, 100]));
    check(&[], "--big-file-threshold=1k", &blobs(&[20, 100, 100]));
    check(
        &[("core.bigFileThreshold", "1k")],
        "",
        &blobs(&[100, 100, 20, 100]),
    );
    check(
        &[("core.bigFileThreshold", "100k")],
        "--big-file-threshold=1k",
        &b,
    );
    check(
        &[],
        "--big-file-threshold=1k",
        &format!("commit refs/heads/main\ncommitter A <a@b> 1 +0000\ndata 0\n\n{b}"),
    );
    check(&[], "", &format!("option git big-file-threshold=1k\n{b}"));
    // Inline data in a commit, and a streamed duplicate.
    let mut s = String::new();
    for c in [1, 2, 1, 3] {
        write!(s, "commit refs/heads/main\ncommitter A <a@b> {c} +0000\n").unwrap();
        data(&mut s, "m\n");
        s.push_str("M 644 inline f\n");
        data(&mut s, &text("f", 100, &format!("change {c}\n")));
        s.push('\n');
    }
    check(&[("core.bigFileThreshold", "1k")], "", &s);
}

/// A base in a pack closed by `checkpoint` is not used.
#[test]
fn checkpoint_starts_without_base() {
    let mut s = String::new();
    for c in 1..=3 {
        s.push_str("blob\n");
        data(&mut s, &text("ck", 80, &format!("{c}")));
        if c == 2 {
            s.push_str("checkpoint\n");
        }
    }
    check(&[], "", &s);
}

/// The objects of a pack with deltas read back right (`cat-blob`, `ls`
/// and loose objects below `fastimport.unpackLimit`).
#[test]
fn deltas_read_back() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"]);
    if fx.is_skipped() {
        return;
    }
    let mut s = String::new();
    for c in 1..=5 {
        write!(s, "blob\nmark :10{c}\n").unwrap();
        data(&mut s, &text("x", 60, &format!("change {c}\n")));
        write!(
            s,
            "commit refs/heads/main\nmark :{c}\ncommitter A <a@b> {c} +0000\n"
        )
        .unwrap();
        data(&mut s, "m\n");
        for d in ["a", "b/c"] {
            writeln!(s, "M 644 inline {d}/f").unwrap();
            data(&mut s, &text(d, 50, &format!("change {c}\n")));
            writeln!(s, "M 644 :10{c} {d}/g").unwrap();
        }
        writeln!(s, "ls \"a/f\"\n").unwrap();
        writeln!(s, "ls :{c} b/c").unwrap();
        writeln!(s, "cat-blob :10{c}").unwrap();
    }
    fx.compare(["fast-import", "--quiet"])
        .stdin(s.as_bytes().to_vec())
        .run();
}
