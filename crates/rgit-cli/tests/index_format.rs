// SPDX-License-Identifier: MIT
//! Differential tests of the index file format options: version selection
//! (`index.version`, `GIT_INDEX_VERSION`, `feature.manyFiles`), the offset
//! tables (`index.threads`, `EOIE`, `IEOT`), `index.skipHash` and the
//! resolve undo extension. The index files must be byte-identical to git's.

use rgit_difftest::{Check, Fixture};

const OID: &str = "1234567890123456789012345678901234567890";

/// The index, compared byte by byte, and the output.
const BYTES: [Check; 5] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Index,
    Check::GitFiles,
];

/// A repository whose index has a cache tree and a resolve undo record
/// (`top` was conflicted and resolved with `git add`).
fn fixture() -> Fixture {
    let fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"));
    if fx.is_skipped() {
        return fx;
    }
    let mut fx = fx.git(["init", "-q", "-b", "main"]);
    for i in 0..12 {
        fx = fx.write(format!("d/f{i}"), format!("{i}\n"));
    }
    let fx = fx
        .write("top", "base\n")
        .write("e/f/g", "g\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "base"])
        .tick()
        .git(["checkout", "-q", "-b", "side"])
        .write("top", "side\n")
        .git(["commit", "-q", "-a", "-m", "side"])
        .tick()
        .git(["checkout", "-q", "main"])
        .write("top", "main\n")
        .git(["commit", "-q", "-a", "-m", "main"])
        .tick();
    let merge = fx.git_output(["merge", "-q", "side"]);
    assert!(!merge.status.success(), "the merge conflicts");
    fx.write("top", "resolved\n").git(["add", "top"])
}

fn bytes(fx: &mut Fixture, args: &[&str]) {
    fx.compare(args).checks(BYTES).git_file("index").run();
}

#[test]
fn rewrite_with_offset_tables() {
    for args in [
        &["update-index", "--force-write-index"][..],
        &["update-index", "--index-version", "4"],
        &["update-index", "--index-version", "3"],
        &[
            "-c",
            "index.threads=2",
            "update-index",
            "--force-write-index",
        ],
        &[
            "-c",
            "index.threads=3",
            "update-index",
            "--index-version",
            "4",
        ],
        &[
            "-c",
            "index.threads=64",
            "update-index",
            "--force-write-index",
        ],
        &[
            "-c",
            "index.threads=true",
            "update-index",
            "--force-write-index",
        ],
        &[
            "-c",
            "index.threads=0",
            "update-index",
            "--force-write-index",
        ],
        &[
            "-c",
            "index.threads=false",
            "update-index",
            "--force-write-index",
        ],
        &[
            "-c",
            "index.threads=1",
            "update-index",
            "--force-write-index",
        ],
        &[
            "-c",
            "index.recordEndOfIndexEntries=true",
            "update-index",
            "--force-write-index",
        ],
        &[
            "-c",
            "index.threads=4",
            "-c",
            "index.recordEndOfIndexEntries=false",
            "update-index",
            "--force-write-index",
        ],
        &[
            "-c",
            "index.threads=4",
            "-c",
            "index.recordOffsetTable=false",
            "update-index",
            "--force-write-index",
        ],
        &[
            "-c",
            "index.skipHash=true",
            "update-index",
            "--force-write-index",
        ],
    ] {
        let mut fx = fixture();
        if fx.is_skipped() {
            return;
        }
        bytes(&mut fx, args);
    }
}

/// An index read with offset tables keeps working with them (v4 blocks
/// restart the path compression).
#[test]
fn read_offset_tables() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx.git([
        "-c",
        "index.threads=3",
        "update-index",
        "--index-version",
        "4",
    ]);
    fx.compare(["ls-files", "-s", "--debug"])
        .checks(BYTES)
        .run();
    bytes(
        &mut fx,
        &[
            "-c",
            "index.threads=2",
            "update-index",
            "--force-write-index",
        ],
    );
    bytes(
        &mut fx,
        &[
            "update-index",
            "--cacheinfo",
            &format!("100644,{OID},d/new"),
        ],
    );
}

#[test]
fn resolve_undo() {
    for args in [
        &["ls-files", "--resolve-undo"][..],
        &["ls-files", "-s", "--resolve-undo"],
        &[
            "ls-files",
            "-t",
            "--abbrev",
            "--resolve-undo",
            "top",
            "nosuch",
        ],
        &["update-index", "--unresolve", "top"],
        &["update-index", "--unresolve", "--", "top", "nosuch"],
        &["update-index", "--clear-resolve-undo"],
        &[
            "-c",
            "index.threads=2",
            "update-index",
            "--unresolve",
            "top",
        ],
        &["rm", "-q", "--cached", "top"],
    ] {
        let mut fx = fixture();
        if fx.is_skipped() {
            return;
        }
        bytes(&mut fx, args);
    }
    // Unresolving the only record leaves an empty extension.
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    fx = fx.git(["update-index", "--unresolve", "top"]);
    bytes(&mut fx, &["update-index", "--force-write-index"]);
    bytes(
        &mut fx,
        &["update-index", "--cacheinfo", &format!("100644,{OID},top")],
    );
    fx.compare(["ls-files", "--resolve-undo"])
        .checks(BYTES)
        .run();
}

/// The version of a new index, with the warnings for invalid values.
#[test]
fn new_index_version() {
    let add = ["update-index", "--add", "--cacheinfo"];
    // feature.manyFiles also turns on the untracked cache (not yet).
    let cases: &[(&[&str], Option<&str>)] = &[
        (&[], None),
        (&["-c", "index.version=4"], None),
        (&["-c", "index.version=3"], None),
        (&["-c", "index.version=1"], None),
        (&["-c", "index.version=9"], None),
        (&["-c", "index.version=bogus"], None),
        (
            &[
                "-c",
                "feature.manyFiles=true",
                "-c",
                "core.untrackedCache=false",
            ],
            None,
        ),
        (
            &[
                "-c",
                "feature.manyFiles=true",
                "-c",
                "core.untrackedCache=false",
                "-c",
                "index.version=2",
            ],
            None,
        ),
        (
            &[
                "-c",
                "feature.manyFiles=true",
                "-c",
                "core.untrackedCache=false",
                "-c",
                "index.skipHash=false",
            ],
            None,
        ),
        (&[], Some("4")),
        (&["-c", "index.version=2"], Some("4")),
        (
            &[
                "-c",
                "feature.manyFiles=true",
                "-c",
                "core.untrackedCache=false",
            ],
            Some("2"),
        ),
        (&[], Some("2bogus")),
        (&[], Some("5")),
    ];
    for (config, env) in cases {
        let fx = fixture();
        if fx.is_skipped() {
            return;
        }
        let mut fx = fx.remove(".git/index");
        let mut args: Vec<&str> = config.to_vec();
        args.extend(add);
        let info = format!("100644,{OID},a");
        args.push(&info);
        let mut c = fx.compare(&args).checks(BYTES).git_file("index");
        if let Some(v) = env {
            c = c.env("GIT_INDEX_VERSION", v);
        }
        c.run();
        // An existing index keeps its version, without warnings.
        let mut c = fx
            .compare(["update-index", "--show-index-version"])
            .checks(BYTES);
        if let Some(v) = env {
            c = c.env("GIT_INDEX_VERSION", v);
        }
        c.run();
    }
}
