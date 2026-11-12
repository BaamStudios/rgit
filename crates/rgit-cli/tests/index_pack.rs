// SPDX-License-Identifier: MIT
//! Differential tests of `rgit index-pack` against the reference git:
//! output, exit code, the written `.idx`/`.rev`/`.keep` files (byte for
//! byte) and the objects stored by `--stdin`.

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"))
}

/// Output plus the files in the work directory (where file-mode
/// index-pack writes) and the stored objects.
const CHECKS: [Check; 5] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Worktree,
    Check::Objects,
];

/// Run every case in its own copy of the fixture state; report all
/// mismatches together.
#[track_caller]
fn all_same(fx: &mut Fixture, cases: &[(&[&str], Option<&[u8]>)]) {
    let mut failures = Vec::new();
    for (args, stdin) in cases {
        let mut c = fx.compare(*args).checks(CHECKS);
        if let Some(input) = stdin {
            c = c.stdin(input.to_vec());
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

/// A repository with a history whose pack has long delta chains.
fn history() -> Fixture {
    let mut fx = fixture()
        .git(["init", "-q"])
        .git(["config", "pack.threads", "1"]);
    for i in 1..=25u32 {
        let lines: String = (1..=i * 40).map(|n| format!("{n}\n")).collect();
        let other: String = (i..400).map(|n| format!("line {n}\n")).collect();
        fx = fx
            .write("f", lines)
            .write("sub/g", other)
            .write(format!("h{}", i % 5), format!("{i}\n"))
            .git(["add", "."])
            .git(["commit", "-q", "-m", &format!("c{i}")])
            .tick();
    }
    fx
}

fn pack_of(fx: &Fixture, revs: &str, extra: &[&str]) -> Vec<u8> {
    let script = format!(
        "printf '%s\\n' {revs} | \"$GIT\" pack-objects --stdout --revs {}",
        extra.join(" ")
    );
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(&script)
        .current_dir(fx.work_dir())
        .env("GIT", fx.reference_git())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("sh");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

#[test]
fn file_mode_matches_git() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    fx.git_stdout(["repack", "-adq", "--depth=100"]);
    let pack = pack_of(&fx, "HEAD", &[]);
    let mut fx = fx.write("t.pack", &pack).write("t.foo", &pack);
    all_same(
        &mut fx,
        &[
            (&["index-pack", "t.pack"], None),
            (&["index-pack", "-o", "x.idx", "t.pack"], None),
            (
                &["index-pack", "--index-version=1", "-o", "x.idx", "t.pack"],
                None,
            ),
            (
                &[
                    "index-pack",
                    "--index-version=2,0x400",
                    "-o",
                    "x.idx",
                    "t.pack",
                ],
                None,
            ),
            (
                &[
                    "index-pack",
                    "--index-version=1,0x400",
                    "-o",
                    "x.idx",
                    "t.pack",
                ],
                None,
            ),
            (&["index-pack", "--no-rev-index", "t.pack"], None),
            (
                &["-c", "pack.writeReverseIndex=false", "index-pack", "t.pack"],
                None,
            ),
            (&["-c", "pack.indexVersion=1", "index-pack", "t.pack"], None),
            (&["index-pack", "--threads=4", "--keep=why", "t.pack"], None),
            (&["index-pack", "--keep", "t.pack"], None),
            (&["index-pack", "--strict", "t.pack"], None),
            (&["index-pack", "--fsck-objects", "t.pack"], None),
            (&["index-pack", "-v", "t.pack"], None),
            (&["index-pack", "t.foo"], None),
            (&["index-pack", "-o", "foo.idx", "t.foo"], None),
            (&["index-pack", "missing.pack"], None),
            (&["index-pack", "--max-input-size=100", "t.pack"], None),
            (&["index-pack", "--index-version=3", "t.pack"], None),
            (&["index-pack", "--fix-thin", "t.pack"], None),
            (&["index-pack", "--object-format=nope", "t.pack"], None),
            (&["index-pack"], None),
            (&["index-pack", "-h"], None),
            (&["index-pack", "--bogus", "t.pack"], None),
            (&["index-pack", "a.pack", "b.pack"], None),
            (&["index-pack", "--verify", "--stdin"], Some(&pack)),
        ],
    );
}

#[test]
fn verify_matches_git() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let pack = pack_of(&fx, "HEAD", &[]);
    let mut fx = fx
        .write("t.pack", &pack)
        .git(["index-pack", "t.pack"])
        .write("v1.pack", &pack)
        .git(["index-pack", "--index-version=1", "v1.pack"])
        .write("big.pack", &pack)
        .git(["index-pack", "--index-version=2,0x200", "big.pack"])
        .write("noidx.pack", &pack);
    all_same(
        &mut fx,
        &[
            (&["index-pack", "--verify", "t.pack"], None),
            (&["index-pack", "--verify", "v1.pack"], None),
            (&["index-pack", "--verify", "big.pack"], None),
            (
                &[
                    "index-pack",
                    "--verify",
                    "--index-version=2,0x200",
                    "big.pack",
                ],
                None,
            ),
            (&["index-pack", "--verify", "noidx.pack"], None),
            (&["index-pack", "--verify", "-o", "t.idx", "t.pack"], None),
            (
                &["index-pack", "--verify", "-o", "other.idx", "t.pack"],
                None,
            ),
        ],
    );
    // A damaged index fails verification.
    let mut idx = std::fs::read(fx.work_dir().join("t.idx")).unwrap();
    let n = idx.len();
    idx[n - 30] ^= 0xff;
    let mut fx = fx.remove("t.idx").write("t.idx", idx);
    all_same(&mut fx, &[(&["index-pack", "--verify", "t.pack"], None)]);
}

#[test]
fn stdin_matches_git() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let pack = pack_of(&fx, "HEAD", &[]);
    let mut short = pack.clone();
    short.truncate(pack.len() / 2);
    let mut junk = pack.clone();
    junk.extend_from_slice(b"junk");
    let mut bad = pack.clone();
    *bad.last_mut().unwrap() ^= 1;
    // An empty repository to receive the pack.
    let mut fx = fx.git(["init", "-q", "empty"]);
    let hex: String = pack[pack.len() - 20..]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    for name in ["pack", "idx", "rev", "keep"] {
        // Compare the stored files byte for byte.
        let mut c = fx
            .compare(["-C", "empty", "index-pack", "--stdin", "--keep=fetch"])
            .checks([
                Check::ExitCode,
                Check::Stdout,
                Check::Stderr,
                Check::GitFiles,
            ])
            .repo("empty")
            .stdin(pack.clone());
        c = c.git_file(format!("objects/pack/pack-{hex}.{name}"));
        c.run();
    }
    all_same(
        &mut fx,
        &[
            (&["-C", "empty", "index-pack", "--stdin"], Some(&pack)),
            (
                &["-C", "empty", "index-pack", "--stdin", "--keep"],
                Some(&pack),
            ),
            (
                &[
                    "-C",
                    "empty",
                    "-c",
                    "pack.threads=1",
                    "index-pack",
                    "--stdin",
                    "-v",
                ],
                Some(&pack),
            ),
            (
                &["-C", "empty", "index-pack", "--stdin", "--strict"],
                Some(&pack),
            ),
            (
                &["-C", "empty", "index-pack", "--stdin", "out.pack"],
                Some(&pack),
            ),
            (
                &["-C", "empty", "index-pack", "--stdin", "-o", "../o.idx"],
                Some(&pack),
            ),
            (&["-C", "empty", "index-pack", "--stdin"], Some(&short)),
            (&["-C", "empty", "index-pack", "--stdin"], Some(&junk)),
            (&["-C", "empty", "index-pack", "--stdin"], Some(&bad)),
            (
                &[
                    "-C",
                    "empty",
                    "index-pack",
                    "--stdin",
                    "--object-format=sha1",
                ],
                Some(&pack),
            ),
            (&["index-pack", "--stdin"], Some(&pack)),
        ],
    );
}

#[test]
fn thin_pack_is_completed() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let thin = pack_of(&fx, "HEAD ^HEAD~10", &["--thin"]);
    let old = pack_of(&fx, "HEAD~10", &[]);
    // The client has the old history in a pack.
    let mut fx = fx
        .git(["init", "-q", "client"])
        .write("client/.git/objects/pack/pack-old.pack", &old)
        .git(["index-pack", "client/.git/objects/pack/pack-old.pack"]);
    // Without --fix-thin the deltas stay unresolved.
    fx.compare(["-C", "client", "index-pack", "--stdin"])
        .checks(Check::OUTPUT)
        .stdin(thin.clone())
        .run();
    // With it the pack is completed from local objects. The compressed
    // bytes of the appended bases may differ from git's (another zlib), so
    // the pack name is not compared; the stored objects are.
    fx.compare(["-C", "client", "index-pack", "--stdin", "--fix-thin", "-v"])
        .checks([Check::ExitCode, Check::Objects])
        .repo("client")
        .stdin(thin)
        .run();
}

