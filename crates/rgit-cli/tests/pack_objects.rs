// SPDX-License-Identifier: MIT
//! `rgit pack-objects` against the reference git: error output must match
//! byte for byte; packs need not be byte-identical, but must be accepted
//! by `git index-pack --strict` and `git verify-pack`, and give the same
//! objects (same ids and the same `cat-file` output) as git's pack.

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"))
}

/// A repository with packed and loose objects, branches, a merge, tags
/// and subdirectories.
fn repo() -> Fixture {
    let mut fx = fixture().git(["init", "-q"]);
    let mut text = String::new();
    for i in 0..80 {
        text.push_str(&format!("line {i} of some text\n"));
    }
    for c in 0..6 {
        text.insert_str((c * 53) % text.len(), &format!("edit {c}\n"));
        fx = fx
            .write("a.txt", &text)
            .write(format!("d/e{}/f.txt", c % 2), &text[c..])
            .git(["add", "-A"])
            .git(["commit", "-q", "-m", &format!("c{c}")])
            .tick();
    }
    fx = fx
        .git(["tag", "-a", "-m", "tag", "v1"])
        .git(["repack", "-adq"])
        .git(["checkout", "-q", "-b", "side", "HEAD~2"])
        .write("side.txt", "side\n")
        .git(["add", "side.txt"])
        .git(["commit", "-q", "-m", "side"])
        .tick()
        .git(["checkout", "-q", "main"])
        .git(["merge", "-q", "--no-edit", "side"])
        .tick()
        .write_executable("run.sh", "#!/bin/sh\n")
        .git(["add", "run.sh"])
        .git(["commit", "-q", "-m", "loose"])
        .git(["tag", "-a", "-m", "tree tag", "t-tree", "HEAD^{tree}"]);
    fx
}

/// The objects in `pack` (written to `name` in the work tree), after
/// checking it with `index-pack --strict` and `verify-pack`.
fn check_pack(fx: Fixture, pack: &[u8], name: &str) -> (Fixture, Vec<String>) {
    let file = format!("{name}.pack");
    let fx = fx.write(&file, pack);
    fx.git_stdout(["index-pack", "--strict", &file]);
    let listing = fx.git_stdout(["verify-pack", "-v", &format!("{name}.idx")]);
    let mut ids: Vec<String> = String::from_utf8(listing)
        .unwrap()
        .lines()
        .filter_map(|l| {
            let mut parts = l.split(' ');
            let id = parts.next()?;
            let kind = parts.find(|p| !p.is_empty())?;
            matches!(kind, "commit" | "tree" | "blob" | "tag").then(|| id.to_string())
        })
        .collect();
    ids.sort();
    (fx, ids)
}

/// Run `pack-objects` with `args` and `input` on both sides; check that
/// rgit's pack is valid and holds the same objects as git's.
fn same_objects(fx: Fixture, args: &[&str], input: &str) -> Fixture {
    let mut fx = fx;
    let mut argv = vec!["pack-objects"];
    argv.extend_from_slice(args);
    let Some(outcome) = fx
        .compare(&argv)
        .stdin(input)
        .checks([Check::ExitCode, Check::Stderr])
        .run()
    else {
        return fx;
    };
    assert_eq!(
        outcome.git.code,
        Some(0),
        "git pack-objects {args:?} <<< {input:?}"
    );
    let (fx, ours) = check_pack(fx, &outcome.rgit.stdout, "ours");
    let (fx, theirs) = check_pack(fx, &outcome.git.stdout, "theirs");
    assert_eq!(ours, theirs, "pack-objects {args:?} <<< {input:?}");
    assert!(!ours.is_empty() || input.is_empty(), "empty pack: {args:?}");
    // Same content through cat-file: one bare repository per pack.
    let mut fx = fx;
    let mut dumps = Vec::new();
    for name in ["ours", "theirs"] {
        let dir = format!("{name}.git");
        fx = fx.git(["init", "-q", "--bare", &dir]);
        for ext in ["pack", "idx"] {
            let from = fx.work_dir().join(format!("{name}.{ext}"));
            let to = fx
                .work_dir()
                .join(format!("{dir}/objects/pack/pack-x.{ext}"));
            std::fs::rename(from, to).unwrap();
        }
        dumps.push(fx.git_stdout([
            "--git-dir",
            &dir,
            "cat-file",
            "--batch-all-objects",
            "--batch",
        ]));
        std::fs::remove_dir_all(fx.work_dir().join(&dir)).unwrap();
    }
    assert!(dumps[0] == dumps[1], "cat-file output differs");
    fx
}

