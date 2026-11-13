// SPDX-License-Identifier: MIT
//! Differential tests of `check-attr` against the reference git: nested
//! `.gitattributes`, `info/attributes`, `core.attributesFile`, macros,
//! malformed lines, the index and tree sources, bare repositories and the
//! command line.

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write(
            ".gitattributes",
            "[attr]notest !test\n\" d \"\ttest=d\n e\ttest=e\n e\"\ttest=e\nf\ttest=f\n\
             a/i test=a/i\nonoff test -test\noffon -test test\nno notest\nA/e/F test=A/e/F\n\
             *.bin binary\n*.c diff=cpp text\n[attr]mym alpha -beta\nm* mym\n",
        )
        .write("a/.gitattributes", "g test=a/g\nb/g test=a/b/g\n")
        .write(
            "a/b/.gitattributes",
            "h test=a/b/h\nd/* test=a/b/d/*\nd/yes notest\ndir/ isdir\n",
        )
        .write(".git/info/attributes", "info* test=info\nf other\n")
        .write("global-attributes", "global test=global\n* g1\n")
}

fn same(fx: &mut Fixture, dir: &str, args: &[&str]) {
    let mut full = vec!["check-attr"];
    full.extend_from_slice(args);
    fx.compare(&full).in_dir(dir).checks(Check::OUTPUT).run();
}

fn same_stdin(fx: &mut Fixture, args: &[&str], input: &[u8]) {
    let mut full = vec!["check-attr"];
    full.extend_from_slice(args);
    fx.compare(&full)
        .stdin(input.to_vec())
        .checks(Check::OUTPUT)
        .run();
}

const PATHS: &[&str] = &[
    " d ",
    "e",
    "e\"",
    "f",
    "a/f",
    "a/c/f",
    "a/g",
    "a/b/g",
    "b/g",
    "a/b/h",
    "a/b/d/g",
    "onoff",
    "offon",
    "no",
    "a/b/d/no",
    "a/b/d/yes",
    "F",
    "a/E/f",
    "x.bin",
    "s/x.c",
    "mine",
    "info1",
    "global",
    "a/b/dir",
    "a/b/dir/",
    "./f",
    "a/./g",
    "a/c/../b/g",
    "a_plus/g",
    "",
    "tab\there",
];

#[test]
fn named_and_all() {
    let mut fx = fixture();
    let mut args = vec!["test", "--"];
    args.extend_from_slice(PATHS);
    same(&mut fx, "", &args);
    let mut args = vec!["-a", "--"];
    args.extend_from_slice(PATHS);
    same(&mut fx, "", &args);
    let mut args = vec![
        "text", "diff", "binary", "alpha", "beta", "mym", "isdir", "--",
    ];
    args.extend_from_slice(PATHS);
    same(&mut fx, "", &args);
    same(&mut fx, "", &["-z", "-a", "f", "x.bin"]);
    same(&mut fx, "a", &["test", "../f", "f", "i", "g", "b/g"]);
    same(&mut fx, "a/b", &["-a", "../../f", "d/no", "dir/"]);
    fx.compare([
        "-c",
        "core.ignorecase=true",
        "check-attr",
        "-a",
        "F",
        "a/G",
        "a/E/f",
        "oNoFf",
    ])
    .checks(Check::OUTPUT)
    .run();
    fx.compare([
        "-c",
        "core.quotepath=false",
        "check-attr",
        "test",
        "\u{e9}",
        "tab\there",
    ])
    .checks(Check::OUTPUT)
    .run();
}

#[test]
fn global_file() {
    let mut fx = fixture()
        .git(["config", "core.attributesFile", "global-attributes"])
        .write("home-attrs", "global test=home\n");
    same(&mut fx, "", &["-a", "global", "f"]);
    let mut fx = fx.git(["config", "core.attributesFile", "~/home-attrs"]);
    let home = fx.home_dir();
    std::fs::write(
        home.join("home-attrs"),
        "global test=home\n[attr]mym gamma\n",
    )
    .unwrap();
    same(&mut fx, "", &["-a", "global", "mine"]);
    let mut fx = fx.git(["config", "--unset", "core.attributesFile"]);
    std::fs::create_dir_all(home.join(".config/git")).unwrap();
    std::fs::write(home.join(".config/git/attributes"), "global xdg\n").unwrap();
    same(&mut fx, "", &["-a", "global"]);
}

