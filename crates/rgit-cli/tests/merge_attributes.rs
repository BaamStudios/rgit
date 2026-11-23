// SPDX-License-Identifier: MIT
//! Differential tests of merge attributes and drivers (#153, #396): the
//! `merge` attribute (`text`, `binary`/`-merge`, `union`, custom drivers,
//! undefined drivers), `merge.default`, `conflict-marker-size`, custom
//! driver placeholders and exit codes, `-X renormalize`, against the
//! reference git.

use rgit_difftest::{Check, Fixture, Normalizer};

/// Files `text`, `binary`, `union`, `cust`, `nodrv` and `big` get a line
/// on `main` and another on `side`; `side` is checked out with the
/// attributes in `.gitattributes` (not committed).
fn fixture(attributes: &str) -> Fixture {
    let files = ["text", "binary", "union", "cust", "nodrv", "big"];
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"]);
    for f in files {
        fx = fx.write(f, "Initial\n");
    }
    fx = fx
        .git(["add", "."])
        .git(["commit", "-q", "-m", "Initial"])
        .git(["branch", "side"])
        .tick();
    for f in files {
        fx = fx.write(f, "Initial\nMain\n");
    }
    fx = fx
        .git(["commit", "-q", "-a", "-m", "Main"])
        .git(["checkout", "-q", "side"])
        .tick();
    for f in files {
        fx = fx.write(f, "Initial\nSide\n");
    }
    fx.git(["commit", "-q", "-a", "-m", "Side"])
        .tick()
        .write(".gitattributes", attributes)
        .write_executable(
            "drv",
            "#!/bin/sh\n\
             echo \"args: $*\" | sed 's/\\.merge_file_[A-Za-z0-9]*/F/g' >&2\n\
             { echo \"driver for $5\"; cat \"$3\"; } >\"$2\"\n\
             exit ${DRIVER_EXIT:-0}\n",
        )
}

const ATTRIBUTES: &str = "binary -merge\n\
                          union merge=union\n\
                          cust merge=custom\n\
                          nodrv merge=undefined\n\
                          big conflict-marker-size=10\n\
                          text merge=text\n";

fn cases(prep: impl Fn() -> Fixture, cases: &[&[&str]]) {
    let mut failed = Vec::new();
    for args in cases {
        let mut fx = prep();
        if fx.is_skipped() {
            return;
        }
        let c = fx
            .compare(*args)
            .normalize(Normalizer::regex(r"\.merge_file_[A-Za-z0-9]{6}", "F"))
            .git_file("AUTO_MERGE");
        if let Err(m) = c.try_run() {
            failed.push(m.to_string());
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

#[test]
fn drivers() {
    let prep = || {
        fixture(ATTRIBUTES).git([
            "config",
            "merge.custom.driver",
            "./drv %O %A %B %L %P %S %X %Y",
        ])
    };
    cases(
        prep,
        &[
            &["merge", "main"],
            &["merge", "-Xours", "main"],
            &["merge", "-Xtheirs", "main"],
            &["merge-tree", "--write-tree", "side", "main"],
        ],
    );
    cases(|| prep().env("DRIVER_EXIT", "1"), &[&["merge", "main"]]);
    // git runs the drivers of several paths in reverse path order; the
    // driver writes to standard error, so that is not compared here.
    let mut fx = prep()
        .git(["config", "merge.default", "custom"])
        .write(".gitattributes", "");
    if !fx.is_skipped() {
        fx.compare(["merge", "main"]).skip(Check::Stderr).run();
    }
    cases(
        || {
            prep()
                .git(["config", "merge.default", "union"])
                .write(".gitattributes", "")
        },
        &[&["merge", "main"]],
    );
}

#[test]
fn killed_driver() {
    let prep = || fixture("* merge=custom\n").git(["config", "merge.custom.driver", "kill -9 $$"]);
    let mut fx = prep();
    if fx.is_skipped() {
        return;
    }
    fx.compare(["merge", "main"]).skip(Check::Stderr).run();
}

#[test]
fn invalid_marker_size() {
    cases(
        || fixture("text conflict-marker-size=3a\nbig conflict-marker-size=0\n"),
        &[&["merge", "main"]],
    );
}

#[test]
fn renormalize() {
    // `main` converts the file to CR LF, `side` changes another line;
    // with text=auto and renormalization the line endings do not count.
    let prep = || {
        Fixture::new()
            .rgit(env!("CARGO_BIN_EXE_rgit"))
            .git(["init", "-q", "-b", "main"])
            .write("f", "a\nb\nc\n")
            .git(["add", "."])
            .git(["commit", "-q", "-m", "init"])
            .git(["branch", "side"])
            .tick()
            .write("f", "a\r\nb\r\nc\r\n")
            .git(["commit", "-q", "-a", "-m", "crlf"])
            .git(["checkout", "-q", "side"])
            .tick()
            .write("f", "a\nb\nC\n")
            .git(["commit", "-q", "-a", "-m", "side"])
            .tick()
            .write(".gitattributes", "* text=auto\n")
    };
    cases(
        prep,
        &[
            &["merge", "main"],
            &["merge", "-Xrenormalize", "main"],
            &["-c", "merge.renormalize=true", "merge", "main"],
            &[
                "-c",
                "merge.renormalize=true",
                "merge",
                "-Xno-renormalize",
                "main",
            ],
        ],
    );
}

#[test]
fn symlinks_with_favor() {
    let prep = || {
        Fixture::new()
            .rgit(env!("CARGO_BIN_EXE_rgit"))
            .git(["init", "-q", "-b", "main"])
            .symlink("target-zero", "link")
            .git(["add", "."])
            .git(["commit", "-q", "-m", "zero"])
            .git(["branch", "one"])
            .tick()
            .remove("link")
            .symlink("target-two", "link")
            .git(["commit", "-q", "-a", "-m", "two"])
            .git(["checkout", "-q", "one"])
            .tick()
            .remove("link")
            .symlink("target-one", "link")
            .git(["commit", "-q", "-a", "-m", "one"])
            .tick()
    };
    cases(
        prep,
        &[
            &["merge", "main"],
            &["merge", "-Xours", "main"],
            &["merge", "-Xtheirs", "main"],
        ],
    );
}