#[test]
fn revs_all() {
    let fx = repo();
    let fx = same_objects(fx, &["--revs", "--stdout", "--all"], "");
    let fx = same_objects(
        fx,
        &["--revs", "--stdout", "--all", "--delta-base-offset"],
        "",
    );
    let fx = same_objects(fx, &["--all", "--stdout", "--no-reuse-delta", "-q"], "");
    same_objects(
        fx,
        &["--all", "--stdout", "--no-reuse-object", "--compression=1"],
        "",
    );
}

#[test]
fn revs_ranges() {
    let fx = repo();
    let fx = same_objects(fx, &["--revs", "--stdout"], "main\n^v1\n");
    let fx = same_objects(fx, &["--revs", "--stdout"], "HEAD\n--not\nside\n");
    let fx = same_objects(fx, &["--revs", "--stdout"], "HEAD~1\n^HEAD~3\n");
    let fx = same_objects(fx, &["--revs", "--stdout"], "v1\nt-tree\n");
    let fx = same_objects(fx, &["--revs", "--stdout"], "HEAD~1^2\n^HEAD~1^1\n");
    same_objects(fx, &["--revs", "--stdout"], "side~1^{tree}\n");
}

#[test]
fn object_list() {
    let fx = repo();
    let list = String::from_utf8(fx.git_stdout(["rev-list", "--objects", "HEAD~2"])).unwrap();
    let fx = same_objects(fx, &["--stdout"], &list);
    let head = String::from_utf8(fx.git_stdout(["rev-parse", "HEAD", "v1"])).unwrap();
    same_objects(fx, &["--stdout", "--delta-base-offset"], &head);
}

#[test]
fn empty_input() {
    let mut fx = repo();
    fx.compare(["pack-objects", "--stdout"])
        .stdin("")
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["pack-objects", "--stdout", "--non-empty"])
        .stdin("")
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn errors() {
    let mut fx = repo();
    for (args, input) in [
        (&["--revs", "--stdout"][..], "nosuch\n"),
        (&["--revs", "--stdout"][..], "--bogus\n"),
        (
            &["--revs", "--stdout"][..],
            "1234567890123456789012345678901234567890\n",
        ),
        (&["--stdout"][..], "HEAD\n"),
        (
            &["--stdout"][..],
            "1234567890123456789012345678901234567890\n",
        ),
        (&["--stdout", "--compression=12"][..], ""),
        (&["--stdout", "base"][..], ""),
        (
            &["--stdout"][..],
            "1234567890123456789012345678901234567890\n-xyz\n",
        ),
        (&["--thin", "base"][..], ""),
        (&["--index-version=3", "base"][..], ""),
        (&["--index-version=x", "base"][..], ""),
        (&[][..], ""),
        (&["--bogus"][..], ""),
    ] {
        let mut argv = vec!["pack-objects"];
        argv.extend_from_slice(args);
        fx.compare(&argv)
            .stdin(input)
            .checks([Check::ExitCode, Check::Stderr])
            .run();
    }
}

#[test]
fn depth_warning() {
    let mut fx = repo();
    fx.compare(["pack-objects", "--stdout", "--depth=5000", "-q"])
        .stdin("")
        .checks(Check::OUTPUT)
        .run();
}

/// Run the reference git with the fixture's isolated environment, feeding
/// `input`.
fn git_with_input(fx: &Fixture, dir: &str, args: &[&str], input: &[u8]) -> Vec<u8> {
    run_with_input(fx, fx.reference_git(), dir, args, input)
}