#[test]
fn hand_made_packs() {
    let mut fx = fixture().git(["init", "-q"]);
    if fx.is_skipped() {
        return;
    }
    let cases = hand_made();
    let mut failures = Vec::new();
    for (name, pack) in &cases {
        for args in [
            &["index-pack", "--stdin"][..],
            &["index-pack", "--stdin", "--fix-thin"][..],
            &["index-pack", "--stdin", "--strict"][..],
        ] {
            let r = fx
                .compare(args)
                .checks([
                    Check::ExitCode,
                    Check::Stdout,
                    Check::Stderr,
                    Check::Objects,
                ])
                .stdin(pack.clone())
                .try_run();
            if let Err(m) = r {
                failures.push(format!("{name}: {m}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Small packs built by hand: deltas, cycles, duplicates, damage.
fn hand_made() -> Vec<(&'static str, Vec<u8>)> {
    /// zlib stream with one stored (uncompressed) block.
    fn zlib(d: &[u8]) -> Vec<u8> {
        let mut out = vec![0x78, 0x01, 0x01];
        let n = d.len() as u16;
        out.extend(n.to_le_bytes());
        out.extend((!n).to_le_bytes());
        out.extend_from_slice(d);
        let (mut a, mut b) = (1u32, 0u32);
        for &x in d {
            a = (a + x as u32) % 65521;
            b = (b + a) % 65521;
        }
        out.extend(((b << 16) | a).to_be_bytes());
        out
    }
    fn id(d: &[u8]) -> Vec<u8> {
        rgit_hash::hash_object(rgit_hash::HashKind::Sha1, "blob", d)
            .unwrap()
            .as_bytes()
            .to_vec()
    }
    fn blob(d: &[u8]) -> Vec<u8> {
        let mut e = vec![0x30 | d.len() as u8];
        e.extend(zlib(d));
        e
    }
    fn delta(base: &[u8], target: &[u8]) -> Vec<u8> {
        let mut d = vec![base.len() as u8, target.len() as u8, target.len() as u8];
        d.extend_from_slice(target);
        let mut e = vec![0x70 | d.len() as u8];
        e.extend(id(base));
        e.extend(zlib(&d));
        e
    }
    fn pack(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut out = b"PACK\0\0\0\x02".to_vec();
        out.extend((entries.len() as u32).to_be_bytes());
        for e in entries {
            out.extend(e);
        }
        let sum = rgit_hash::hash(rgit_hash::HashKind::Sha1, &out).unwrap();
        out.extend_from_slice(sum.as_bytes());
        out
    }
    let (a, b) = (&b"hello world"[..], &b"hello there"[..]);
    let mut trunc = pack(&[blob(a), blob(b)]);
    trunc.truncate(30);
    vec![
        ("delta", pack(&[delta(a, b), blob(a)])),
        ("missing base", pack(&[delta(a, b)])),
        ("cycle", pack(&[delta(a, b), delta(b, a)])),
        (
            "cycle with copy",
            pack(&[delta(a, b), delta(b, a), blob(a)]),
        ),
        ("duplicates", pack(&[blob(b"x"), blob(b"y"), blob(b"x")])),
        ("truncated", trunc),
        ("empty", pack(&[])),
    ]
}
