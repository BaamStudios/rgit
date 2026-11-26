// SPDX-License-Identifier: MIT
//! Differential tests of `rgit range-diff` and `format-patch
//! --range-diff` against the reference git: two versions of a series with
//! a dropped, an added, a reworded, a changed, a renamed and a mode-only
//! commit, compared in all three argument forms with diff and colour
//! options, and the argument errors.

use rgit_difftest::{Check, Fixture};

fn lines(range: std::ops::Range<u32>, change: &dyn Fn(u32) -> String) -> String {
    range.map(|i| change(i) + "\n").collect()
}

fn commit(fx: Fixture, msg: &str) -> Fixture {
    fx.tick().git(["commit", "-q", "-a", "-m", msg])
}

/// `main` with a base, `v1` and `v2` as two versions of a series on it,
/// and `rebased` as `v1` on a newer `main`.
fn history() -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"]);
    if fx.is_skipped() {
        return fx;
    }
    let plain = |i: u32| format!("line {i}");
    fx = fx
        .write("file", lines(1..40, &plain))
        .write("other", lines(1..10, &|i| format!("other {i}")))
        .git(["add", "."]);
    fx = commit(fx, "base");
    fx = fx.git(["branch", "base"]);

    // Version 1.
    fx = fx.git(["checkout", "-q", "-b", "v1"]);
    fx = fx.write(
        "file",
        lines(1..40, &|i| if i == 5 { "five".into() } else { plain(i) }),
    );
    fx = commit(fx, "change five");
    fx = fx.write(
        "file",
        lines(1..40, &|i| match i {
            5 => "five".into(),
            12 => "twelve".into(),
            _ => plain(i),
        }),
    );
    fx = commit(fx, "change twelve");
    fx = fx.write("other", lines(1..10, &|i| format!("other {i} v1")));
    fx = commit(fx, "rewrite other");
    fx = fx.write(
        "file",
        lines(1..40, &|i| match i {
            5 => "five".into(),
            12 => "twelve".into(),
            30 => "thirty".into(),
            _ => plain(i),
        }),
    );
    fx = commit(fx, "change thirty");

    // Version 2: "change five" unchanged, "change twelve" changed,
    // "rewrite other" dropped, a new commit, "change thirty" reworded.
    fx = fx.git(["checkout", "-q", "-b", "v2", "base"]);
    fx = fx.write(
        "file",
        lines(1..40, &|i| if i == 5 { "five".into() } else { plain(i) }),
    );
    fx = commit(fx, "change five");
    fx = fx.write(
        "file",
        lines(1..40, &|i| match i {
            5 => "five".into(),
            12 => "TWELVE".into(),
            _ => plain(i),
        }),
    );
    fx = commit(fx, "change twelve");
    fx = fx.write("added", "a new file\n").git(["add", "added"]);
    fx = commit(fx, "add a file");
    fx = fx.write(
        "file",
        lines(1..40, &|i| match i {
            5 => "five".into(),
            12 => "TWELVE".into(),
            30 => "thirty".into(),
            _ => plain(i),
        }),
    );
    fx = fx.tick().git([
        "commit",
        "-q",
        "-a",
        "-m",
        "change thirty\n\nWith a body this time.",
    ]);
    fx = fx
        .git(["mv", "added", "renamed"])
        .git(["update-index", "--chmod=+x", "renamed"]);
    fx = commit(fx, "rename and chmod");

    // `v1` rebased onto a newer base.
    fx = fx.git(["checkout", "-q", "main"]);
    fx = fx
        .write("unrelated", "unrelated\n")
        .git(["add", "unrelated"]);
    fx = commit(fx, "unrelated");
    fx = fx
        .git(["checkout", "-q", "-b", "rebased", "v1"])
        .git(["rebase", "-q", "main"]);
    fx
}