#[test]
fn stdin() {
    let mut fx = fixture();
    let mut input = Vec::new();
    for p in PATHS {
        input.extend_from_slice(p.as_bytes());
        input.push(b'\n');
    }
    input.extend_from_slice(b"\"a/\\147\"\n\"tab\\there\"\nlast-without-newline");
    same_stdin(&mut fx, &["--stdin", "test"], &input);
    same_stdin(&mut fx, &["--stdin", "-a"], &input);
    same_stdin(&mut fx, &["--stdin", "-z", "-a"], b"f\0a/g\0x.bin\0");
    same_stdin(&mut fx, &["--stdin", "test"], b"f\n\"bad\n");
    same_stdin(&mut fx, &["--stdin", "test", "--"], b"f\n");
}

#[test]
fn macros_and_precedence() {
    let cases: &[&str] = &[
        "file binary text\n",
        "file text binary\n",
        "file text\nfile binary\n",
        "file binary\nfile -binary\n",
        "file -binary\nfile binary\n",
        "file !binary\nfile binary=x\n",
        "[attr]m1 m2\n[attr]m2 x -y\nfile m1\n",
        "file m1\n[attr]m1 x\n",
        "[attr]m1 x\nfile m1\n[attr]m1 y\n",
        "[attr]binary foo\nfile binary\n",
        "[attr]c1 c2\n[attr]c2 c1\nfile c1\n",
        "[attr]m1 -text\nfile text m1\n",
        "[attr]m1 m1x !text\nfile text\nfile m1\n",
        "[attr]m1 x\nfile -m1 m1=5\n",
        "file zeta alpha binary\n[attr]mym beta -gamma\nfile mym\n",
    ];
    for c in cases {
        let mut fx = Fixture::new()
            .rgit(env!("CARGO_BIN_EXE_rgit"))
            .git(["init", "-q"])
            .write(".gitattributes", c)
            .write("sub/.gitattributes", "[attr]m1 sub\nfile -text\n")
            .write(".git/info/attributes", "[attr]m2 frominfo\n");
        same(&mut fx, "", &["-a", "file", "sub/file"]);
        same(
            &mut fx,
            "",
            &["text", "binary", "m1", "x", "--", "file", "sub/file"],
        );
    }
}

#[test]
fn malformed_lines() {
    let long = format!("path {}\nok set\n", "0".repeat(2043));
    let fine = format!("path {}\n", "0".repeat(2042));
    let cases: Vec<String> = vec![
        "\"a test=a\n".into(),
        "file 1ok -bad2 a.b-c_d foo=bar=baz x= =y\n".into(),
        "file a $x b\nfile ok\n".into(),
        "file builtin_foo\nfile builtin_objectmode=1\n".into(),
        "!file q\n\\!file r\n".into(),
        "\u{feff}file bom\r\n# c\n\n\tfile  tab\t\r\n".into(),
        "\"fi\\154e\" q\n\"file\"r s\n\"fi\\le\" t\n\"\" u\n".into(),
        "[attr]-x a\n[attr]$y b\n[attr]z $c\n[attr]builtin_q r\n".into(),
        "file -\nfile !\nfile =\nfile -- b\n".into(),
        long,
        fine,
        "file a\nfile b".into(),
    ];
    for c in &cases {
        let mut fx = Fixture::new()
            .rgit(env!("CARGO_BIN_EXE_rgit"))
            .git(["init", "-q"])
            .write(".gitattributes", c)
            .write("sub/.gitattributes", "  [attr]m8\tp  \nfile m8\n");
        same(
            &mut fx,
            "",
            &["-a", "file", "path", "ok", "!file", "sub/file"],
        );
    }
}

#[test]
fn index_and_tree_sources() {
    let mut fx = fixture()
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .write("a/.gitattributes", "g test=changed\n")
        .write("new/.gitattributes", "* test=new\n")
        .git(["add", "a/.gitattributes"])
        .remove("a/b/.gitattributes");
    let paths = ["a/g", "a/b/h", "new/x", "f"];
    for opts in [
        &[][..],
        &["--cached"],
        &["--source", "HEAD"],
        &["--source=HEAD^{tree}"],
        &["--cached", "--source", "HEAD"],
        &["--source", "HEAD:a"],
    ] {
        let mut args = opts.to_vec();
        args.extend(["test", "--"]);
        args.extend(paths);
        same(&mut fx, "", &args);
    }
    for (pre, env) in [
        (vec!["--attr-source=HEAD"], None),
        (vec!["-c", "attr.tree=HEAD"], None),
        (vec!["-c", "attr.tree=nonexistent"], None),
        (vec!["-c", "attr.tree=HEAD:f"], None),
        (
            vec!["-c", "attr.tree=HEAD", "--attr-source=HEAD~0"],
            Some("nonexistent"),
        ),
        (vec![], Some("HEAD")),
        (vec![], Some("nonexistent")),
        (vec!["--attr-source=nonexistent"], None),
    ] {
        let mut args = pre.clone();
        args.extend(["check-attr", "-a"]);
        args.extend(paths);
        let mut cmp = fx.compare(&args).checks(Check::OUTPUT);
        if let Some(v) = env {
            cmp = cmp.env("GIT_ATTR_SOURCE", v);
        }
        cmp.run();
    }
    same(&mut fx, "", &["--source", "nonexistent", "test", "f"]);
    same(&mut fx, "", &["--source", "", "test", "f"]);
}

