// SPDX-License-Identifier: MIT
//! Differential tests of the whitespace options (`-w`, `-b`,
//! `--ignore-space-at-eol`, `--ignore-cr-at-eol`) and of ignorable
//! changes (`--ignore-blank-lines`, `-I<regex>`) against the reference
//! git.

use rgit_difftest::{Check, Fixture};

const NUMBERS: &str = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n15\n16\n";

/// Blank lines near and far from real changes.
const BLANKS: &str = "1\n\n2\nX\n4\n5\n\n6\n7\n8\n9\n10\n\n11\nY\n13\n14\n15\n16\n";

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("ws", "a\nb  c\nd\ne\nf\ng\n")
        .write("eol", "a\nb\r\nc \n")
        .write("blanks", NUMBERS)
        .write("comments", "x\ny\n")
        .write("same", "p\nq\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "first"])
        .tick()
        .write("ws", "a \nb c\nd\nE\nf\n\tg\n")
        .write("eol", "a\nb\nc")
        .write("blanks", BLANKS)
        .write("comments", "x\n# note\ny\n\n")
        .write("same", "p \nq\t\n")
        .git(["commit", "-q", "-a", "-m", "second"])
        .tick()
        .write("ws", "a \nb c\nd\nE\nf\n\tg\nh\n")
        .write("same", "p\nq\n\n")
}

#[test]
fn whitespace_options_match_git() {
    let mut fx = fixture();
    let mut failures = Vec::new();
    let options: &[&[&str]] = &[
        &["-w"],
        &["-b"],
        &["--ignore-all-space"],
        &["--ignore-space-change"],
        &["--ignore-space-at-eol"],
        &["--ignore-cr-at-eol"],
        &["--ignore-blank-lines"],
        &["--ignore-blank-lines", "-U1"],
        &["--ignore-blank-lines", "-U0"],
        &["--ignore-blank-lines", "-w"],
        &["-I^#"],
        &["-I", "^#"],
        &["--ignore-matching-lines=^#", "--ignore-blank-lines"],
        &["-I^#", "-I^$"],
    ];
    let formats: &[&[&str]] = &[
        &["-p"],
        &["--raw"],
        &["--stat"],
        &["--numstat"],
        &["--name-status"],
        &["--exit-code", "--stat"],
    ];
    for o in options {
        for f in formats {
            let mut tree = vec!["diff-tree", "-r"];
            tree.extend_from_slice(o);
            tree.extend_from_slice(f);
            tree.extend_from_slice(&["HEAD^", "HEAD"]);
            let mut wt = vec!["diff"];
            wt.extend_from_slice(o);
            wt.extend_from_slice(f);
            for argv in [tree, wt] {
                if let Err(m) = fx.compare(&argv).checks(Check::OUTPUT).try_run() {
                    failures.push(format!("{argv:?}\n{}", m.report()));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn bad_ignore_regex() {
    fixture()
        .compare(["diff", "-I(", "HEAD^", "HEAD"])
        .checks(Check::OUTPUT)
        .run();
}
