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

/// Whitespace errors: `--check`, their colours, `core.whitespace`, the
/// `whitespace` attribute and `--ws-error-highlight`.
#[test]
fn whitespace_errors_match_git() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("f", "a\nb\n\nold \nq\r\n")
        .write("g", "x\n")
        .write("h", "one")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "first"])
        .tick()
        .write(
            "f",
            "a \nb\n  \tc\n\td\n        e\n\tx\r\n \t g \n\t \t\tz\n\nnew \nq\r\n\n\n",
        )
        .write("g", "x\n          y\n\t        w\nz")
        .write("h", "two\n")
        .write(".gitattributes", "g whitespace=indent,-trail\n");
    let mut failures = Vec::new();
    let configs: &[&[&str]] = &[
        &[],
        &["-c", "core.whitespace=tab-in-indent"],
        &["-c", "core.whitespace=tab-in-indent,-space-before-tab"],
        &["-c", "core.whitespace=cr-at-eol,incomplete-line,tabwidth=4"],
        &["-c", "core.whitespace=-trailing-space,indent"],
        &["-c", "diff.wsErrorHighlight=all"],
        &["-c", "diff.color.whitespace=blue reverse"],
    ];
    let commands: &[&[&str]] = &[
        &["diff", "--check"],
        &["diff", "--check", "-R"],
        &["diff", "--color"],
        &["diff", "--color", "-R"],
        &["diff", "--color", "--ws-error-highlight=old,context"],
        &["diff", "--color", "--ws-error-highlight=none"],
    ];
    for c in configs {
        for cmd in commands {
            let mut argv = c.to_vec();
            argv.extend_from_slice(cmd);
            if let Err(m) = fx.compare(&argv).checks(Check::OUTPUT).try_run() {
                failures.push(format!("{argv:?}\n{}", m.report()));
            }
        }
    }
    for argv in [
        &["-c", "core.whitespace=tab-in-indent,indent", "diff"][..],
        &["diff", "--ws-error-highlight=bogus"],
        &["-c", "diff.wsErrorHighlight=bogus", "diff"],
        &["diff", "--check", "--name-only"],
    ] {
        if let Err(m) = fx.compare(argv).checks(Check::OUTPUT).try_run() {
            failures.push(format!("{argv:?}\n{}", m.report()));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
