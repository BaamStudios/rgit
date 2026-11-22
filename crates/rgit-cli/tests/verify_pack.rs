// SPDX-License-Identifier: MIT
//! Differential tests of `rgit verify-pack`, `rgit show-index` and
//! `rgit count-objects` against the reference git.

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"))
}

const OUTPUT: [Check; 3] = [Check::ExitCode, Check::Stdout, Check::Stderr];

/// A repository with one pack that has delta chains, plus a few loose
/// objects.
fn repo() -> Fixture {
    let mut fx = fixture()
        .git(["init", "-q"])
        .git(["config", "pack.threads", "1"]);
    for i in 1..=8u32 {
        let lines: String = (1..=i * 60).map(|n| format!("{n}\n")).collect();
        fx = fx
            .write("f", lines)
            .write(format!("g{}", i % 3), format!("{i}\n"))
            .git(["add", "."])
            .git(["commit", "-q", "-m", &format!("c{i}")])
            .tick();
    }
    fx.git(["repack", "-adq"])
        .git([
            "pack-objects",
            "--index-version=1",
            "-q",
            "v1",
            "--all",
            "--revs",
        ])
        .write("loose", "loose\n")
        .git(["add", "loose"])
}

fn pack_base(fx: &Fixture) -> String {
    let dir = fx.work_dir().join(".git/objects/pack");
    let idx = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.ends_with(".idx"))
        .unwrap();
    format!(".git/objects/pack/{}", idx.strip_suffix(".idx").unwrap())
}

fn v1_index(fx: &Fixture) -> String {
    std::fs::read_dir(fx.work_dir())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.starts_with("v1-") && n.ends_with(".idx"))
        .unwrap()
}

