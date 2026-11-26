// SPDX-License-Identifier: MIT
//! Differential tests of directory/file conflicts when the index and the
//! work tree move to another tree (#674): `checkout`/`switch`, `reset`,
//! `read-tree -m [-u]`, with staged, unstaged, untracked and ignored
//! content where a directory becomes a file or a file a directory.
//! Whether other paths sort between `d` and `d/` (`d-`, `d.x`) changes
//! what git does with index entries below `d/`, so every case runs with
//! and without such siblings.

use rgit_difftest::Fixture;

/// `main`: d/a, d/b/c, e, z (and with `sib` d-, d.x, e-, e.x);
/// `f`: d is a file; `g`: e is a directory (e/x); `gx`: e/x and e/y;
/// `gone`: no e; `emod`: e changed; `none`: no e (checked out with
/// `start`).
fn fixture(sib: bool, start: &str) -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("d/a", "a\n")
        .write("d/b/c", "c\n")
        .write("e", "e\n")
        .write("z", "z\n")
        .write(".gitignore", "*.ign\n");
    if sib {
        fx = fx
            .write("d-", "s\n")
            .write("d.x", "s\n")
            .write("e-", "s\n")
            .write("e.x", "s\n");
    }
    fx.git(["add", "."])
        .git(["commit", "-q", "-m", "init"])
        .git(["checkout", "-q", "-b", "f"])
        .remove("d")
        .write("d", "file\n")
        .git(["add", "-A"])
        .git(["commit", "-q", "-m", "f"])
        .git(["checkout", "-q", "-b", "g", "main"])
        .remove("e")
        .write("e/x", "x\n")
        .git(["add", "-A"])
        .git(["commit", "-q", "-m", "g"])
        .git(["checkout", "-q", "-b", "gx", "main"])
        .remove("e")
        .write("e/x", "x\n")
        .write("e/y", "y\n")
        .git(["add", "-A"])
        .git(["commit", "-q", "-m", "gx"])
        .git(["checkout", "-q", "-b", "gone", "main"])
        .git(["rm", "-q", "e"])
        .git(["commit", "-q", "-m", "gone"])
        .git(["checkout", "-q", "-b", "emod", "main"])
        .write("e", "e2\n")
        .git(["commit", "-q", "-a", "-m", "emod"])
        .git(["checkout", "-q", "-b", "none", "main"])
        .git(["rm", "-q", "e"])
        .git(["commit", "-q", "-m", "none"])
        .git(["checkout", "-q", start])
}

/// Every command in `cmds` (with `{}` replaced by `target`) in a fresh
/// fixture prepared by `prep`, with and without siblings.
fn cases(start: &str, prep: fn(Fixture) -> Fixture, target: &str, cmds: &[&str]) {
    for sib in [false, true] {
        for cmd in cmds {
            let fx = prep(fixture(sib, start));
            if fx.is_skipped() {
                return;
            }
            let mut fx = fx;
            let args: Vec<String> = cmd
                .split(' ')
                .map(|a| {
                    if a == "{}" {
                        target.to_string()
                    } else {
                        a.to_string()
                    }
                })
                .collect();
            fx.compare(&args).run();
        }
    }
}

const ALL: &[&str] = &[
    "checkout {}",
    "checkout -f {}",
    "switch {}",
    "reset --hard {}",
    "reset --merge {}",
    "reset --keep {}",
    "read-tree -m HEAD {}",
    "read-tree -u -m HEAD {}",
    "read-tree -u --reset {}",
];

const TWO_WAY: &[&str] = &[
    "checkout {}",
    "read-tree -m HEAD {}",
    "read-tree -u -m HEAD {}",
];

/// t1092 "checkout behaves oddly with df-conflict-1/2": a staged new
/// file in a directory that becomes a file. Without siblings it is
/// dropped with the directory; with them it stays and the file is not
/// checked out.
#[test]
fn staged_file_in_directory_that_becomes_a_file() {
    cases(
        "main",
        |fx| fx.write("d/new", "n\n").git(["add", "d/new"]),
        "f",
        ALL,
    );
    cases(
        "main",
        |fx| {
            fx.write("d/new", "n\n")
                .git(["add", "d/new"])
                .write("d/new", "n2\n")
        },
        "f",
        TWO_WAY,
    );
    cases(
        "main",
        |fx| fx.write("d/new", "n\n").git(["add", "-N", "d/new"]),
        "f",
        TWO_WAY,
    );
    cases(
        "main",
        |fx| fx.write("d/a", "m\n").git(["add", "d/a"]),
        "f",
        TWO_WAY,
    );
    cases(
        "main",
        |fx| fx.write("d/b/c", "m\n").git(["add", "d/b/c"]),
        "f",
        TWO_WAY,
    );
}