#[test]
fn range_diff_forms_and_options() {
    let mut fx = history();
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["base", "v1", "v2"],
        &["base..v1", "base..v2"],
        &["v1...v2"],
        &["v1...rebased"],
        &["base", "v1", "rebased"],
        &["v1~1^!", "v2~1^!"],
        &["v1^-1", "v2~1^-1"],
        &["base", "v1", "v2", "--", "other"],
        &["base", "v1", "v2", "--", "file"],
        &["base..v1", "base..v2", "file"],
        &["--no-patch", "base", "v1", "v2"],
        &["-s", "base", "v1", "v2"],
        &["--stat", "base", "v1", "v2"],
        &["-U1", "base", "v1", "v2"],
        &["-U10", "base", "v1", "v2"],
        &["--inter-hunk-context=4", "base", "v1", "v2"],
        &["--abbrev=10", "base", "v1", "v2"],
        &["--no-abbrev", "base", "v1", "v2"],
        &["--creation-factor=0", "base", "v1", "v2"],
        &["--creation-factor=200", "base", "v1", "v2"],
        &["--creation-factor=1k", "base", "v1", "v2"],
        &["--left-only", "base", "v1", "v2"],
        &["--right-only", "base", "v1", "v2"],
        &["--color", "base", "v1", "v2"],
        &["--color", "--no-dual-color", "base", "v1", "v2"],
        &["--color", "--creation-factor=200", "base", "v1", "v2"],
        &["--diff-merges=1", "base", "v1", "v2"],
        &["-c", "diff.noprefix=true", "range-diff", "base", "v1", "v2"],
        &["-c", "format.pretty=%H", "range-diff", "base", "v1", "v2"],
        &[
            "-c",
            "color.diff.newBold=cyan",
            "range-diff",
            "--color",
            "base",
            "v1",
            "v2",
        ],
        &[
            "-c",
            "color.diff.oldDimmed=magenta",
            "range-diff",
            "--color",
            "base",
            "v1",
            "v2",
        ],
        &[
            "-c",
            "color.diff.commit=blue",
            "range-diff",
            "--color",
            "base",
            "v1",
            "v2",
        ],
    ];
    for case in cases {
        let argv: Vec<&str> = if case.first() == Some(&"-c") {
            case.to_vec()
        } else {
            std::iter::once("range-diff")
                .chain(case.iter().copied())
                .collect()
        };
        fx.compare(&argv).checks(Check::OUTPUT).run();
    }
}

#[test]
fn range_diff_errors() {
    let mut fx = history();
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &[],
        &["v1"],
        &["v1", "v2"],
        &["base..v1", "v2"],
        &["base..v1", "v2^{/..}", "--"],
        &["base..v1", "--", "file"],
        &["base..v1", "base..v2", "base..v2", "--"],
        &["base", "v1", "v2", "x", "--"],
        &["base", "v1", "nope"],
        &["base..v1", "nope"],
        &["base..v1", "v2^-2", "--"],
        &["--left-only", "--right-only", "base", "v1", "v2"],
        &["--creation-factor=x", "base", "v1", "v2"],
        &["--max-memory=1", "base", "v1", "v2"],
        &["--no-such-option", "base", "v1", "v2"],
    ];
    for case in cases {
        let argv: Vec<&str> = std::iter::once("range-diff")
            .chain(case.iter().copied())
            .collect();
        fx.compare(&argv).checks(Check::OUTPUT).run();
    }
}

#[test]
fn format_patch_range_diff() {
    let mut fx = history();
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["--cover-letter", "--range-diff=v1", "base..v2"],
        &["--range-diff=v1", "base..v2"],
        &["-v3", "--range-diff=v1", "base..v2"],
        &["--range-diff=base..v1", "base..v2"],
        &["--range-diff=v1~3", "-1", "v2~4"],
        &["-v2", "--range-diff=v1~3", "-1", "v2~4"],
        &["--interdiff=v1", "--range-diff=v1", "-1", "v2"],
        &[
            "--cover-letter",
            "--interdiff=v1",
            "--range-diff=v1",
            "base..v2",
        ],
        &["--creation-factor=0", "--range-diff=v1", "base..v2"],
        &["--no-cover-letter", "--range-diff=v1", "base..v2"],
        &["--creation-factor=10", "-1", "v2"],
        &["--range-diff=nope", "-1", "v2"],
        &["--range-diff=base..nope", "-1", "v2"],
    ];
    for case in cases {
        // The default signature is the version, which differs by design.
        let argv: Vec<&str> = ["format-patch", "--stdout", "--no-signature"]
            .into_iter()
            .chain(case.iter().copied())
            .collect();
        fx.compare(&argv).checks(Check::OUTPUT).run();
    }
}

