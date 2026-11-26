// SPDX-License-Identifier: MIT
//! Differential tests of `rgit commit-graph write|verify` against the
//! reference git: the written files must be byte-identical (the chain file
//! names every layer by its checksum, so equal chains mean equal layers),
//! messages and exit codes the same. Also walks with and without the graph
//! and path-limited logs with changed-path filters.

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"))
}

const GRAPH: &str = "objects/info/commit-graph";
const CHAIN: &str = "objects/info/commit-graphs/commit-graph-chain";

/// Compare `args` including the graph files.
#[track_caller]
fn same_graph(fx: &mut Fixture, args: &[&str]) {
    fx.compare(args)
        .checks([
            Check::ExitCode,
            Check::Stdout,
            Check::Stderr,
            Check::GitFiles,
        ])
        .git_file(GRAPH)
        .git_file(CHAIN)
        .run();
}

/// A commit with the given committer date, made with `hash-object`.
fn commit_at(fx: Fixture, name: &str, parents: &[&str], date: &str) -> Fixture {
    if fx.is_skipped() {
        return fx;
    }
    let tree = String::from_utf8(fx.git_stdout(["write-tree"])).unwrap();
    let mut text = format!("tree {}\n", tree.trim());
    for p in parents {
        let id = String::from_utf8(fx.git_stdout(["rev-parse", p])).unwrap();
        text.push_str(&format!("parent {}\n", id.trim()));
    }
    text.push_str(&format!(
        "author A <a@example.com> {date} +0000\ncommitter C <c@example.com> {date} +0000\n\n{name}\n"
    ));
    let fx = fx.write(".msg", text);
    let id =
        String::from_utf8(fx.git_stdout(["hash-object", "-t", "commit", "-w", ".msg"])).unwrap();
    fx.remove(".msg")
        .git(["update-ref", &format!("refs/heads/{name}"), id.trim()])
}

