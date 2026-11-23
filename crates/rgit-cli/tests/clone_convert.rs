// SPDX-License-Identifier: MIT
//! Differential tests of the conversions (eol, text, ident, filter
//! drivers) of clone's initial checkout (#334).

use rgit_difftest::{Check, Fixture};

const CHECKS: [Check; 5] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Refs,
    Check::Worktree,
];

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main", "src"])
        .write(
            "src/.gitattributes",
            "*.crlf text eol=crlf\n*.lf text eol=lf\n*.auto text=auto\n*.f filter=up\n\
             *.id ident\n*.bin -text\n",
        )
        .write("src/x.crlf", "a\nb\n")
        .write("src/y.lf", "a\r\nb\r\n")
        .write("src/z.auto", "a\nb\n")
        .write("src/w.f", "hello\n")
        .write("src/v.id", "$Id$\n")
        .write("src/u.bin", "a\nb\n")
        .write("src/plain", "p\n")
        .git(["-C", "src", "add", "."])
        .git(["-C", "src", "commit", "-q", "-m", "one"])
}

#[test]
fn clone_checkout_conversions_same_as_git() {
    let cases: &[&[&str]] = &[
        &["clone", "-q", "src", "c"],
        &["-c", "core.autocrlf=true", "clone", "-q", "src", "c"],
        &["clone", "-q", "-c", "core.eol=crlf", "src", "c"],
        &[
            "clone",
            "-q",
            "-c",
            "filter.up.smudge=tr a-z A-Z",
            "src",
            "c",
        ],
        &[
            "clone",
            "-q",
            "-c",
            "filter.up.required=true",
            "-c",
            "filter.up.smudge=false",
            "src",
            "c",
        ],
    ];
    for argv in cases {
        let fx = fixture();
        if fx.is_skipped() {
            return;
        }
        let mut fx = fx;
        // The checked-out files are compared byte for byte.
        fx.compare(*argv).repo("c").checks(CHECKS).run();
    }
}
