// SPDX-License-Identifier: MIT
//! Differential tests of `rgit rev-list` and `rgit log` against the
//! reference git.

use rgit_difftest::{Check, Fixture};

fn commit(fx: Fixture, msg: &str, tick: bool) -> Fixture {
    let fx = fx.git(["commit", "-q", "--allow-empty", "-m", msg]);
    if tick { fx.tick() } else { fx }
}

/// Branches and merges; some commits share a date (tie-breaks), one tag
/// is annotated, part of the history is packed.
fn history() -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"]);
    fx = commit(fx, "root", true);
    fx = commit(fx, "second\n\nwith a body\n\twith a tab", true);
    fx = fx.git(["branch", "side"]).git(["branch", "other"]);
    fx = commit(fx, "main one", false);
    fx = commit(fx, "main two", true);
    fx = fx.git(["checkout", "-q", "side"]);
    fx = commit(fx, "side one", false);
    fx = commit(fx, "side two", true);
    fx = fx.git(["checkout", "-q", "other"]);
    fx = commit(fx, "other one", true);
    fx = fx
        .git(["checkout", "-q", "main"])
        .git(["merge", "-q", "--no-ff", "-m", "merge side", "side"])
        .tick()
        .git(["repack", "-a", "-d", "-q"])
        .git(["merge", "-q", "--no-ff", "-m", "merge other", "other"])
        .git(["tag", "-a", "-m", "tag", "v1", "HEAD~1"])
        .git(["tag", "light", "side~1"])
        .tick();
    commit(fx, "after", true)
}

fn same(fx: &mut Fixture, args: &[&str]) {
    fx.compare(args).checks(Check::OUTPUT).run();
}

#[test]
fn rev_list_walks() {
    let mut fx = history();
    for args in [
        &["rev-list", "HEAD"][..],
        &["rev-list", "--all"],
        &["rev-list", "--branches"],
        &["rev-list", "--tags"],
        &["rev-list", "--count", "--all"],
        &["rev-list", "--first-parent", "HEAD"],
        &["rev-list", "--topo-order", "HEAD"],
        &["rev-list", "--date-order", "HEAD"],
        &["rev-list", "--reverse", "HEAD"],
        &["rev-list", "-n", "3", "HEAD"],
        &["rev-list", "-2", "--skip=1", "HEAD"],
        &[
            "rev-list",
            "--max-count=4",
            "--reverse",
            "--topo-order",
            "HEAD",
        ],
        &["rev-list", "main", "^side"],
        &["rev-list", "side..main"],
        &["rev-list", "main", "--not", "side", "other"],
        &["rev-list", "--all", "--not", "v1"],
        &["rev-list", "v1"],
        &["rev-list", "light", "other"],
        &["rev-list", "other", "light"],
        &["rev-list", "--parents", "HEAD"],
        &["rev-list", "--parents", "--first-parent", "HEAD"],
        &["rev-list", "--abbrev-commit", "--parents", "HEAD"],
        &["rev-list", "--abbrev-commit", "--abbrev=10", "HEAD"],
        &["rev-list", "--oneline", "HEAD"],
        &["rev-list", "HEAD^{tree}"],
        &["rev-list", "HEAD~1^!"],
        &["rev-list", "HEAD~1^@"],
        &["rev-list", "side^!", "--not", "main"],
        &["rev-list", "main...other"],
        &["rev-list", "side...other", "--topo-order"],
        &["rev-list", "HEAD^-1"],
    ] {
        same(&mut fx, args);
    }
}

#[test]
fn log_formats() {
    let mut fx = history();
    for args in [
        &["log"][..],
        &["log", "--oneline"],
        &["log", "--oneline", "--all"],
        &["log", "--oneline", "--no-abbrev-commit"],
        &["log", "--oneline", "--abbrev=4"],
        &["log", "--abbrev-commit", "-3"],
        &["log", "--no-abbrev", "-3"],
        &["log", "--parents", "--oneline"],
        &["log", "--parents", "-2"],
        &["log", "--pretty=short"],
        &["log", "--pretty=medium"],
        &["log", "--pretty=full"],
        &["log", "--pretty=fuller"],
        &["log", "--pretty=raw"],
        &["log", "--pretty=oneline"],
        &["log", "--format=oneline", "--abbrev-commit"],
        &["log", "--pretty"],
        &["log", "--date=iso", "-2"],
        &["log", "--reverse", "--oneline", "-n", "3"],
        &["log", "--topo-order", "--oneline"],
        &["log", "side"],
        &["log", "v1", "-1"],
    ] {
        same(&mut fx, args);
    }
}