/// Merges, an octopus merge, and commit dates at 0, beyond 31 bits and
/// beyond 32 bits (overflowing corrected-date offsets).
fn history() -> Fixture {
    let mut fx = fixture().git(["init", "-q"]);
    for (i, path) in ["a/x", "a/b/y", "z", "a/b/c/w"].iter().enumerate() {
        fx = fx
            .write(path, format!("{i}\n"))
            .git(["add", "."])
            .git(["commit", "-q", "-m", &format!("c{i}")])
            .tick();
    }
    fx = fx
        .git(["checkout", "-q", "-b", "s1", "HEAD~2"])
        .write("s1", "1\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "s1"])
        .tick()
        .git(["checkout", "-q", "-b", "s2", "main~2"])
        .write("s2/q", "1\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "s2"])
        .tick()
        .git(["checkout", "-q", "main"])
        .git(["merge", "-q", "-m", "octopus", "s1", "s2"])
        .tick();
    fx = commit_at(fx, "zero", &["main"], "0");
    fx = commit_at(fx, "future", &["zero"], "4147483646");
    commit_at(fx, "after", &["future"], "1112354055")
}

#[test]
fn write_is_byte_identical() {
    let mut fx = history();
    same_graph(&mut fx, &["commit-graph", "write", "--reachable"]);
    same_graph(&mut fx, &["commit-graph", "verify"]);
    same_graph(
        &mut fx,
        &["commit-graph", "write", "--reachable", "--changed-paths"],
    );
    same_graph(
        &mut fx,
        &[
            "-c",
            "commitGraph.generationVersion=1",
            "commit-graph",
            "write",
            "--reachable",
        ],
    );
    same_graph(
        &mut fx,
        &[
            "-c",
            "commitGraph.changedPathsVersion=2",
            "commit-graph",
            "write",
            "--reachable",
            "--changed-paths",
            "--max-new-filters=3",
        ],
    );
    fx = fx.git(["repack", "-adq"]);
    same_graph(&mut fx, &["commit-graph", "write"]);
}

#[test]
fn stdin_sources_and_append() {
    let mut fx = history();
    let s1 = String::from_utf8(fx.git_stdout(["rev-parse", "s1"])).unwrap();
    let tag = fx.git_stdout(["rev-parse", "HEAD^{tree}"]);
    fx.compare(["commit-graph", "write", "--stdin-commits"])
        .stdin(s1.clone())
        .git_file(GRAPH)
        .run();
    let mut input = s1.into_bytes();
    input.extend_from_slice(&tag);
    fx.compare(["commit-graph", "write", "--stdin-commits", "--append"])
        .stdin(input)
        .git_file(GRAPH)
        .run();
    for bad in ["HEAD\n", "0000000000000000000000000000000000000000\n"] {
        fx.compare(["commit-graph", "write", "--stdin-commits"])
            .stdin(bad)
            .git_file(GRAPH)
            .run();
    }
    fx.compare(["commit-graph", "write", "--stdin-packs"])
        .stdin("nosuchpack.idx\n")
        .run();
}

#[test]
fn split_chains() {
    let mut fx = history();
    let write = |fx: &mut Fixture, rev: &str, extra: &[&str]| {
        let id = String::from_utf8(fx.git_stdout(["rev-parse", rev])).unwrap();
        let mut args = vec!["commit-graph", "write", "--stdin-commits"];
        args.extend_from_slice(extra);
        fx.compare(args)
            .stdin(id)
            .checks([
                Check::ExitCode,
                Check::Stdout,
                Check::Stderr,
                Check::GitFiles,
            ])
            .git_file(GRAPH)
            .git_file(CHAIN)
            .run();
    };
    write(&mut fx, "main~1", &["--split"]);
    write(&mut fx, "s1", &["--split=no-merge", "--changed-paths"]);
    write(&mut fx, "zero", &["--split=no-merge"]);
    write(&mut fx, "future", &["--split", "--size-multiple=1"]);
    write(&mut fx, "after", &["--split", "--max-commits=1"]);
    same_graph(&mut fx, &["commit-graph", "verify"]);
    same_graph(&mut fx, &["commit-graph", "verify", "--shallow"]);
    write(&mut fx, "after", &["--split=replace"]);
    same_graph(&mut fx, &["commit-graph", "write", "--reachable"]);
}

#[test]
fn split_on_top_of_a_single_file() {
    let mut fx = history();
    let id = String::from_utf8(fx.git_stdout(["rev-parse", "s2"])).unwrap();
    fx.compare(["commit-graph", "write", "--stdin-commits"])
        .stdin(id)
        .git_file(GRAPH)
        .run();
    same_graph(
        &mut fx,
        &["commit-graph", "write", "--reachable", "--split=no-merge"],
    );
}

#[test]
fn walks_use_the_graph() {
    let mut fx = history();
    same_graph(
        &mut fx,
        &["commit-graph", "write", "--reachable", "--changed-paths"],
    );
    for args in [
        &["log", "--oneline", "--all"][..],
        &["log", "--topo-order", "--format=%H %P", "main"],
        &["log", "--graph", "--oneline", "s1..after"],
        &["rev-list", "--all", "--count"],
        &["rev-parse", "after~3", "main^3"],
        &["merge-base", "--all", "s1", "s2"],
        &["log", "--format=%s", "--", "a/b"],
        &["log", "--format=%s", "--", "a/b/c/w", "s2"],
        &["log", "--format=%s", "--full-history", "--", "a"],
        &["rev-list", "--all", "--", "nothere"],
        &["log", "--format=%s", "--follow", "--", "a/x"],
    ] {
        fx.compare(args).output_only().run();
    }
}

#[test]
fn verify_reports_corruption() {
    let mut fx = history().git(["commit-graph", "write", "--reachable"]);
    if fx.is_skipped() {
        return;
    }
    let path = fx.work_dir().join(".git").join(GRAPH);
    let good = std::fs::read(&path).unwrap();
    // Positions in a SHA-1 graph with 10 commits and 6 chunks (OIDF, OIDL,
    // CDAT, GDA2, GDO2, EDGE) plus the closing row.
    let fanout = 8 + 12 * 7;
    let lookup = fanout + 1024;
    let cdat = lookup + 20 * 10;
    let cases: &[(usize, u8)] = &[
        (0, 0),
        (4, 2),
        (5, 3),
        (fanout + 16, 1),
        (lookup + 20 * 3, 1),
        (cdat, 1),
        (cdat + 20 + 3, 1),
        (cdat + 20 + 11, 0x38),
        (cdat + 20 + 12, 1),
        (good.len() - 1, 0),
    ];
    for &(at, byte) in cases {
        let mut bad = good.clone();
        bad[at] = byte;
        make_writable(&path);
        std::fs::write(&path, &bad).unwrap();
        eprintln!("corrupting byte {at} with {byte:#x}");
        fx.compare(["commit-graph", "verify"]).output_only().run();
        if at < 8 {
            // A file with a bad header is not used at all.
            fx.compare(["log", "--format=%H"]).output_only().run();
        }
    }
    std::fs::write(&path, b"a small graph\n").unwrap();
    fx.compare(["commit-graph", "verify"]).output_only().run();
}

#[test]
fn usage_and_option_errors() {
    let mut fx = history();
    for args in [
        &["commit-graph"][..],
        &["commit-graph", "unknown"],
        &["commit-graph", "write", "blah"],
        &["commit-graph", "write", "--split=bogus"],
        &["commit-graph", "write", "--reachable", "--stdin-packs"],
        &["commit-graph", "write", "--object-dir", "/nonexistent"],
        &["-c", "core.commitGraph=false", "commit-graph", "write"],
        &["commit-graph", "-h"],
        &["commit-graph", "write", "-h"],
        &["commit-graph", "verify", "-h"],
    ] {
        fx.compare(args).output_only().run();
    }
}

#[test]
fn sha256_repository() {
    let mut fx = fixture().git(["init", "-q", "--object-format=sha256"]);
    for (i, path) in ["a/x", "a/b/y", "z", "a/b/y"].iter().enumerate() {
        fx = fx
            .write(path, format!("{i}\n"))
            .git(["add", "."])
            .git(["commit", "-q", "-m", &format!("c{i}")])
            .tick();
    }
    fx = fx
        .git(["checkout", "-q", "-b", "side", "HEAD~2"])
        .write("s", "1\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "side"])
        .tick()
        .git(["checkout", "-q", "main"])
        .git(["merge", "-q", "-m", "merge", "side"]);
    same_graph(
        &mut fx,
        &["commit-graph", "write", "--reachable", "--changed-paths"],
    );
    same_graph(&mut fx, &["commit-graph", "verify"]);
    for args in [
        &["log", "--oneline"][..],
        &["log", "--format=%s", "--", "a/b"],
        &["rev-parse", "main~2", "main^2"],
    ] {
        fx.compare(args).output_only().run();
    }
    fx = fx
        .write("n", "n\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "n"]);
    same_graph(
        &mut fx,
        &["commit-graph", "write", "--reachable", "--split=no-merge"],
    );
    same_graph(&mut fx, &["commit-graph", "verify"]);
}

/// The graph file is written read-only; the test overwrites it.
fn make_writable(path: &std::path::Path) {
    #[cfg(unix)]
    let perms = std::os::unix::fs::PermissionsExt::from_mode(0o644);
    #[cfg(not(unix))]
    let perms = {
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        perms
    };
    std::fs::set_permissions(path, perms).unwrap();
}
