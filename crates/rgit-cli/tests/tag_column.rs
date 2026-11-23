// SPDX-License-Identifier: MIT
//! `tag --column`, `column.tag` and `column.ui` against the reference git.

use rgit_difftest::{Check, Fixture};

#[test]
fn tag_columns() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .git(["commit", "-q", "--allow-empty", "-m", "c1"]);
    if fx.is_skipped() {
        return;
    }
    for t in [
        "a1", "aa1", "cba", "t210", "t211", "v0.2.1", "v1.0", "v1.0.1", "v1.1.3",
    ] {
        fx = fx.git(["tag", t]);
    }
    let modes = [
        "row",
        "column",
        "row,dense",
        "column dense",
        "plain",
        "always",
        "never",
    ];
    for width in ["20", "30", "40", "41", "80"] {
        for m in modes {
            let opt = format!("--column={m}");
            fx.compare(["tag", opt.as_str()])
                .env("COLUMNS", width)
                .checks(Check::OUTPUT)
                .run();
            let cfg = format!("column.ui={m}");
            fx.compare(["-c", cfg.as_str(), "tag"])
                .env("COLUMNS", width)
                .checks(Check::OUTPUT)
                .run();
        }
    }
    let mut fx = fx.git(["tag", "averyveryverylongtagname"]);
    let cases: &[&[&str]] = &[
        &["tag", "--column=row,dense"],
        &["tag", "--column=column,dense"],
        &["tag", "--column=bogus"],
        &["-c", "column.ui=bogus", "tag"],
        &["-c", "column.tag=bogus", "tag"],
        &["-c", "column.ui=always", "tag", "--no-column"],
        &["tag", "--column", "-n"],
        &["-c", "column.ui=always", "tag", "-n"],
        &["tag", "--column", "--format=%(refname)"],
        &["-c", "column.ui=always", "-c", "column.tag=never", "tag"],
    ];
    for args in cases {
        fx.compare(*args)
            .env("COLUMNS", "40")
            .checks(Check::OUTPUT)
            .run();
    }
}
