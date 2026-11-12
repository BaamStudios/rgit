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
    assert!(!ours.is_empty() || input.is_empty());
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

/// `--thin` is accepted, but rgit's pack is never thin: it must be
/// accepted by `index-pack --strict` without `--fix-thin`.
#[test]
fn thin_is_not_thin() {
    let mut fx = repo();
    let Some(outcome) = fx
        .compare(["pack-objects", "--revs", "--stdout", "--thin"])
        .stdin("HEAD\n^HEAD~3\n")
        .checks([Check::ExitCode, Check::Stderr])
        .run()
    else {
        return;
    };
    let expect =
        String::from_utf8(fx.git_stdout(["rev-list", "--objects", "HEAD", "^HEAD~3"])).unwrap();
    let mut expect: Vec<String> = expect.lines().map(|l| l[..40].to_string()).collect();
    expect.sort();
    let (_, ours) = check_pack(fx, &outcome.rgit.stdout, "ours");
    assert_eq!(ours, expect);
}