#[test]
fn odd_messages_and_headers() {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("empty", "")
        .git(["hash-object", "-w", "-t", "tree", "empty"]);
    if fx.is_skipped() {
        return;
    }
    let tree = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    let raw = |parents: &str, extra: &str, msg: &str| {
        format!(
            "tree {tree}\n{parents}author A U Thor <a@x> 1000000000 +0130\n\
             committer C O <c@x> 1000000100 -0500\n{extra}\n{msg}"
        )
    };
    let mut fx = fx.write(
        "c1",
        raw(
            "",
            "",
            "\n\n  first  \n   second\t \n\n\nbody\ttab x\n \n\t\n\nend   \n\n\n",
        ),
    );
    let c1 = hash(&fx, "c1");
    fx = fx.write(
        "c2",
        raw(
            &format!("parent {c1}\n"),
            "x-extra foo\n continued\n",
            "no newline at the end",
        ),
    );
    let c2 = hash(&fx, "c2");
    fx = fx.write(
        "c3",
        raw(&format!("parent {c2}\nparent {c1}\n"), "", "merge\n"),
    );
    let c3 = hash(&fx, "c3");
    fx = fx.write("c4", raw(&format!("parent {c3}\n"), "", ""));
    let c4 = hash(&fx, "c4");
    fx = fx.git(["update-ref", "refs/heads/main", &c4]);
    for pretty in ["medium", "short", "full", "fuller", "raw", "oneline"] {
        same(&mut fx, &["log", &format!("--pretty={pretty}")]);
    }
    same(&mut fx, &["log", "--parents", "--oneline"]);
    same(&mut fx, &["rev-list", "--parents", "main"]);
}

fn hash(fx: &Fixture, file: &str) -> String {
    let out = fx.git_stdout(["hash-object", "-w", "-t", "commit", "--literally", file]);
    String::from_utf8(out).unwrap().trim().to_string()
}

#[test]
fn mailmap() {
    let mut fx = history()
        .write(
            ".mailmap",
            "Proper Name <author@example.com>\n<canonical@example.com> <committer@example.com>\n",
        )
        .git(["config", "mailmap.file", "extra-map"])
        .write("extra-map", "# nothing\n");
    for args in [
        &["log", "-2"][..],
        &["log", "--pretty=fuller", "-2"],
        &["log", "--no-mailmap", "-2"],
        &["log", "--pretty=raw", "-1"],
    ] {
        same(&mut fx, args);
    }
}

#[test]
fn errors() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"]);
    for args in [
        &["log"][..],
        &["log", "--all"],
        &["rev-list", "--all"],
        &["rev-list", "HEAD"],
    ] {
        same(&mut fx, args);
    }
    let mut fx = history();
    for args in [
        &["rev-list"][..],
        &["rev-list", "nosuch"],
        &["rev-list", "^nosuch", "HEAD"],
        &["log", "nosuch"],
        &["rev-list", "--frobnicate", "HEAD"],
        &["log", "--frobnicate"],
        &["rev-list", "-n", "x", "HEAD"],
        &["rev-list", "--skip=y", "HEAD"],
        &["log", "--pretty=bogus"],
        &["log", "-h"],
        &["log", "--help-all"],
        &["rev-list", "-h"],
    ] {
        same(&mut fx, args);
    }
}

#[test]
fn shallow_clone() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let src = format!("file://{}", fx.work_dir().display());
    let mut fx = fx.git(["clone", "-q", "--depth=2", "--no-local", &src, "shallow"]);
    for args in [
        &["-C", "shallow", "rev-list", "HEAD"][..],
        &["-C", "shallow", "log", "--parents", "--oneline"],
        &["-C", "shallow", "rev-list", "--count", "--all"],
    ] {
        same(&mut fx, args);
    }
}

#[test]
fn linked_worktree() {
    let mut fx = history()
        .git(["worktree", "add", "-q", "--detach", "wt", "side"])
        .git(["worktree", "add", "-q", "wt2", "-b", "topic", "other"]);
    for args in [
        &["log", "--oneline"][..],
        &["rev-list", "--all"],
        &["rev-list", "HEAD", "main"],
    ] {
        fx.compare(args).in_dir("wt").checks(Check::OUTPUT).run();
        fx.compare(args).in_dir("wt2").checks(Check::OUTPUT).run();
    }
    same(&mut fx, &["rev-list", "--all"]);
}

