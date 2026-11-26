// SPDX-License-Identifier: MIT
//! Replace refs and grafts (#105): `rgit replace` in all its modes, object
//! reads through `refs/replace/` and the switches that turn them off, and
//! the legacy `info/grafts` file, each compared with git.

use rgit_difftest::Fixture;

/// A linear history of five commits (`c1` .. `c5`, tags of the same
/// name) in `r`.
fn history() -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "r"]);
    if fx.is_skipped() {
        return fx;
    }
    for i in 1..=5 {
        fx = fx
            .write("r/f", format!("{i}\n"))
            .git(["-C", "r", "add", "f"])
            .tick()
            .git(["-C", "r", "commit", "-q", "-m", &format!("c{i}")])
            .git(["-C", "r", "tag", &format!("c{i}")]);
    }
    fx
}

fn rev(fx: &Fixture, name: &str) -> String {
    let out = fx.git_stdout(["-C", "r", "rev-parse", name]);
    String::from_utf8(out).unwrap().trim().to_owned()
}

#[test]
fn create_list_delete() {
    let mut fx = history();
    if fx.is_skipped() {
        return;
    }
    let c2 = rev(&fx, "c2");
    let short = &c2[..8];
    for args in [
        &["replace"][..],
        &["replace", "c2", "c4"],
        &["replace", "c2", "c3"],
        &["replace", "-f", "c2", "c3"],
        &["replace"],
        &["replace", "-l", "--format=medium"],
        &["replace", "--format=long"],
        &["replace", "--format", "bogus"],
        &["replace", "-l", "x*"],
        &["log", "--format=%s", "c3"],
        &["cat-file", "-p", "c2"],
        &["--no-replace-objects", "log", "--format=%s", "c3"],
        &["-c", "core.useReplaceRefs=false", "cat-file", "-p", "c2"],
        &["replace", "c2^{tree}", "c1"],
        &["replace", "c1", "nonexistent"],
        &["replace", "-d", short, "c1"],
        &["replace", "-d", "c2"],
        &["replace", "c2"],
        &["replace", "-l", "-d", "c2"],
        &["replace", "-f"],
        &["replace", "--raw", "c1", "c2"],
        &["replace", "--format=short", "c1", "c2"],
        &["replace", "-d"],
        &["replace", "-l", "a", "b"],
        &["replace", "-e"],
        &["replace", "-g"],
        &["replace", "--convert-graft-file", "x"],
    ] {
        fx.compare(args.iter().copied()).in_dir("r").run();
    }
    fx.compare(["log", "--format=%s", "c3"])
        .in_dir("r")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .run();
}

#[test]
fn graft_rewrites_history() {
    let mut fx = history();
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["replace", "--graft", "c4", "c1"][..],
        &["log", "--oneline"],
        &["rev-list", "--parents", "HEAD"],
        &["rev-parse", "c5~2"],
        &["replace", "--graft", "c4", "c2"],
        &["replace", "-f", "--graft", "c4", "c3"],
        &["replace", "-f", "--graft", "c4", "c2", "c1"],
        &["log", "--format=%h %p %s"],
        &["replace", "--graft", "c3"],
        &["log", "--oneline"],
        &["replace", "--graft", "c2", "c1^{tree}"],
        &["replace", "--graft", "c2", "nope"],
        &["replace", "--graft", "c1^{tree}"],
        &["fsck"],
        &["replace", "-d", "c3", "c4"],
        &["log", "--oneline"],
    ] {
        fx.compare(args.iter().copied()).in_dir("r").run();
    }
}

