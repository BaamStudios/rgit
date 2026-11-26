// SPDX-License-Identifier: MIT
//! Differential tests of the newer inspection plumbing (#143): `url-parse`,
//! `repo info`, `repo structure`, `last-modified`, `diff-pairs` and
//! `format-rev` against the reference git.

use rgit_difftest::{Check, Fixture};

#[test]
fn url_parse() {
    let mut fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"));
    if fx.is_skipped() {
        return;
    }
    let urls = [
        "ssh://user@example.com:1234/~user/repo?q#f",
        "HTTPS://u:p%41@Ex.COM/a/./b/../c d",
        "user@[myhost:123]:src",
        "[::1]:~user/repo",
        "host:a%3fb",
        "h:/p@q",
        "https://u%3fx@h/",
        "file://",
    ];
    for c in ["scheme", "user", "password", "host", "port", "path"] {
        let mut args = vec!["url-parse".to_owned(), "-c".into(), c.into()];
        args.extend(urls.iter().map(|u| u.to_string()));
        fx.compare(&args).checks(Check::OUTPUT).run();
    }
    for bad in [
        "not a url",
        "/abs/path",
        "https://",
        "https://h:0/",
        "https://h/../x",
        "9x://h/",
        "a b:c",
    ] {
        fx.compare(["url-parse", bad]).checks(Check::OUTPUT).run();
    }
    fx.compare(["url-parse", "-c", "bogus", "x:y"])
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn repo_info_and_structure() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a", "a\n")
        .write("sub/b", "b\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .git(["tag", "-a", "-m", "t", "v1"])
        .git(["update-ref", "refs/remotes/o/main", "HEAD"])
        .write("sub/b", "bb\n")
        .git(["commit", "-q", "-a", "-m", "two"]);
    if fx.is_skipped() {
        return;
    }
    fx.compare(["repo", "info", "--all"])
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["repo", "info", "-z", "layout.bare", "nope", "object.format"])
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["repo", "info", "--keys", "--format=nul"])
        .checks(Check::OUTPUT)
        .run();
    fx.compare([
        "repo",
        "info",
        "path.gitdir.relative",
        "path.commondir.absolute",
    ])
    .in_dir("sub")
    .checks(Check::OUTPUT)
    .run();
    for f in ["--format=lines", "--format=table"] {
        fx.compare(["repo", "structure", f])
            .checks(Check::OUTPUT)
            .run();
    }
    fx.compare(["repo", "info", "--keys", "--all"])
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn last_modified_and_format_rev() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("file", "1\n")
        .write("a/file", "2\n")
        .write("z", "z\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .tick()
        .write("a/b/file", "3\n")
        .write("z", "zz\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "two"])
        .tick()
        .write("file", "4\n")
        .git(["commit", "-q", "-a", "-m", "three"]);
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["last-modified"][..],
        &["last-modified", "-r", "-t"],
        &["last-modified", "--max-depth=1"],
        &["last-modified", "-r", "a"],
        &["last-modified", "-1"],
        &["last-modified", "HEAD~1..HEAD", "-z"],
        &["last-modified", "HEAD", "HEAD~1"],
        &["last-modified", "HEAD^{tree}"],
        &["last-modified", "--foo"],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
    let input = "HEAD\nHEAD~2\nnot-valid\nHEAD^{tree}\n";
    for f in ["%s", "reference", "tformat:%h %an", "format:%s", "medium"] {
        fx.compare(["format-rev", "--stdin-mode=revs", &format!("--format={f}")])
            .stdin(input)
            .checks(Check::OUTPUT)
            .run();
    }
    let head = String::from_utf8(fx.git_stdout(["rev-parse", "HEAD", "HEAD^{tree}"])).unwrap();
    fx.compare(["format-rev", "--stdin-mode=text", "--format=%s", "-z"])
        .stdin(head.replace('\n', "\0"))
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["format-rev", "--format=%s"])
        .checks(Check::OUTPUT)
        .run();
    let raw = fx.git_stdout(["diff-tree", "-r", "-z", "HEAD~2", "HEAD"]);
    for f in ["--raw", "-p", "--stat", "--patch-with-raw"] {
        fx.compare(["diff-pairs", "-z", f])
            .stdin(raw.clone())
            .checks(Check::OUTPUT)
            .run();
    }
    fx.compare(["diff-pairs"])
        .stdin(raw)
        .checks(Check::OUTPUT)
        .run();
}