/// Notes of the compared commits: compared by default in `range-diff`,
/// only on request (`--notes`, `format.notes`) in `format-patch`, where
/// the standard notes leave the other refs out of the comparison.
#[test]
fn range_diff_notes() {
    let mut fx = history();
    if fx.is_skipped() {
        return;
    }
    fx = fx
        .git(["notes", "add", "-m", "v1 note", "v1~2"])
        .git(["notes", "add", "-m", "v2 note", "v2~3"])
        .git(["notes", "--ref=custom", "add", "-m", "v1 custom", "v1~2"])
        .git(["notes", "--ref=custom", "add", "-m", "v2 custom", "v2~3"])
        .git(["notes", "--ref=other", "add", "-m", "other", "v2~3"]);
    let cases: &[&[&str]] = &[
        &["range-diff", "--no-color", "base", "v1", "v2"],
        &["range-diff", "--no-color", "--no-notes", "base", "v1", "v2"],
        &[
            "range-diff",
            "--no-color",
            "--notes=custom",
            "base",
            "v1",
            "v2",
        ],
        &[
            "range-diff",
            "--no-color",
            "--notes=custom",
            "--notes=other",
            "base",
            "v1",
            "v2",
        ],
        &[
            "log",
            "--pretty=medium",
            "--show-notes-by-default",
            "-1",
            "v2~3",
        ],
        &[
            "log",
            "--pretty=medium",
            "--show-notes-by-default",
            "--notes=custom",
            "-1",
            "v2~3",
        ],
    ];
    for case in cases {
        fx.compare(*case).checks(Check::OUTPUT).run();
    }
    let fp: &[&[&str]] = &[
        &["--cover-letter", "--range-diff=v1", "base..v2"],
        &["--notes", "--cover-letter", "--range-diff=v1", "base..v2"],
        &[
            "--notes=custom",
            "--cover-letter",
            "--range-diff=v1",
            "base..v2",
        ],
        &[
            "--notes",
            "--no-notes",
            "--cover-letter",
            "--range-diff=v1",
            "base..v2",
        ],
        &[
            "-c",
            "format.notes=custom",
            "--range-diff=v1",
            "--cover-letter",
            "base..v2",
        ],
        &[
            "--notes=custom",
            "--notes=other",
            "-1",
            "--range-diff=v1~2",
            "v2~3",
        ],
        &[
            "--notes=custom",
            "--notes",
            "-1",
            "--range-diff=v1~2",
            "v2~3",
        ],
        &[
            "-c",
            "format.notes=other",
            "--notes=custom",
            "-1",
            "--range-diff=v1~2",
            "v2~3",
        ],
    ];
    for case in fp {
        let (config, rest): (Vec<&str>, Vec<&str>) = if case[0] == "-c" {
            (vec!["-c", case[1]], case[2..].to_vec())
        } else {
            (Vec::new(), case.to_vec())
        };
        let argv: Vec<&str> = config
            .into_iter()
            .chain(["format-patch", "--stdout", "--no-signature"])
            .chain(rest)
            .collect();
        fx.compare(&argv).checks(Check::OUTPUT).run();
    }
    fx = fx.git(["config", "--add", "format.notes", "true"]).git([
        "config",
        "--add",
        "format.notes",
        "other",
    ]);
    fx.compare([
        "format-patch",
        "--stdout",
        "--no-signature",
        "--cover-letter",
        "--range-diff=v1",
        "base..v2",
    ])
    .checks(Check::OUTPUT)
    .run();
}

#[test]
fn range_diff_help_outside_a_repository() {
    let mut fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"));
    if fx.is_skipped() {
        return;
    }
    fx.compare(["range-diff", "-h"]).checks(Check::OUTPUT).run();
    fx.compare(["range-diff", "a", "b"])
        .checks(Check::OUTPUT)
        .run();
}