#[test]
fn edit() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx
        .write_executable("ed", "#!/bin/sh\nsed -e 's/c3/edited/' \"$1\" >\"$1.new\" && mv \"$1.new\" \"$1\"\n")
        .write_executable("addentry", "#!/bin/sh\nprintf '100644 blob %s\\tg\\n' $(git rev-parse c1:f) >>\"$1\"\n")
        .write_executable("garbage", "#!/bin/sh\necho garbage >\"$1\"\n");
    let dir = fx.work_dir();
    let ed = dir.join("ed");
    let addentry = dir.join("addentry");
    let garbage = dir.join("garbage");
    for (editor, args) in [
        (&ed, &["replace", "--edit", "c3"][..]),
        (&ed, &["replace", "--edit", "c3"]),
        (&ed, &["replace", "-f", "--edit", "c4"]),
        (&addentry, &["replace", "--edit", "c3^{tree}"]),
        (&garbage, &["replace", "-f", "--edit", "c2"]),
    ] {
        fx.compare(args.iter().copied())
            .in_dir("r")
            .env("GIT_EDITOR", editor)
            .run();
    }
    fx.compare(["cat-file", "-p", "c3"]).in_dir("r").run();
    fx.compare(["ls-tree", "c3"]).in_dir("r").run();
    let mut fx = fx.git(["-C", "r", "replace", "-d", "c3"]);
    fx.compare(["replace", "--edit", "c3"])
        .in_dir("r")
        .env("GIT_EDITOR", "false")
        .run();
    fx.compare(["replace", "--edit", "c3"])
        .in_dir("r")
        .env("GIT_EDITOR", "true")
        .run();
}

#[test]
fn convert_graft_file() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let (c1, c2, c4) = (rev(&fx, "c1"), rev(&fx, "c2"), rev(&fx, "c4"));
    let mut fx = fx.write(
        "r/.git/info/grafts",
        format!("{c4} {c1}\n\n# comment\n{c2}\n"),
    );
    fx.compare(["status", "--short"]).in_dir("r").run();
    fx.compare(["rev-list", "--count", "HEAD"]).in_dir("r").run();
    fx.compare(["replace", "--convert-graft-file"]).in_dir("r").run();
    fx.compare(["rev-list", "--count", "HEAD"]).in_dir("r").run();
    let mut fx = fx.write(
        "r/.git/info/grafts",
        format!("{c4} {c1}\njunk\n{c2} zz\n"),
    );
    fx.compare(["replace", "--convert-graft-file"]).in_dir("r").run();
}

#[test]
fn graft_file_changes_parents() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let (c1, c3) = (rev(&fx, "c1"), rev(&fx, "c3"));
    let mut fx = fx.write("r/.git/info/grafts", format!("{c3} {c1}\nbad line\n"));
    for args in [
        &["rev-list", "HEAD"][..],
        &["rev-list", "--parents", "HEAD"],
        &["log", "--format=%h %p %s"],
        &["rev-parse", "c3^"],
        &["--no-replace-objects", "rev-list", "--count", "HEAD"],
        &["cat-file", "-p", "c3"],
        &["-c", "advice.graftFileDeprecated=false", "show", "-s", "HEAD"],
    ] {
        fx.compare(args.iter().copied()).in_dir("r").run();
    }
    fx.compare(["rev-list", "--count", "HEAD"])
        .in_dir("r")
        .env("GIT_GRAFT_FILE", "/nonexistent")
        .run();
}

#[test]
fn replace_depth() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let c: Vec<String> = (1..=5).map(|i| rev(&fx, &format!("c{i}"))).collect();
    let fx = fx
        .git(["-C", "r", "update-ref", &format!("refs/replace/{}", c[0]), &c[1]])
        .git(["-C", "r", "update-ref", &format!("refs/replace/{}", c[1]), &c[0]]);
    let mut fx = fx;
    fx.compare(["cat-file", "-t", "c1"]).in_dir("r").run();
    fx.compare(["log", "--oneline", "-1", "c2"]).in_dir("r").run();
    fx.compare(["--no-replace-objects", "log", "--oneline", "-1", "c2"])
        .in_dir("r")
        .run();
}

#[test]
fn other_objects_and_ref_base() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let (b1, b5) = (rev(&fx, "c1:f"), rev(&fx, "c5:f"));
    let mut fx = fx.git(["-C", "r", "replace", &b5, &b1]);
    for args in [
        &["cat-file", "-p", "HEAD:f"][..],
        &["show", "HEAD:f"],
        &["checkout", "--", "f"],
        &["diff", "c4", "c5"],
        &["ls-tree", "HEAD"],
        &["replace", "-l", "--format=long"],
    ] {
        fx.compare(args.iter().copied()).in_dir("r").run();
    }
    fx.compare(["cat-file", "-p", "HEAD:f"])
        .in_dir("r")
        .env("GIT_REPLACE_REF_BASE", "refs/other/")
        .run();
}