/// History with files, an annotated tag on a commit and one on a tree, for
/// `--objects`.
fn history_with_files() -> Fixture {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a", "a\n")
        .write("d/b", "b\n")
        .write("d/e/c", "c\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .tick()
        .git(["branch", "side"])
        .write("a", "a2\n")
        .write("d/x", "x\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "two"])
        .tick()
        .git(["rm", "-q", "d/b"])
        .git(["commit", "-q", "-m", "three"])
        .tick()
        .write("d/b", "b\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "four"])
        .tick()
        .git(["tag", "-a", "-m", "t1", "t1", "HEAD~2"])
        .git(["tag", "-a", "-m", "tree", "treetag", "HEAD^{tree}"])
        .git(["checkout", "-q", "side"])
        .write("s", "s\n")
        .git(["add", "s"])
        .git(["commit", "-q", "-m", "side"])
        .tick()
        .git(["checkout", "-q", "main"])
        .git(["merge", "-q", "--no-ff", "-m", "merge", "side"])
        .tick();
    commit(fx, "after", true)
}

#[test]
fn rev_list_orderings_and_limiting() {
    let mut fx = history();
    for args in [
        &["rev-list", "--author-date-order", "--all"][..],
        &["rev-list", "--reverse", "--date-order", "--all"],
        &["rev-list", "--merges", "HEAD"],
        &["rev-list", "--no-merges", "--count", "HEAD"],
        &["rev-list", "--max-parents=0", "--all"],
        &["rev-list", "--min-parents=2", "--no-min-parents", "HEAD"],
        &["rev-list", "--since=1112912173", "--all"],
        &["rev-list", "--until=1112912173", "--all"],
        &[
            "rev-list",
            "--max-age=1112912113",
            "--min-age=1112912233",
            "--all",
        ],
        &["rev-list", "--grep=side", "--all"],
        &["rev-list", "--grep=SIDE", "-i", "--all"],
        &[
            "rev-list",
            "--grep=one",
            "--grep=main",
            "--all-match",
            "--all",
        ],
        &["rev-list", "--grep=one", "--invert-grep", "--all"],
        &["rev-list", "-E", "--grep=^(side|other) ", "--all"],
        &["rev-list", "-F", "--grep=a tab", "--all"],
        &["rev-list", "--author=Thor", "--committer=nobody", "--all"],
        &["rev-list", "--ancestry-path", "side~1..HEAD"],
        &["rev-list", "--ancestry-path=side", "HEAD~3..HEAD"],
        &["rev-list", "--ancestry-path", "HEAD"],
        &["rev-list", "--no-walk", "side", "other", "main"],
        &["rev-list", "--no-walk=unsorted", "side", "other", "main"],
        &["rev-list", "--no-walk", "--do-walk", "side"],
        &["rev-list", "--no-walk=bogus", "side"],
        &["rev-list", "--timestamp", "-3", "HEAD"],
        &["rev-list", "--children", "HEAD"],
        &["rev-list", "--parents", "--children", "HEAD"],
        &["rev-list", "--header", "-2", "HEAD"],
        &["rev-list", "--quiet", "HEAD"],
        &["rev-list", "--pretty=short", "-2", "HEAD"],
        &["log", "--merges", "--oneline"],
        &["log", "--author=Thor", "--since=1112912173", "--oneline"],
    ] {
        same(&mut fx, args);
    }
}

#[test]
fn rev_list_boundary_and_sides() {
    let mut fx = history();
    for args in [
        &["rev-list", "--boundary", "side..main"][..],
        &["rev-list", "--boundary", "-n", "3", "HEAD"],
        &["rev-list", "--boundary", "--topo-order", "other..HEAD"],
        &["rev-list", "--boundary", "--reverse", "side..main"],
        &["rev-list", "--first-parent", "--boundary", "HEAD~3..HEAD"],
        &[
            "rev-list",
            "--boundary",
            "--parents",
            "--abbrev-commit",
            "side..main",
        ],
        &["rev-list", "--left-right", "side...other"],
        &["rev-list", "--left-right", "--boundary", "main...other"],
        &["rev-list", "--left-right", "--count", "side...other"],
        &[
            "rev-list",
            "--left-right",
            "--count",
            "--boundary",
            "side...other",
        ],
        &["rev-list", "--left-right", "main", "other"],
        &["rev-list", "--left-only", "side...other"],
        &["rev-list", "--right-only", "side...other"],
        &[
            "rev-list",
            "--left-right",
            "--oneline",
            "--boundary",
            "side...other",
        ],
        &["rev-list", "--left-right", "--pretty=raw", "side...other"],
        &[
            "rev-list",
            "--timestamp",
            "--left-right",
            "--boundary",
            "side...other",
        ],
        &[
            "rev-list",
            "--children",
            "--left-right",
            "--boundary",
            "side...other",
        ],
        &[
            "log",
            "--left-right",
            "--boundary",
            "--oneline",
            "side...other",
        ],
        &["log", "--left-right", "-2", "side...other"],
    ] {
        same(&mut fx, args);
    }
}

#[test]
fn rev_list_ref_sets_and_stdin() {
    let mut fx = history()
        .git(["branch", "foo/bar", "side"])
        .git(["branch", "foobar", "other"])
        .git(["update-ref", "refs/remotes/origin/main", "HEAD~1"]);
    for args in [
        &["rev-list", "--no-walk", "--branches=foo"][..],
        &["rev-list", "--no-walk", "--branches=foo*"],
        &["rev-list", "--no-walk", "--glob=heads/foo"],
        &["rev-list", "--no-walk", "--glob=refs/heads/*"],
        &["rev-list", "--no-walk", "--remotes=origin"],
        &["rev-list", "--no-walk", "--tags=v*"],
        &["rev-list", "--no-walk", "--exclude=foo*", "--branches"],
        &["rev-list", "--no-walk", "--exclude=refs/heads/*", "--all"],
        &["rev-list", "--exclude=HEAD", "--exclude=refs/*", "--all"],
        &["rev-list", "--branches", "--not", "--remotes"],
        &["rev-list", "--glob"],
    ] {
        same(&mut fx, args);
    }
    for (input, args) in [
        ("side\n^other\n", &["rev-list", "--stdin"][..]),
        ("--all\n--not\nside\n", &["rev-list", "--stdin"]),
        ("--glob\nrefs/heads\n", &["rev-list", "--stdin"]),
        ("--no-walk=garbage\n", &["rev-list", "--stdin"]),
        ("-dashed\n", &["rev-list", "--stdin"]),
        ("--end-of-options\nside\n", &["rev-list", "--stdin"]),
        ("side\n", &["rev-list", "main", "--not", "--stdin"]),
        ("--not\n", &["rev-list", "main", "--stdin", "side"]),
        ("side\n\n^other\n", &["rev-list", "--stdin"]),
        (
            "other\n",
            &["rev-list", "--no-walk=unsorted", "main", "--stdin", "side"],
        ),
    ] {
        fx.compare(args).stdin(input).checks(Check::OUTPUT).run();
    }
}

#[test]
fn rev_list_objects() {
    let mut fx = history_with_files();
    for args in [
        &["rev-list", "--objects", "--all"][..],
        &["rev-list", "--objects", "HEAD"],
        &["rev-list", "--objects", "HEAD~1..HEAD"],
        &["rev-list", "--objects", "side..main"],
        &["rev-list", "--objects-edge", "side..main"],
        &[
            "rev-list",
            "--objects-edge",
            "--skip=1",
            "-n",
            "1",
            "side..main",
        ],
        &["rev-list", "--objects", "--in-commit-order", "--all"],
        &["rev-list", "--objects", "--no-object-names", "--all"],
        &["rev-list", "--objects", "t1", "treetag"],
        &["rev-list", "--objects", "HEAD:d", "HEAD:a", "HEAD^{tree}"],
        &["rev-list", "--objects", "HEAD", "^side^{tree}"],
        &["rev-list", "--objects", "HEAD", "^t1"],
        &["rev-list", "--objects", "--boundary", "side..main"],
        &["rev-list", "--objects", "--reverse", "side..main"],
        &["rev-list", "--objects", "--left-right", "side...main"],
        &["rev-list", "--count", "--objects", "side..main"],
        &[
            "rev-list",
            "--count",
            "--left-right",
            "--objects",
            "side...main",
        ],
        &["rev-list", "--objects", "--timestamp", "-1", "HEAD"],
    ] {
        same(&mut fx, args);
    }
}