/// Run `program` (git or rgit) in `dir` of the work tree with the
/// fixture's isolated environment, feeding `input`; it must succeed.
fn run_with_input(
    fx: &Fixture,
    program: &std::path::Path,
    dir: &str,
    args: &[&str],
    input: &[u8],
) -> Vec<u8> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let home = fx.home_dir();
    let mut child = Command::new(program)
        .args(args)
        .current_dir(fx.work_dir().join(dir))
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// Objects per delta chain length in a pack's `verify-pack -v` listing.
fn chain_lengths(fx: &Fixture, idx: &str) -> Vec<(u32, u32)> {
    let listing = String::from_utf8(fx.git_stdout(["verify-pack", "-v", idx])).unwrap();
    listing
        .lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("chain length = ")?;
            let (depth, count) = rest.split_once(": ")?;
            let count = count.split(' ').next()?;
            Some((depth.parse().ok()?, count.parse().ok()?))
        })
        .collect()
}

/// A repository with many versions of a few files, so that the delta
/// search has work to do.
fn history() -> Fixture {
    let mut fx = fixture().git(["init", "-q"]);
    let mut text = String::new();
    for i in 0..200 {
        text.push_str(&format!("line {i}: the quick brown fox jumps\n"));
    }
    for c in 0..12 {
        text.insert_str((c * 997) % text.len(), &format!("edit {c}\n"));
        fx = fx
            .write("big.txt", &text)
            .write("sub/copy.txt", &text[c * 3..])
            .git(["add", "-A"])
            .git(["commit", "-q", "-m", &format!("c{c}")])
            .tick();
    }
    fx
}

/// New deltas are found, `--depth` and `pack.depth` are respected, and
/// `--window=0` turns the search off.
#[test]
fn delta_search() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let fx = same_objects(fx, &["--all", "--stdout", "--no-reuse-delta"], "");
    let mut fx = fx;
    for (args, max_depth) in [
        (&["--depth=2"][..], 2),
        (&["--depth=1", "--window=3"][..], 1),
        (&["--window=0"][..], 0),
        (&["--threads=1", "--window-memory=1k"][..], 50),
    ] {
        let mut argv = vec![
            "pack-objects",
            "--all",
            "--revs",
            "--stdout",
            "--no-reuse-delta",
            "--delta-base-offset",
        ];
        argv.extend_from_slice(args);
        let Some(outcome) = fx
            .compare(&argv)
            .stdin("")
            .checks([Check::ExitCode, Check::Stderr])
            .run()
        else {
            return;
        };
        let (next, _) = check_pack(fx, &outcome.rgit.stdout, "ours");
        fx = next;
        let chains = chain_lengths(&fx, "ours.idx");
        let deepest = chains.iter().map(|c| c.0).max().unwrap_or(0);
        assert!(deepest <= max_depth, "{args:?}: {chains:?}");
        if max_depth > 0 {
            assert!(!chains.is_empty(), "{args:?}: no deltas");
        }
        // Not much larger than git's pack.
        let ours = outcome.rgit.stdout.len();
        let theirs = outcome.git.stdout.len();
        assert!(ours * 10 <= theirs * 13, "{args:?}: {ours} vs {theirs}");
    }
    let mut fx = fx.git(["config", "pack.depth", "1"]);
    let Some(outcome) = fx
        .compare(["pack-objects", "--all", "--revs", "--stdout", "-q"])
        .stdin("")
        .checks([Check::ExitCode, Check::Stderr])
        .run()
    else {
        return;
    };
    let (fx, _) = check_pack(fx, &outcome.rgit.stdout, "cfg");
    let chains = chain_lengths(&fx, "cfg.idx");
    assert!(chains.iter().all(|c| c.0 <= 1), "{chains:?}");
}

