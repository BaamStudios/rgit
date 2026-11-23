// SPDX-License-Identifier: MIT
//! `git refs verify` against the reference git: findings, levels and exit
//! codes for loose references, symbolic references, root references,
//! worktree references and `packed-refs`.

use rgit_difftest::Fixture;

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .git(["commit", "-q", "--allow-empty", "-m", "one"])
        .git(["tag", "-a", "-m", "t", "t1"])
        .git(["branch", "side"])
}

/// Changes a fresh fixture (given the id of `HEAD`).
type Tweak<'a> = &'a dyn Fn(Fixture, &str) -> Fixture;

fn head(fx: &Fixture) -> String {
    String::from_utf8(fx.git_stdout(["rev-parse", "HEAD"]))
        .unwrap()
        .trim()
        .to_owned()
}

/// Runs `cases` (each a fixture tweak) and compares `refs verify` (with
/// the given extra arguments) for each.
fn each(cases: &[(&str, Tweak<'_>)], args: &[&str]) {
    let mut failures = Vec::new();
    for (label, tweak) in cases {
        let base = fixture();
        if base.is_skipped() {
            return;
        }
        let h = head(&base);
        let mut fx = tweak(base, &h);
        let mut argv = vec!["refs", "verify"];
        argv.extend_from_slice(args);
        if let Err(m) = fx.compare(&argv).try_run() {
            failures.push(format!("{label}: {m}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn loose_refs() {
    each(
        &[
            ("clean", &|fx, _| fx),
            ("bad name", &|fx, h| {
                fx.write(".git/refs/heads/.dot", format!("{h}\n"))
            }),
            ("lock ignored", &|fx, h| {
                fx.write(".git/refs/tags/x.lock", format!("{h}\n"))
            }),
            ("bare lock", &|fx, _| fx.write(".git/refs/heads/.lock", "")),
            ("bad content", &|fx, _| {
                fx.write(".git/refs/heads/bad", "foobar\n")
            }),
            ("oid then x", &|fx, h| {
                fx.write(".git/refs/heads/bad", format!("{h}x"))
            }),
            ("no newline", &|fx, h| fx.write(".git/refs/heads/nonl", h)),
            ("garbage", &|fx, h| {
                fx.write(".git/refs/heads/g", format!("{h} garbage"))
            }),
            ("newlines", &|fx, h| {
                fx.write(".git/refs/heads/g", format!("{h}\n\n\n  x"))
            }),
            ("symref ok", &|fx, _| {
                fx.write(".git/refs/heads/s", "ref: refs/heads/main\n")
            }),
            ("symref HEAD", &|fx, _| {
                fx.write(".git/refs/heads/s", "ref: HEAD\n")
            }),
            ("bad referent", &|fx, _| {
                fx.write(".git/refs/heads/s", "ref: refs/heads/.x\n")
            }),
            ("one-level referent", &|fx, _| {
                fx.write(".git/refs/heads/s", "ref: main\n")
            }),
            ("pseudo referent", &|fx, _| {
                fx.write(".git/refs/heads/s", "ref: FETCH_HEAD\n")
            }),
            ("non-ref target", &|fx, _| {
                fx.write(".git/refs/heads/s", "ref: logs/x/y\n")
            }),
            ("trailing", &|fx, _| {
                fx.write(".git/refs/heads/s", "ref: refs/heads/main \n  ")
            }),
            ("double newline", &|fx, _| {
                fx.write(".git/refs/heads/s", "ref: refs/heads/main\n\n")
            }),
            ("empty referent", &|fx, _| {
                fx.write(".git/refs/heads/s", "ref: \n")
            }),
            ("symlink", &|fx, _| {
                fx.symlink("./main", ".git/refs/heads/link")
            }),
            ("symlink out", &|fx, _| {
                fx.symlink("../../logs/x", ".git/refs/heads/link")
            }),
            ("symlink bad", &|fx, _| {
                fx.symlink("./.x", ".git/refs/tags/link")
            }),
            ("broken HEAD", &|fx, _| {
                fx.write(".git/HEAD", "ref: refs/heads/../HEAD\n")
            }),
            ("bad ORIG_HEAD", &|fx, _| {
                fx.write(".git/ORIG_HEAD", "garbage\n")
            }),
        ],
        &[],
    );
}

#[test]
fn levels_and_options() {
    let nonl: Tweak<'_> = &|fx, h| fx.write(".git/refs/heads/nonl", h);
    let bad: Tweak<'_> = &|fx, _| fx.write(".git/refs/heads/.bad", "x\n");
    each(&[("strict", nonl)], &["--strict"]);
    // `--verbose` lists entries in directory order in git (name order
    // here), so it is not compared.
    each(&[("argument", bad)], &["x"]);
    let mut failures = Vec::new();
    for (config, tweak) in [
        ("fsck.badRefName=warn", bad),
        ("fsck.badRefName=ignore", bad),
        ("fsck.refMissingNewline=error", nonl),
        ("fsck.badRefContent=bogus", bad),
    ] {
        let base = fixture();
        if base.is_skipped() {
            return;
        }
        let h = head(&base);
        let mut fx = tweak(base, &h);
        if let Err(m) = fx.compare(["-c", config, "refs", "verify"]).try_run() {
            failures.push(format!("{config}: {m}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn packed_refs() {
    let mut failures = Vec::new();
    let base = fixture();
    if base.is_skipped() {
        return;
    }
    let h = head(&base);
    let short = &h[..4];
    let header = "# pack-refs with: peeled fully-peeled sorted \n";
    let contents = [
        String::new(),
        "# pack-refs wit: peeled\n".to_owned(),
        "# pack-refs with: peeled fully-peeled sorted foo\n".to_owned(),
        format!("{h} refs/heads/main\n"),
        format!(
            "{header}{short} refs/heads/a\n{h}x\n{h}   refs/heads/b\n{h} refs/heads/c.\n^{short}\n{h} refs/heads/d\n^{h} junk\n"
        ),
        format!("{header}{h} refs/heads/z\n{h} refs/heads/a\n"),
        format!("# pack-refs with: peeled\n{h} refs/heads/z\n{h} refs/heads/a\n"),
        format!("{header}^{h}\n{h} refs/heads/a\n{h} refs/heads/b"),
        format!("{h} HEAD\n\n#c\n{h}\n"),
    ];
    for (i, text) in contents.iter().enumerate() {
        let mut fx = fixture().write(".git/packed-refs", text);
        if let Err(m) = fx.compare(["refs", "verify"]).try_run() {
            failures.push(format!("packed-refs {i}: {m}"));
        }
    }
    let mut fx = fixture()
        .write(".git/packed-refs-x", "")
        .symlink("packed-refs-x", ".git/packed-refs");
    if let Err(m) = fx.compare(["refs", "verify"]).try_run() {
        failures.push(format!("symlink: {m}"));
    }
    let mut fx = fixture().write(".git/packed-refs/x", "");
    if let Err(m) = fx.compare(["refs", "verify"]).try_run() {
        failures.push(format!("directory: {m}"));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn worktrees() {
    // One finding per run: git lists them in directory order.
    let tweaks: [Tweak<'_>; 4] = [
        &|fx, h| fx.write(".git/worktrees/wt1/refs/worktree/~bad", format!("{h}\n")),
        &|fx, h| fx.write(".git/worktrees/wt1/refs/worktree/nonl", h),
        &|fx, _| fx.symlink("./.x", ".git/worktrees/wt1/refs/worktree/link"),
        &|fx, _| fx.write(".git/worktrees/wt1/ORIG_HEAD", "ref: refs/heads/../HEAD\n"),
    ];
    for tweak in tweaks {
        let fx = fixture()
            .git(["worktree", "add", "-q", "wt1", "side"])
            .git(["-C", "wt1", "update-ref", "refs/worktree/x", "HEAD"]);
        if fx.is_skipped() {
            return;
        }
        let h = head(&fx);
        let mut fx = tweak(fx, &h);
        fx.compare(["refs", "verify"]).run();
        fx.compare(["-C", "wt1", "refs", "verify"]).run();
    }
}
