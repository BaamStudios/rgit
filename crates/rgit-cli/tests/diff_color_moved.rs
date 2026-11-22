// SPDX-License-Identifier: MIT
//! Differential tests of `--color-moved`, `--color-moved-ws`,
//! `diff.colorMoved`, `diff.colorMovedWS` and the moved-line colour
//! slots against the reference git.

use rgit_difftest::{Check, Fixture};

const POEM_OLD: &str = "long line 1\nlong line 2\nlong line 3\nline 4\nline 5\nline 6\n\
line 7\nline 8\nline 9\nline 10\nline 11\nline 12\nline 13\nlong line 14\nlong line 15\n\
long line 16\n";

const POEM_NEW: &str = "line 4\nline 5\nline 6\nline 7\nline 8\nline 9\nlong line 1\n\
long line 2\nlong line 3\nlong line 14\nlong line 15\nlong line 16\nline 10\nline 11\n\
line 12\nline 13\n";

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("poem", POEM_OLD)
        .write(
            "p",
            "aaaa line 1\naaaa line 2\naaaa line 3\nbbbb line 1\nbbbb line 2\nbbbb line 3\n\
             cccc line 1\ncccc line 2\ncccc line 3\nshort\n",
        )
        .write(
            "ws",
            "\tindented line one here\n\tindented line two here\n    spaced  out   words go\nend\n",
        )
        .write(
            "file",
            "A\nB\nC\none\ntwo\nthree\nfour\nfive\nsix\nseven\nD\nE\nF\nG\nH\nI\nJ\n",
        )
        .git(["add", "."])
        .git(["commit", "-q", "-m", "first"])
        .tick()
        .write("poem", POEM_NEW)
        .git(["rm", "-q", "p"])
        .write(
            "q",
            "bbbb line 1\nbbbb line 2\nbbbb line 3\naaaa line 1\naaaa line 2\naaaa line 3\n\
             short\ncccc line 1\ncccc line 2\ncccc line 3\n",
        )
        .write(
            "ws",
            "end\n        indented line one here\n  indented line two here\nspaced out words go  \n",
        )
        .write(
            "file",
            "one\ntwo\nA\nB\nC\nD\nE\nF\nG\nH\nI\nJ\ntwo\nthree\nfour\nfive\nsix\nseven\n",
        )
        .git(["add", "."])
}

#[test]
fn color_moved_matches_git() {
    let mut fx = fixture();
    let mut failures = Vec::new();
    let runs: &[&[&str]] = &[
        &["diff", "--cached", "--color", "--color-moved"],
        &["diff", "--cached", "--color", "--color-moved=plain"],
        &["diff", "--cached", "--color", "--color-moved=blocks"],
        &["diff", "--cached", "--color", "--color-moved=zebra"],
        &["diff", "--cached", "--color", "--color-moved=dimmed-zebra"],
        &["diff", "--cached", "--color", "--color-moved=no"],
        &[
            "diff",
            "--cached",
            "--color",
            "--color-moved",
            "--no-renames",
        ],
        &[
            "diff",
            "--cached",
            "--color",
            "--color-moved",
            "--color-moved-ws=ignore-all-space",
        ],
        &[
            "diff",
            "--cached",
            "--color",
            "--color-moved",
            "--color-moved-ws=ignore-space-change,ignore-space-at-eol",
        ],
        &[
            "diff",
            "--cached",
            "--color",
            "--color-moved",
            "--color-moved-ws=allow-indentation-change",
        ],
        &[
            "-c",
            "diff.colorMoved=dimmed-zebra",
            "diff",
            "--cached",
            "--color",
        ],
        &["-c", "diff.colorMoved=true", "diff", "--cached", "--color"],
        &[
            "-c",
            "diff.colorMoved=zebra",
            "-c",
            "diff.colorMovedWS=ignore-all-space",
            "diff",
            "--cached",
            "--color",
        ],
        &[
            "-c",
            "color.diff.oldMoved=yellow",
            "-c",
            "color.diff.newMovedAlternative=blue reverse",
            "diff",
            "--cached",
            "--color",
            "--color-moved",
        ],
        &["diff", "--cached", "--color-moved"],
        &["diff", "--color-moved=bogus"],
        &["diff", "--color-moved-ws=bogus"],
        &[
            "diff",
            "--color-moved-ws=allow-indentation-change,ignore-all-space",
        ],
        &["-c", "diff.colorMoved=bogus", "diff"],
        &["-c", "diff.colorMovedWS=bogus", "diff"],
    ];
    for argv in runs {
        if let Err(m) = fx.compare(*argv).checks(Check::OUTPUT).try_run() {
            failures.push(format!("{argv:?}\n{}", m.report()));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