/// `pack-objects <base-name>` writes `.pack`, `.idx` and `.rev` named
/// after the checksum, and prints it; the index is the one git builds.
#[test]
fn base_name_files() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let list = String::from_utf8(fx.git_stdout(["rev-list", "--objects", "--all"])).unwrap();
    let fx = fx.git(["init", "-q", "--bare", "out.git"]);
    let rgit = std::path::Path::new(env!("CARGO_BIN_EXE_rgit"));
    let out = run_with_input(
        &fx,
        rgit,
        "",
        &["pack-objects", "out.git/objects/pack/pack"],
        list.as_bytes(),
    );
    let hash = String::from_utf8(out).unwrap();
    let hash = hash.trim();
    assert_eq!(hash.len(), 40);
    let base = format!("out.git/objects/pack/pack-{hash}");
    for ext in ["pack", "idx", "rev"] {
        let p = fx.work_dir().join(format!("{base}.{ext}"));
        assert!(p.exists(), "{p:?}");
    }
    fx.git_stdout(["verify-pack", &format!("{base}.idx")]);
    // git's own index of the same pack.
    fx.git_stdout([
        "index-pack",
        "--rev-index",
        "-o",
        "check.idx",
        &format!("{base}.pack"),
    ]);
    let read = |p: String| std::fs::read(fx.work_dir().join(p)).unwrap();
    assert!(read(format!("{base}.idx")) == read("check.idx".into()));
    assert!(read(format!("{base}.rev")) == read("check.rev".into()));
    // The repository holding only that pack has every object.
    let dump = fx.git_stdout([
        "--git-dir",
        "out.git",
        "cat-file",
        "--batch-all-objects",
        "--batch-check",
    ]);
    assert_eq!(
        String::from_utf8(dump).unwrap().lines().count(),
        list.lines().count()
    );
    // Version 1 index.
    let out = run_with_input(
        &fx,
        rgit,
        "",
        &["pack-objects", "--index-version=1", "v1"],
        list.as_bytes(),
    );
    let hash = String::from_utf8(out).unwrap();
    let idx = std::fs::read(fx.work_dir().join(format!("v1-{}.idx", hash.trim()))).unwrap();
    assert_ne!(&idx[..4], b"\xfftOc");
    fx.git_stdout(["verify-pack", &format!("v1-{}.idx", hash.trim())]);
}

/// `--thin` packs use objects the receiver has as delta bases; they are
/// completed with `index-pack --fix-thin`.
#[test]
fn thin_packs() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let old = String::from_utf8(fx.git_stdout(["rev-parse", "HEAD~3"])).unwrap();
    let fx = fx.git(["init", "-q", "--bare", "t.git"]).git([
        "-C",
        "t.git",
        "fetch",
        "-q",
        "..",
        &format!("{}:refs/heads/old", old.trim()),
    ]);
    let expect: usize =
        String::from_utf8(fx.git_stdout(["rev-list", "--objects", "HEAD", "^HEAD~3"]))
            .unwrap()
            .lines()
            .count();
    let mut fx = fx;
    for (args, input) in [
        (
            &["--revs", "--thin", "--stdout"][..],
            "HEAD\n^HEAD~3\n".to_string(),
        ),
        (
            &["--thin", "--stdout", "--delta-base-offset"][..],
            "HEAD\n--not\nHEAD~3\n".to_string(),
        ),
    ] {
        let mut argv = vec!["pack-objects"];
        argv.extend_from_slice(args);
        let Some(outcome) = fx
            .compare(&argv)
            .stdin(input)
            .checks([Check::ExitCode, Check::Stderr])
            .run()
        else {
            return;
        };
        let pack = outcome.rgit.stdout;
        let count = u32::from_be_bytes(pack[8..12].try_into().unwrap()) as usize;
        assert_eq!(count, expect);
        let out = git_with_input(
            &fx,
            "t.git",
            &["index-pack", "--strict", "--fix-thin", "--stdin"],
            &pack,
        );
        let name = String::from_utf8(out).unwrap();
        let name = name.trim().strip_prefix("pack\t").unwrap().to_string();
        let idx = format!("t.git/objects/pack/pack-{name}.idx");
        let listing = String::from_utf8(fx.git_stdout(["verify-pack", "-v", &idx])).unwrap();
        let objects = listing
            .lines()
            .filter(|l| {
                l.split(' ')
                    .any(|w| matches!(w, "commit" | "tree" | "blob"))
            })
            .count();
        // Bases were appended: the pack was thin.
        assert!(objects > expect, "{objects} objects, {expect} sent");
        fx.git_stdout(["-C", "t.git", "fsck", "--no-progress"]);
    }
}

