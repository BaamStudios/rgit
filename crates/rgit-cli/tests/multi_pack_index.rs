// SPDX-License-Identifier: MIT
//! `multi-pack-index` against the reference git: the written file must be
//! byte-identical (compared as a git file), `verify` must accept git's
//! files and report the same problems, and the commands that read
//! objects must behave the same with a MIDX present.

use rgit_difftest::{Check, Fixture};

const MIDX: &str = "objects/pack/multi-pack-index";

fn fixture() -> Fixture {
    Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"))
}

/// A repository with `n` packs of one commit each (no MIDX).
fn packs(n: usize) -> Fixture {
    let mut fx = fixture()
        .git(["init", "-q"])
        .git(["config", "maintenance.auto", "false"]);
    for c in 0..n {
        fx = fx
            .write(format!("f{c}.txt"), format!("file {c}\n"))
            .git(["add", "-A"])
            .git(["commit", "-q", "-m", &format!("c{c}")])
            .git(["repack", "-dq"])
            .tick();
    }
    fx
}

fn compare_midx(fx: &mut Fixture, args: &[&str]) {
    fx.compare(args)
        .checks([
            Check::ExitCode,
            Check::Stdout,
            Check::Stderr,
            Check::GitFiles,
        ])
        .git_file(MIDX)
        .run();
}

#[test]
fn write_is_byte_identical() {
    let mut fx = packs(3);
    compare_midx(&mut fx, &["multi-pack-index", "write"]);
    compare_midx(
        &mut fx,
        &["-c", "midx.version=2", "multi-pack-index", "write"],
    );
    compare_midx(&mut fx, &["multi-pack-index", "write", "--no-progress"]);
}

#[test]
fn write_with_preferred_pack_and_duplicates() {
    // Two packs holding the same blob: the preferred one wins.
    let fx = packs(1)
        .write("dup.txt", "duplicate\n")
        .git(["add", "dup.txt"])
        .git(["commit", "-q", "-m", "dup"])
        .git(["repack", "-adq"]);
    let blob = String::from_utf8(fx.git_stdout(["rev-parse", "HEAD:dup.txt"])).unwrap();
    let mut fx = fx.write("in", blob);
    let pack = {
        let o = std::process::Command::new(fx.reference_git())
            .current_dir(fx.work_dir())
            .args(["pack-objects", ".git/objects/pack/test"])
            .stdin(std::fs::File::open(fx.work_dir().join("in")).unwrap())
            .env("HOME", fx.home_dir())
            .env("XDG_CONFIG_HOME", fx.home_dir())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        String::from_utf8(o.stdout).unwrap().trim().to_owned()
    };
    if pack.is_empty() {
        return;
    }
    let preferred = format!("--preferred-pack=test-{pack}.idx");
    compare_midx(&mut fx, &["multi-pack-index", "write", &preferred]);
    let preferred = format!("--preferred-pack=test-{pack}.pack");
    compare_midx(&mut fx, &["multi-pack-index", "write", &preferred]);
    compare_midx(
        &mut fx,
        &["multi-pack-index", "write", "--preferred-pack=nothere.idx"],
    );
}

#[test]
fn write_incremental_layers() {
    let mut fx = packs(2).git(["multi-pack-index", "write"]);
    fx = fx
        .write("g.txt", "g\n")
        .git(["add", "g.txt"])
        .git(["commit", "-q", "-m", "g"])
        .git(["repack", "-dq"]);
    fx.compare(["multi-pack-index", "write", "--incremental"])
        .checks([
            Check::ExitCode,
            Check::Stdout,
            Check::Stderr,
            Check::GitFiles,
        ])
        .git_file("objects/pack/multi-pack-index.d/multi-pack-index-chain")
        .git_file(MIDX)
        .run();
}

#[test]
fn verify_and_errors() {
    let mut fx = packs(3).git(["multi-pack-index", "write"]);
    fx.compare(["multi-pack-index", "verify"])
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["multi-pack-index", "verify", "--progress"])
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["multi-pack-index"]).checks(Check::OUTPUT).run();
    fx.compare(["multi-pack-index", "write", "-h"])
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["multi-pack-index", "write", "--no-write-chain-file"])
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["multi-pack-index", "--object-dir=nowhere", "verify"])
        .checks([Check::ExitCode])
        .run();
}

#[test]
fn corrupt_midx_is_reported_like_git() {
    let mut fx = packs(3).git(["multi-pack-index", "write"]);
    let path = fx.work_dir().join(".git").join(MIDX);
    let good = std::fs::read(&path).unwrap();
    // (offset, byte): signature, version, hash version, chunk count (too
    // small and too large), first chunk id.
    for (at, byte) in [(0usize, 0u8), (4, 0), (5, 3), (6, 1), (6, 7), (12, 1)] {
        let mut bad = good.clone();
        bad[at] = byte;
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, &bad).unwrap();
        fx.compare(["multi-pack-index", "verify"])
            .checks(Check::OUTPUT)
            .run();
        fx.compare(["log", "--oneline"]).checks(Check::OUTPUT).run();
    }
}

#[test]
fn expire_and_repack() {
    let mut fx = packs(4).git(["multi-pack-index", "write"]);
    fx.compare(["multi-pack-index", "expire"])
        .checks([
            Check::ExitCode,
            Check::Stdout,
            Check::Stderr,
            Check::Objects,
        ])
        .run();
    fx.compare(["multi-pack-index", "repack", "--batch-size=0"])
        .checks([
            Check::ExitCode,
            Check::Stdout,
            Check::Stderr,
            Check::Objects,
        ])
        .run();
}