/// Untracked, ignored and modified files in a directory that becomes a
/// file, and a file where the directory was.
#[test]
fn directory_becomes_file_with_local_content() {
    cases("main", |fx| fx.write("d/u", "u\n"), "f", ALL);
    cases("main", |fx| fx.write("d/b/u", "u\n"), "f", TWO_WAY);
    cases("main", |fx| fx.write("d/u.ign", "u\n"), "f", TWO_WAY);
    cases("main", |fx| fx.write("d/a", "m\n"), "f", TWO_WAY);
    cases(
        "main",
        |fx| fx.remove("d").write("d", "file\n"),
        "f",
        TWO_WAY,
    );
    cases(
        "main",
        |fx| fx.write("d/new", "n\n").git(["add", "d/new"]).remove("d"),
        "f",
        TWO_WAY,
    );
}

/// A directory (empty, with untracked or ignored files, or a symbolic
/// link) where the tracked file e is: it goes away, changes or becomes a
/// directory.
#[test]
fn directory_in_place_of_tracked_file() {
    let preps: &[fn(Fixture) -> Fixture] = &[
        |fx| fx.remove("e").write("e/y", "y\n"),
        |fx| fx.remove("e").write("e/x", "x\n"),
        |fx| fx.remove("e").write("e/y.ign", "y\n"),
        |fx| fx.write("e", "m\n"),
        |fx| fx.write("e", "m\n").git(["add", "e"]),
    ];
    for prep in preps {
        cases("main", *prep, "g", ALL);
        cases("main", *prep, "gone", TWO_WAY);
        cases("main", *prep, "emod", TWO_WAY);
    }
}

/// e removed from the index and its directory staged instead (e/y).
#[test]
fn staged_directory_in_place_of_file() {
    let staged = |fx: Fixture| {
        fx.git(["rm", "-q", "e"])
            .write("e/y", "y\n")
            .git(["add", "e/y"])
    };
    for target in ["g", "gx", "gone", "emod", "main"] {
        cases("main", staged, target, TWO_WAY);
    }
    cases(
        "main",
        |fx| {
            fx.git(["rm", "-q", "e"])
                .write("e/y", "y\n")
                .git(["add", "e/y"])
                .write("e/u", "u\n")
        },
        "gone",
        TWO_WAY,
    );
    cases(
        "main",
        |fx| {
            fx.git(["rm", "-q", "e"])
                .write("e/y", "y\n")
                .git(["add", "e/y"])
                .write("e/y", "y2\n")
        },
        "gx",
        TWO_WAY,
    );
    // A staged new file e where the target needs the directory e: git
    // reports it as untracked.
    cases(
        "none",
        |fx| fx.write("e", "st\n").git(["add", "e"]),
        "g",
        TWO_WAY,
    );
    // No e at the start; the target adds the file e.
    cases(
        "none",
        |fx| fx.write("e/y", "y\n").git(["add", "e/y"]),
        "main",
        ALL,
    );
}

/// A file that the index no longer tracks where the old tree's file
/// goes away ("would be removed").
#[test]
fn untracked_file_where_old_file_goes_away() {
    for target in ["gone", "g", "emod"] {
        cases(
            "main",
            |fx| fx.git(["rm", "-q", "--cached", "e"]),
            target,
            TWO_WAY,
        );
    }
    cases(
        "main",
        |fx| fx.git(["rm", "-q", "--cached", "e"]),
        "gone",
        &["switch {}", "reset --merge {}", "reset --keep {}"],
    );
    cases(
        "main",
        |fx| {
            fx.git(["rm", "-q", "--cached", "e"])
                .write(".git/info/exclude", "e\n")
        },
        "gone",
        TWO_WAY,
    );
}