/// `--max-pack-size` and `pack.packSizeLimit` split the output into
/// self-contained packs; an object larger than the limit gets its own.
#[test]
fn split_packs() {
    let mut fx = fixture().git(["init", "-q"]);
    if fx.is_skipped() {
        return;
    }
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut noise = |n: usize| -> Vec<u8> {
        (0..n)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect()
    };
    for name in ["a_big", "b_big"] {
        fx = fx.write(name, noise(1_500_000));
    }
    fx = fx
        .write("small", "small\n")
        .git(["add", "-A"])
        .git(["commit", "-q", "-m", "c"]);
    let list = String::from_utf8(fx.git_stdout(["rev-list", "--objects", "--all"])).unwrap();
    let rgit = std::path::Path::new(env!("CARGO_BIN_EXE_rgit"));
    for (args, packs) in [
        (&["pack-objects", "--max-pack-size=1m", "p"][..], 4),
        (&["-c", "pack.packSizeLimit=2m", "pack-objects", "q"][..], 2),
    ] {
        let out = run_with_input(&fx, rgit, "", args, list.as_bytes());
        let names: Vec<String> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(names.len(), packs, "{args:?}");
        let prefix = args.last().unwrap();
        let mut total = 0;
        for n in &names {
            let idx = format!("{prefix}-{n}.idx");
            let listing = String::from_utf8(fx.git_stdout(["verify-pack", "-v", &idx])).unwrap();
            total += listing
                .lines()
                .filter(|l| {
                    l.split(' ')
                        .any(|w| matches!(w, "commit" | "tree" | "blob"))
                })
                .count();
        }
        assert_eq!(total, list.lines().count(), "{args:?}");
    }
}

/// Stderr normalisers for the progress tests: the pack size and rate in
/// `Writing objects`, and the delta counts of the `Total` line (the delta
/// search need not find the same deltas as git, so delta bases may be
/// written ahead at other places, and `Writing objects` may skip other
/// counts).
fn progress_normalizers() -> [rgit_difftest::Normalizer; 3] {
    use rgit_difftest::Normalizer;
    [
        Normalizer::regex(r"Writing objects: +\d+% \(\d+/\d+\)\r", ""),
        Normalizer::regex(
            r", [0-9.]+ (bytes|KiB|MiB) \| [0-9.]+ (bytes|KiB|MiB)/s",
            ", <SIZE> | <RATE>",
        ),
        Normalizer::regex(
            r"Total (\d+) \(delta \d+\), reused (\d+) \(delta \d+\)",
            "Total $1 (delta <D>), reused $2 (delta <D>)",
        ),
    ]
}

/// The progress meters: `Enumerating objects`, `Counting objects`,
/// `Delta compression using up to N threads`, `Compressing objects`,
/// `Writing objects` (with `--all-progress`, or for `<base-name>`
/// output), and which of `-q`, `--progress`, `--all-progress` wins.
#[test]
fn progress_meters() {
    let mut fx = history();
    if fx.is_skipped() {
        return;
    }
    let all = ["--all", "--stdout"];
    let cases: Vec<Vec<&str>> = vec![
        vec!["-c", "pack.threads=2", "pack-objects", "--all-progress"],
        vec!["-c", "pack.threads=1", "pack-objects", "--all-progress"],
        vec!["-c", "pack.threads=2", "pack-objects", "--progress"],
        vec![
            "-c",
            "pack.threads=2",
            "pack-objects",
            "--progress",
            "--all-progress-implied",
        ],
        vec![
            "-c",
            "pack.threads=2",
            "pack-objects",
            "--all-progress",
            "-q",
        ],
        vec!["-c", "pack.threads=2", "pack-objects", "-q", "--progress"],
        vec![
            "-c",
            "pack.threads=2",
            "pack-objects",
            "--progress",
            "--window=0",
        ],
    ];
    for mut args in cases {
        args.extend(all);
        progress_case(&mut fx, &args);
    }
    progress_case(
        &mut fx,
        &[
            "-c",
            "pack.threads=3",
            "pack-objects",
            "--progress",
            "--all",
            "p",
        ],
    );
    // Reused deltas are not searched again: they do not count. git's
    // threaded delta search hands work between threads as they finish, so
    // the pack a multi-threaded repack writes (and with it what is reused
    // below) depends on scheduling; one thread makes it reproducible.
    let mut fx = fx.git(["-c", "pack.threads=1", "repack", "-adq"]);
    progress_case(
        &mut fx,
        &[
            "-c",
            "pack.threads=2",
            "pack-objects",
            "--all-progress",
            "--all",
            "--stdout",
        ],
    );
}