#[test]
fn work_tree_file_kinds() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("sub/.gitattributes", "* test=index\n")
        .git(["add", "sub/.gitattributes"])
        .remove("sub/.gitattributes")
        .write("real", "* test=link\n");
    same(&mut fx, "", &["test", "sub/x"]);
    #[cfg(unix)]
    {
        let mut fx2 = fx.symlink("../real", "sub/.gitattributes");
        same(&mut fx2, "", &["test", "sub/x"]);
        let mut fx3 =
            fx2.symlink("real", ".gitattributes")
                .git(["config", "core.attributesFile", "real"]);
        same(&mut fx3, "", &["test", "x", "sub/x"]);
    }
}

#[test]
fn bare_repository() {
    let mut fx = fixture()
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .git(["clone", "-q", "--bare", ".", "bare.git"])
        .write("bare.git/.gitattributes", "f test=bare\n")
        .write("bare.git/info/attributes", "a/i test=info\n");
    for args in [
        &["test", "f", "a/i", "a/g"][..],
        &["--source", "HEAD", "test", "f", "a/i", "a/g"],
        &["--cached", "-a", "f", "a/g"],
    ] {
        same(&mut fx, "bare.git", args);
    }
    fx.compare(["-c", "attr.tree=HEAD", "check-attr", "test", "a/g"])
        .in_dir("bare.git")
        .checks(Check::OUTPUT)
        .run();
    same(&mut fx, ".git", &["test", "f"]);
}

#[test]
fn object_mode() {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("normal", "")
        .write_executable("exec", "")
        .write("dir/file", "")
        .write("sub/x", "")
        .git(["-C", "sub", "init", "-q"])
        .write("unborn/x", "")
        .git(["-C", "unborn", "init", "-q"])
        .git(["-C", "sub", "add", "x"])
        .git(["-C", "sub", "commit", "-q", "-m", "x"])
        .write(".gitattributes", "* builtin_objectmode=1 test\n");
    #[cfg(unix)]
    let fx = fx.symlink("normal", "link");
    let mut fx = fx;
    same(
        &mut fx,
        "",
        &[
            "builtin_objectmode",
            "test",
            "--",
            "normal",
            "exec",
            "dir",
            "dir/",
            "sub",
            "unborn",
        ],
    );
    #[cfg(unix)]
    same(&mut fx, "", &["builtin_objectmode", "link"]);
    same(&mut fx, "", &["builtin_objectmode", "missing"]);
    same(&mut fx, "", &["builtin_objectmode", "./missing"]);
    same(&mut fx, "", &["-a", "normal", "dir"]);
    let mut fx = fx.git(["add", "normal", "exec", "dir"]);
    same(
        &mut fx,
        "",
        &[
            "--cached",
            "builtin_objectmode",
            "--",
            "normal",
            "exec",
            "dir",
            "sub",
            "missing",
        ],
    );
}

#[test]
fn command_line() {
    let mut fx = fixture();
    for args in [
        &[][..],
        &["--"],
        &["test"],
        &["test", "--"],
        &["--", "f"],
        &["--source"],
        &["", "--", "f"],
        &["te$t", "f"],
        &["-a"],
        &["-a", "--"],
        &["-a", "--", "--"],
        &["--all", "test", "--", "f"],
        &["test", "--", "f", "--", "g"],
        &["test", "f", "--", "g"],
        &["-a", "f", "g"],
        &["--no-source", "test", "f"],
        &["-z", "test", "--"],
        &["test", "/outside"],
        &["test", "../outside"],
        &["test", "a/../../outside"],
        &["-h"],
    ] {
        same(&mut fx, "", args);
    }
    for args in [
        &["--stdin"][..],
        &["--stdin", "--", "f"],
        &["--stdin", "test", "--", "f"],
        &["--stdin", "test", "f"],
        &["--stdin", "-a", "f"],
        &["--stdin", "--"],
        &["--all", "--stdin", "--"],
    ] {
        same_stdin(&mut fx, args, b"f\n");
    }
}