#[track_caller]
fn all_same(fx: &mut Fixture, cases: &[(Vec<String>, Option<Vec<u8>>)]) {
    let mut failures = Vec::new();
    for (args, stdin) in cases {
        let mut c = fx.compare(args).checks(OUTPUT);
        if let Some(input) = stdin {
            c = c.stdin(input.clone());
        }
        if let Err(m) = c.try_run() {
            failures.push(m.to_string());
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

#[test]
fn verify_pack() {
    let mut fx = repo();
    if fx.is_skipped() {
        return;
    }
    let base = pack_base(&fx);
    let v1 = v1_index(&fx);
    let idx = format!("{base}.idx");
    let pack = format!("{base}.pack");
    let cases: Vec<(Vec<String>, Option<Vec<u8>>)> = vec![
        (args(&["verify-pack", &idx]), None),
        (args(&["verify-pack", "-v", &idx]), None),
        (args(&["verify-pack", "--verbose", &pack, &v1]), None),
        (args(&["verify-pack", "-s", &idx]), None),
        (args(&["verify-pack", "-s", "-v", &base]), None),
        (
            args(&["verify-pack", "--object-format=sha1", "-v", &idx]),
            None,
        ),
        (args(&["verify-pack", "-v", "nonexist.idx", &idx]), None),
        (args(&["verify-pack", "nonexist.pack"]), None),
        (args(&["verify-pack"]), None),
        (args(&["verify-pack", "-h"]), None),
    ];
    all_same(&mut fx, &cases);
}

#[test]
fn verify_pack_corrupt() {
    let fx = repo();
    if fx.is_skipped() {
        return;
    }
    let base = pack_base(&fx);
    let pack = std::fs::read(fx.work_dir().join(format!("{base}.pack"))).unwrap();
    let idx = std::fs::read(fx.work_dir().join(format!("{base}.idx"))).unwrap();
    let mut bad_sig = pack.clone();
    bad_sig[2] = b'X';
    let mut bad_data = pack.clone();
    bad_data[300] ^= 0xff;
    let mut bad_idx = idx.clone();
    let n = bad_idx.len();
    bad_idx[n - 3] ^= 0xff;
    let mut fx = fx
        .write("sig.pack", &bad_sig)
        .write("sig.idx", &idx)
        .write("data.pack", &bad_data)
        .write("data.idx", &idx)
        .write("idx.pack", &pack)
        .write("idx.idx", &bad_idx);
    let mut cases = Vec::new();
    for name in ["sig.idx", "data.idx", "idx.idx"] {
        cases.push((args(&["verify-pack", name]), None));
        cases.push((args(&["verify-pack", "-v", name]), None));
        cases.push((args(&["verify-pack", "-s", name]), None));
    }
    all_same(&mut fx, &cases);
}

#[test]
fn show_index() {
    let mut fx = repo();
    if fx.is_skipped() {
        return;
    }
    let base = pack_base(&fx);
    let v2 = std::fs::read(fx.work_dir().join(format!("{base}.idx"))).unwrap();
    let v1 = std::fs::read(fx.work_dir().join(v1_index(&fx))).unwrap();
    let mut cases: Vec<(Vec<String>, Option<Vec<u8>>)> = vec![
        (args(&["show-index"]), Some(v2.clone())),
        (args(&["show-index"]), Some(v1.clone())),
        (
            args(&["show-index", "--object-format=sha1"]),
            Some(v2.clone()),
        ),
        (
            args(&["show-index", "--object-format=md5"]),
            Some(v2.clone()),
        ),
        (args(&["show-index", "-h"]), None),
        (args(&["show-index"]), Some(b"\xfftOc\0\0\0\x03".to_vec())),
    ];
    for cut in [0, 3, 7, 8, 100, 1032, 1040, 1100, 1300, 1500, v2.len() - 41] {
        cases.push((
            args(&["show-index"]),
            Some(v2[..cut.min(v2.len())].to_vec()),
        ));
    }
    for cut in [0, 5, 1024, 1030, 1050] {
        cases.push((
            args(&["show-index"]),
            Some(v1[..cut.min(v1.len())].to_vec()),
        ));
    }
    all_same(&mut fx, &cases);
}

#[test]
fn show_index_large_offsets() {
    let mut fx = repo().git([
        "pack-objects",
        "--index-version=2,0x100",
        "-q",
        "big",
        "--all",
        "--revs",
    ]);
    if fx.is_skipped() {
        return;
    }
    let name = std::fs::read_dir(fx.work_dir())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.starts_with("big-") && n.ends_with(".idx"))
        .unwrap();
    let idx = std::fs::read(fx.work_dir().join(name)).unwrap();
    let mut cases: Vec<(Vec<String>, Option<Vec<u8>>)> =
        vec![(args(&["show-index"]), Some(idx.clone()))];
    for cut in [idx.len() - 41, idx.len() - 60, idx.len() - 100] {
        cases.push((args(&["show-index"]), Some(idx[..cut].to_vec())));
    }
    all_same(&mut fx, &cases);
}

#[test]
fn count_objects() {
    let fx = repo();
    if fx.is_skipped() {
        return;
    }
    let mut cases: Vec<(Vec<String>, Option<Vec<u8>>)> = vec![
        (args(&["count-objects"]), None),
        (args(&["count-objects", "-v"]), None),
        (args(&["count-objects", "-H"]), None),
        (args(&["count-objects", "-vH"]), None),
        (args(&["count-objects", "x"]), None),
        (args(&["count-objects", "-h"]), None),
    ];
    let mut fx2 = fx;
    all_same(&mut fx2, &cases);

    // Garbage in the pack directory and in loose object directories (one
    // of each kind, so the report order does not depend on readdir).
    let mut fx = fx2
        .write(".git/objects/pack/foo.bar", "x")
        .write(".git/objects/pack/fake2.keep", "")
        .write(".git/objects/ab/zz", "abc");
    cases.truncate(2);
    all_same(&mut fx, &cases);
    let mut fx = fx
        .remove(".git/objects/pack/foo.bar")
        .remove(".git/objects/pack/fake2.keep")
        .remove(".git/objects/ab/zz")
        .write(".git/objects/pack/k.pack", "");
    all_same(&mut fx, &cases);
}