fn progress_case(fx: &mut Fixture, args: &[&str]) {
    let mut cmp = fx
        .compare(args)
        .stdin("")
        .checks([Check::ExitCode, Check::Stderr]);
    for n in progress_normalizers() {
        cmp = cmp.normalize_stderr(n);
    }
    cmp.run();
}

/// `--include-tag`, `--incremental`, `--unpacked`, `--honor-pack-keep`,
/// `--keep-pack=<name>`, `--keep-true-parents`.
#[test]
fn selection_options() {
    let fx = repo();
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx;
    for args in [
        &["--revs", "--stdout", "--include-tag"][..],
        &["--revs", "--stdout", "--incremental"],
        &["--revs", "--stdout", "--unpacked"],
        &["--revs", "--stdout", "--keep-true-parents"],
    ] {
        fx = same_objects(fx, args, "HEAD~1\n");
    }
    // Object list input with --include-tag.
    let list = String::from_utf8(fx.git_stdout(["rev-list", "--objects", "main~2"])).unwrap();
    fx = same_objects(fx, &["--stdout", "--include-tag"], &list);
    let pack_dir = fx.work_dir().join(".git/objects/pack");
    let pack = std::fs::read_dir(&pack_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .find(|n| n.ends_with(".pack"))
        .unwrap();
    let keep = format!("--keep-pack={pack}");
    fx = same_objects(fx, &["--revs", "--stdout", &keep], "HEAD\n");
    std::fs::write(pack_dir.join(pack.replace(".pack", ".keep")), "").unwrap();
    fx = same_objects(fx, &["--revs", "--stdout", "--honor-pack-keep"], "HEAD\n");
    // Reflog entries and index entries as further starting points.
    fx = fx
        .git(["reset", "-q", "--hard", "HEAD~2"])
        .write("staged", "staged\n")
        .git(["add", "staged"]);
    fx = same_objects(fx, &["--stdout", "--reflog"], "");
    fx = same_objects(fx, &["--stdout", "--indexed-objects"], "");
    fx = same_objects(
        fx,
        &["--stdout", "--reflog", "--indexed-objects"],
        "HEAD~1\n",
    );
    same_objects(fx, &["--revs", "--stdout"], "HEAD\n");
}

/// `--local` leaves out objects the alternates have, even with a local
/// copy.
#[test]
fn local_option() {
    let fx = fixture().git(["init", "-q", "alt"]);
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx
        .write("alt/f", "one\n")
        .git(["-C", "alt", "add", "f"])
        .git(["-C", "alt", "commit", "-q", "-m", "one"])
        .tick()
        .git(["clone", "-q", "--shared", "alt", "o"])
        .write("o/g", "two\n")
        .git(["-C", "o", "add", "g"])
        .git(["-C", "o", "commit", "-q", "-m", "two"])
        .tick()
        .git(["-C", "o", "repack", "-adq"]);
    let args = ["pack-objects", "--revs", "--stdout", "--local", "-q"];
    let Some(out) = fx
        .compare(args)
        .in_dir("o")
        .stdin("HEAD\n")
        .checks([Check::ExitCode, Check::Stderr])
        .run()
    else {
        return;
    };
    // Same number of objects in the pack header.
    assert_eq!(out.git.stdout[8..12], out.rgit.stdout[8..12]);
}
