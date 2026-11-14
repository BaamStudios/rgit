// SPDX-License-Identifier: MIT
//! Differential tests of `rgit apply` against the reference git: output,
//! exit code, the resulting index and work tree.

use rgit_difftest::{Check, Fixture};

const STATE: [Check; 5] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Index,
    Check::Worktree,
];

fn lines(range: std::ops::RangeInclusive<u32>) -> String {
    range.map(|i| format!("{i}\n")).collect()
}

/// A repository with a few committed files of different kinds.
fn base() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("f", lines(1..=20))
        .write("g", "a\nb\n")
        .write_executable("x", "x\n")
        .write("sub/dir/file", "1\n")
        .write("sub/other", "o\n")
        .write("nonl", "a\nb")
        .write("empty", "")
        .symlink("f", "l")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "init"])
        .tick()
}

/// Runs `apply <args> p.diff` on a fresh fixture holding `patch`.
fn case(patch: &str, args: &[&str]) {
    case_with(patch, args, |fx| fx, "");
}

/// `git apply`, with stat checks that survive the copy of the fixture
/// (`--index` compares stat data).
const APPLY: [&str; 5] = [
    "-c",
    "core.checkStat=minimal",
    "-c",
    "core.trustCtime=false",
    "apply",
];

fn case_with(patch: &str, args: &[&str], setup: impl FnOnce(Fixture) -> Fixture, dir: &str) {
    let mut fx = setup(base().write("p.diff", patch));
    let mut argv = APPLY.to_vec();
    argv.extend_from_slice(args);
    let patch_path = if dir.is_empty() {
        "p.diff".to_string()
    } else {
        let depth = dir.split('/').filter(|c| !c.is_empty()).count();
        format!("{}p.diff", "../".repeat(depth))
    };
    argv.push(&patch_path);
    fx.compare(&argv).in_dir(dir).checks(STATE).run();
}

/// A patch the reference git makes from the changes `change` applies to
/// the base repository (`git diff <diff_args>`), which are then undone.
fn generated(change: impl FnOnce(Fixture) -> Fixture, diff_args: &[&str]) -> String {
    let fx = change(base());
    let mut args = vec!["diff"];
    args.extend_from_slice(diff_args);
    String::from_utf8(fx.git_stdout(&args)).unwrap()
}

const MODIFY: &str = "diff --git a/f b/f\nindex 0000000..1111111 100644\n--- a/f\n+++ b/f\n@@ -2,3 +2,4 @@\n 2\n 3\n+X\n 4\n";
const CREATE: &str = "diff --git a/n b/n\nnew file mode 100644\nindex 0000000..1111111\n--- /dev/null\n+++ b/n\n@@ -0,0 +1,2 @@\n+n1\n+n2\n";
const DELETE: &str = "diff --git a/g b/g\ndeleted file mode 100644\nindex 1111111..0000000\n--- a/g\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-a\n-b\n";
const RENAME: &str = "diff --git a/g b/h\nsimilarity index 50%\nrename from g\nrename to h\n--- a/g\n+++ b/h\n@@ -1,2 +1,2 @@\n a\n-b\n+c\n";
const COPY: &str = "diff --git a/g b/sub/g2\nsimilarity index 50%\ncopy from g\ncopy to sub/g2\n--- a/g\n+++ b/sub/g2\n@@ -1,2 +1,3 @@\n a\n b\n+c\n";
const MODE: &str = "diff --git a/g b/g\nold mode 100644\nnew mode 100755\n";

#[test]
fn modes_of_application() {
    for patch in [MODIFY, CREATE, DELETE, RENAME, COPY, MODE] {
        for args in [
            &[][..],
            &["--index"],
            &["--cached"],
            &["--check"],
            &["--index", "--check"],
            &["-v"],
            &["--index", "-v"],
            &["-R"],
            &["-R", "--index"],
        ] {
            case(patch, args);
        }
    }
}

#[test]
fn stat_numstat_summary() {
    let all = [MODIFY, CREATE, DELETE, RENAME, COPY, MODE].concat();
    for args in [
        &["--stat"][..],
        &["--numstat"],
        &["--summary"],
        &["--numstat", "-z"],
        &["--stat", "--summary", "--numstat"],
        &["--stat", "-R", "--summary"],
        &["--stat", "--apply"],
        &["--stat", "--summary", "--apply", "-v"],
        &["--check", "--stat"],
        &["--recount", "--stat"],
    ] {
        case(&all, args);
    }
    // Scaled graph, long and quoted names.
    let wide = generated(
        |fx| {
            fx.write("f", "x\n".repeat(150))
                .write(
                    "very/long/directory/name/that/goes/on/and/on/forever/file-with-a-long-name.txt",
                    "a\n",
                )
                .write("n\u{e9}w\tname", "q\n")
                .git(["add", "-A"])
        },
        &["--cached"],
    );
    for args in [
        &["--stat"][..],
        &["--numstat"],
        &["--summary"],
        &["--numstat", "-z"],
    ] {
        case(&wide, args);
    }
}

#[test]
fn generated_patches() {
    let renamed = generated(
        |fx| {
            fx.git(["mv", "g", "sub/g"])
                .write("sub/g", "a\nb\nc\n")
                .git(["mv", "x", "y"])
                .git(["add", "-A"])
        },
        &["--cached", "-M"],
    );
    let modified = generated(
        |fx| {
            fx.write("f", lines(1..=5) + "five\n" + &lines(7..=18) + "end\n")
                .write("nonl", "a\nb\nc")
                .write("empty", "now\n")
                .remove("g")
        },
        &[],
    );
    let symlink = generated(|fx| fx.remove("l").symlink("g", "l"), &[]);
    let typechange = generated(
        |fx| fx.remove("l").write("l", "file now\n").git(["add", "-A"]),
        &["--cached"],
    );
    for patch in [&renamed, &modified, &symlink, &typechange] {
        for args in [
            &[][..],
            &["--index"],
            &["--cached"],
            &["--stat", "--summary"],
        ] {
            case(patch, args);
        }
        // Reverse application on top of the result.
        let mut fx = base().write("p.diff", patch.as_str());
        fx.compare([&APPLY[..], &["--index", "p.diff"]].concat())
            .checks(STATE)
            .run();
        fx.compare([&APPLY[..], &["-R", "--index", "p.diff"]].concat())
            .checks(STATE)
            .run();
    }
}

#[test]
fn hunk_matching() {
    let f = |hdr: &str, body: &str| format!("--- a/f\n+++ b/f\n@@ {hdr} @@\n{body}");
    let cases: &[(String, &[&str])] = &[
        // Offsets.
        (f("-5,3 +5,3", " 1\n-2\n+X\n 3\n"), &["-v"]),
        (f("-1,3 +1,3", " 10\n-11\n+X\n 12\n"), &["-v"]),
        (f("-15,3 +15,3", " 10\n-11\n+X\n 12\n"), &["-v"]),
        // Anchoring at the start and the end.
        (f("-1,2 +1,3", " 1\n 2\n+X\n"), &["-v"]),
        (f("-19,2 +19,3", " 19\n 20\n+X\n"), &["-v"]),
        (f("-0,0 +1 @@", "+X\n"), &[]),
        (f("-0,0 +1", "+X\n"), &["--unidiff-zero"]),
        (f("-3,0 +4", "+X\n"), &["--unidiff-zero"]),
        (f("-3,0 +4", "+X\n"), &["-v"]),
        (f("-2 +2", "-2\n+X\n"), &["--unidiff-zero"]),
        (f("-2 +2", "-2\n+X\n"), &[]),
        // Fuzz.
        (
            f("-7,7 +7,7", " Q\n 8\n 9\n-10\n+X\n 11\n 12\n 13\n"),
            &["-v"],
        ),
        (
            f("-7,7 +7,7", " Q\n 8\n 9\n-10\n+X\n 11\n 12\n 13\n"),
            &["-v", "-C2"],
        ),
        (
            f("-7,7 +7,7", " 7\n Q\n 9\n-10\n+X\n 11\n 12\n 13\n"),
            &["-v", "-C1"],
        ),
        (
            f("-7,7 +7,7", " 7\n Q\n 9\n-10\n+X\n 11\n 12\n 13\n"),
            &["-v", "-C2"],
        ),
        (
            f("-8,6 +8,6", " 8\n 9\n-10\n+X\n 11\n 12\n Q\n"),
            &["-v", "-C0"],
        ),
        (f("-7,5 +7,5", " 7\n 8\n Q\n-10\n+X\n 11\n"), &["-C0", "-v"]),
        (f("-1,5 +1,5", " 1\n Q\n-3\n+X\n 4\n 5\n"), &["-C0", "-v"]),
        // Failures, with and without --reject.
        (f("-2,3 +2,3", " 2\n-Q\n+X\n 4\n"), &[]),
        (f("-2,3 +2,3", " 2\n-Q\n+X\n 4\n"), &["-v"]),
        (f("-2,3 +2,3", " 2\n-Q\n+X\n 4\n"), &["-q"]),
        (f("-2,3 +2,3", " 2\n-Q\n+X\n 4\n"), &["--index", "-v"]),
        (
            f(
                "-2,3 +2,3",
                " 2\n-3\n+X\n 4\n@@ -10,3 +10,3 @@\n 10\n-Q\n+Y\n 12\n",
            ),
            &["--reject"],
        ),
        (
            f(
                "-2,3 +2,3",
                " 2\n-3\n+X\n 4\n@@ -10,3 +10,3 @@\n 10\n-Q\n+Y\n 12\n",
            ),
            &["--reject", "-q"],
        ),
        (
            f(
                "-2,3 +2,3",
                " 2\n-3\n+X\n 4\n@@ -10,3 +10,3 @@\n 10\n-Q\n+Y\n 12\n",
            ),
            &["--reject", "--check"],
        ),
        (
            f(
                "-2,3 +2,3",
                " 2\n-3\n+X\n 4\n@@ -10,3 +10,3 @@\n 10\n-Q\n+Y\n 12\n",
            ),
            &["--reject", "--index"],
        ),
        // Overlapping hunks.
        (
            f(
                "-2,3 +2,3",
                " 2\n-3\n+X\n 4\n@@ -2,3 +2,3 @@\n 2\n-X\n+Y\n 4\n",
            ),
            &[],
        ),
        (
            f(
                "-2,3 +2,3",
                " 2\n-3\n+X\n 4\n@@ -2,3 +2,3 @@\n 2\n-X\n+Y\n 4\n",
            ),
            &["--allow-overlap"],
        ),
        // --no-add.
        (f("-2,3 +2,3", " 2\n-3\n+X\n 4\n"), &["--no-add"]),
        // Recount.
        (f("-2,9 +2,9", " 2\n-3\n+X\n 4\n"), &["--recount"]),
        (
            f("-2,9 +2,9", " 2\n-3\n+X\n GARBAGE\nmore\n"),
            &["--recount"],
        ),
    ];
    for (patch, args) in cases {
        case(patch, args);
    }
}

#[test]
fn incomplete_lines() {
    let cases = [
        "--- a/nonl\n+++ b/nonl\n@@ -1,2 +1,2 @@\n a\n-b\n\\ No newline at end of file\n+b\n",
        "--- a/nonl\n+++ b/nonl\n@@ -1,2 +1,3 @@\n a\n-b\n\\ No newline at end of file\n+b\n+c\n\\ No newline at end of file\n",
        "--- a/g\n+++ b/g\n@@ -1,2 +1,2 @@\n a\n-b\n+b\n\\ No newline at end of file\n",
        "--- a/g\n+++ b/g\n@@ -1,2 +1,2 @@\n a\n-b\n\\ No newline at end of file\n+c\n",
    ];
    for p in cases {
        case(p, &[]);
    }
}

#[test]
fn parse_errors() {
    let cases = [
        "diff -u a/f b/f\n--- a/f\n+++ b/f\n@@ -98765432109876543210 +98765432109876543210 @@\n-a\n+b\n",
        "@@ -1 +1 @@\n-a\n+b\n",
        "diff --git a/f b/f\nindex 7898192..6178079 100644\n--- a/f\n@@ -1 +1 @@\n-a\n+b\n",
        "diff --git a/f b/f\nindex 7898192..6178079 100644\n+++ b/f\n@@ -1 +0,0 @@\n-1\n",
        "diff --git a/f b/f\nold mode 10x644\n",
        "diff --git a/f b/f\nold mode 100644\nnew mode \n",
        "diff --git a/f b/f\n--- a/f\n+++ b/f\nthis is garbage\n",
        "diff --git a/f b/f\nnew file mode 100644\nindex 0000000..d00491f\n--- /dev/null\n+++ b/f-blah\n@@ -0,0 +1 @@\n+1\n",
        "diff --git a/f b/f\ndeleted file mode 100644\nindex d00491f..0000000\n--- b/f-blah\n+++ /dev/null\n@@ -1 +0,0 @@\n-1\n",
        "diff --git a/f b/f\nnew file mode 100644\n--- a/f\n+++ b/f\n",
        "--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n-1\nXX\n",
        "--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n 1\n",
        "--- /dev/null\n+++ b/q\n@@ -1 +1 @@\n-a\n+b\n",
        "--- a/g\n+++ /dev/null\n@@ -1 +1 @@\n-a\n+b\n",
        "I am not a patch\n",
        "",
        "diff --git a/x y/f b/x y/g\nold mode 100644\nnew mode 100755\n",
        "diff --git a/1 b/2\nnew file mode 100644\ncopy from 1\ncopy to 2\n",
    ];
    for p in cases {
        case(p, &[]);
        case(p, &["--check"]);
        case(p, &["--stat"]);
    }
    case("I am not a patch\n", &["--allow-empty"]);
    case_with("", &["-"], |fx| fx, "");
}

#[test]
fn traditional_patches() {
    let t = |a: &str, b: &str, body: &str| format!("--- {a}\n+++ {b}\n{body}");
    let mod_g = "@@ -1,2 +1,2 @@\n a\n-b\n+c\n";
    let add = "@@ -0,0 +1 @@\n+something\n";
    let cases: Vec<(String, Vec<&str>)> = vec![
        (t("a/g", "b/g", mod_g), vec![]),
        (t("g.orig", "g", mod_g), vec![]),
        (t("g", "g.orig", mod_g), vec![]),
        (
            t(
                "a/g\t2010-01-01 10:00:00.000 +0100",
                "b/g\t2010-01-01",
                mod_g,
            ),
            vec![],
        ),
        (
            t(
                "a/g 2010-01-01 10:00:00.000 +0100",
                "b/g 2010-01-01 10:00:00 -08:00",
                mod_g,
            ),
            vec![],
        ),
        (t("x/a/g", "x/a/g", mod_g), vec![]),
        (t("x/a/g", "x/a/g", mod_g), vec!["-p2"]),
        (t("x/a/g", "x/a/g", mod_g), vec!["-p3"]),
        (t("g", "g", mod_g), vec!["-p0"]),
        (t("g", "g", mod_g), vec!["-p1"]),
        (t("\"a/\"g", "\"b/\"g", mod_g), vec![]),
        // Creation and deletion by name or epoch timestamp.
        (t("/dev/null", "b/new", add), vec!["--index"]),
        (
            t(
                "a/new\t1970-01-01 00:00:00.000000000 +0000",
                "b/new\t2010-01-01 10:00:00.000 +0100",
                add,
            ),
            vec!["--index"],
        ),
        (
            t(
                "a/new\t1969-12-31 16:00:00.000000000 -0800",
                "b/new\t1982-09-16 07:00:00.000000000 -0800",
                add,
            ),
            vec![],
        ),
        (
            t(
                "a/empty\t1982-09-16 07:00:00 -0800",
                "b/empty\t1982-09-16 07:00:00 -0800",
                add,
            ),
            vec![],
        ),
        (
            t(
                "a/new\t1982-09-16 07:00:00 -0800",
                "b/new\t1982-09-16 07:00:00 -0800",
                add,
            ),
            vec![],
        ),
        (
            t(
                "a/g\t1982-09-16 07:00:00 -0800",
                "b/g\t1970-01-01 09:00:00 +0900",
                "@@ -1,2 +0,0 @@\n-a\n-b\n",
            ),
            vec!["--index"],
        ),
        (
            t(
                "a/g\t1982-09-16 07:00:00 -0800",
                "b/g\t1982-09-16 07:00:00 -0800",
                "@@ -1,2 +0,0 @@\n-a\n-b\n",
            ),
            vec!["--index"],
        ),
        (t("a/empty", "b/empty", add), vec!["--index"]),
        (t("a/f", "b/f", "@@ -1 +1 @@\n-a\n+b\n"), vec![]),
        (
            t("a/a/b/c", "a/a/b/c", "@@ -1 +1 @@\n-a\n+b\n"),
            vec!["-p5"],
        ),
    ];
    for (p, args) in &cases {
        case(p, args);
    }
}

#[test]
fn directory_include_exclude() {
    let p = "diff a/bla/blub/dir/file b/bla/blub/dir/file\n--- a/bla/blub/dir/file\n+++ b/bla/blub/dir/file\n@@ -1,1 +1,1 @@\n-1\n+Bello\n";
    for d in [
        "--directory=sub",
        "--directory=sub/",
        "--directory=./sub",
        "--directory=s//../sub",
        "--directory=../foo",
    ] {
        case(p, &[d, "-p3", "--index"]);
    }
    let all = [MODIFY, CREATE, DELETE, RENAME, COPY, MODE].concat();
    for args in [
        &["--include=sub/*"][..],
        &["--include=g"],
        &["--exclude=g"],
        &["--exclude=*", "--include=g"],
        &["--include=g", "--exclude=*"],
        &["--include=*", "--exclude=g"],
        &["--exclude=*"],
        &["--directory=pre/", "--numstat"],
    ] {
        let mut a = args.to_vec();
        a.push("--numstat");
        case(&all, &a);
        case(&all, args);
    }
}

#[test]
fn subdirectory() {
    let git = "diff --git a/sub/dir/file b/sub/dir/file\n--- a/sub/dir/file\n+++ b/sub/dir/file\n@@ -1 +1 @@\n-1\n+2\n";
    let other = "diff --git a/g b/g\n--- a/g\n+++ b/g\n@@ -1,2 +1,2 @@\n a\n-b\n+c\n";
    let trad = "--- a/file.orig\n+++ b/file\n@@ -1 +1,2 @@\n 1\n+2\n";
    for p in [git.to_string(), [git, other].concat(), trad.to_string()] {
        for args in [&[][..], &["-v"], &["--index"], &["--cached"], &["--stat"]] {
            case_with(&p, args, |fx| fx, "sub/dir");
        }
    }
}

#[test]
fn conflicts_with_the_tree() {
    // A new file that exists; changes to missing or untracked files.
    case(CREATE, &[]);
    case_with(CREATE, &[], |fx| fx.write("n", "x\n"), "");
    case_with(
        CREATE,
        &["--index"],
        |fx| fx.write("n", "x\n").git(["add", "n"]),
        "",
    );
    case_with(CREATE, &["--index"], |fx| fx.write("n", "x\n"), "");
    case_with(CREATE, &["--cached"], |fx| fx.write("n", "x\n"), "");
    case_with(CREATE, &[], |fx| fx.write("n/inner", "x\n"), "");
    case_with(MODIFY, &[], |fx| fx.remove("f"), "");
    case_with(MODIFY, &["--index"], |fx| fx.remove("f"), "");
    case_with(MODIFY, &["--index"], |fx| fx.write("f", lines(1..=21)), "");
    case_with(MODIFY, &["--cached"], |fx| fx.write("f", "other\n"), "");
    case_with(
        MODIFY,
        &["--index"],
        |fx| fx.git(["rm", "-q", "--cached", "f"]),
        "",
    );
    case_with(DELETE, &[], |fx| fx.write("g", "a\nb\nc\n"), "");
    // Intent-to-add entries.
    let ita = |fx: Fixture| fx.write("n", "n1\nn2\n").git(["add", "-N", "n"]);
    case_with(CREATE, &["--cached"], ita, "");
    case_with(CREATE, &["--index"], ita, "");
    case_with(CREATE, &[], ita, "");
    case_with(CREATE, &["--index"], |fx| ita(fx).remove("n"), "");
    case_with(CREATE, &["-N"], |fx| fx, "");
    case_with(&[MODIFY, CREATE].concat(), &["-N"], |fx| fx, "");
    // Mode mismatches.
    case_with(
        "diff --git a/g b/g\nindex 1111111..2222222 100755\n--- a/g\n+++ b/g\n@@ -1,2 +1,2 @@\n a\n-b\n+c\n",
        &["--index"],
        |fx| fx,
        "",
    );
    case_with(
        "diff --git a/g b/g\nnew file mode 100660\n--- /dev/null\n+++ b/g\n",
        &["--index"],
        |fx| fx.git(["rm", "-q", "g"]),
        "",
    );
    case_with(
        "diff --git a/g b/g\nold mode 100660\nnew mode 100770\n",
        &["--index"],
        |fx| fx,
        "",
    );
}

#[test]
fn several_patches_for_one_path() {
    let m = |name: &str, from: &str, to: &str| {
        format!(
            "diff --git a/{name} b/{name}\n--- a/{name}\n+++ b/{name}\n@@ -1,2 +1,2 @@\n a\n-{from}\n+{to}\n"
        )
    };
    let rename = |a: &str, b: &str| {
        format!("diff --git a/{a} b/{b}\nsimilarity index 100%\nrename from {a}\nrename to {b}\n")
    };
    let cases: Vec<String> = vec![
        [m("g", "b", "c"), m("g", "c", "d")].concat(),
        [m("g", "b", "c"), m("g", "b", "d")].concat(),
        [rename("g", "h"), m("h", "b", "c")].concat(),
        [rename("g", "h"), m("g", "b", "c")].concat(),
        [rename("g", "h"), rename("sub/other", "g"), m("g", "b", "c")].concat(),
        [rename("g", "sub/other"), rename("sub/other", "g")].concat(),
        [m("g", "b", "c"), rename("g", "h")].concat(),
        [DELETE, &m("g", "b", "c")].concat(),
        [CREATE, "diff --git a/n b/m\ncopy from n\ncopy to m\n"].concat(),
        [DELETE, "diff --git a/g b/g\nnew file mode 100755\n--- /dev/null\n+++ b/g\n@@ -0,0 +1 @@\n+new\n"].concat(),
        [DELETE, "diff --git a/g/x b/g/x\nnew file mode 100644\n--- /dev/null\n+++ b/g/x\n@@ -0,0 +1 @@\n+new\n"].concat(),
    ];
    for p in &cases {
        case(p, &[]);
        case(p, &["--index"]);
        case(p, &["-R", "--stat"]);
    }
}

#[test]
fn several_inputs() {
    let mut fx = base().write("ok.diff", CREATE).write(
        "bad.diff",
        "diff --git a/g b/g\n--- a/g\n+++ b/g\n@@ -1 +1 @@\n-zz\n+b\n",
    );
    fx.compare([&APPLY[..], &["bad.diff", "ok.diff"]].concat())
        .checks(STATE)
        .run();
    fx.compare([&APPLY[..], &["ok.diff", "bad.diff"]].concat())
        .checks(STATE)
        .run();
    let mut fx = base().write("ok.diff", CREATE);
    fx.compare([&APPLY[..], &["--index", "ok.diff", "ok.diff"]].concat())
        .checks(STATE)
        .run();
    let mut fx = base().write("ok.diff", CREATE);
    fx.compare([&APPLY[..], &["--numstat", "-", "ok.diff"]].concat())
        .stdin(MODIFY)
        .checks(STATE)
        .run();
    let mut fx = base();
    fx.compare([&APPLY[..], &["-v"]].concat())
        .stdin(MODIFY)
        .checks(STATE)
        .run();
    let mut fx = base();
    fx.compare([&APPLY[..], &["nosuch.diff"]].concat())
        .checks(STATE)
        .run();
}

#[test]
fn symbolic_links() {
    let in_link = "diff --git a/l/x b/l/x\nnew file mode 100644\n--- /dev/null\n+++ b/l/x\n@@ -0,0 +1 @@\n+x\n";
    let link_then_file = "diff --git a/d b/d\nnew file mode 120000\n--- /dev/null\n+++ b/d\n@@ -0,0 +1 @@\n+sub\n\\ No newline at end of file\ndiff --git a/d/x b/d/x\nnew file mode 100644\n--- /dev/null\n+++ b/d/x\n@@ -0,0 +1 @@\n+x\n";
    let renamed_link = "diff --git a/l b/rl\nsimilarity index 100%\nrename from l\nrename to rl\n--\ndiff --git /dev/null b/rl/create-me\nnew file mode 100644\nindex 0000000..039727e\n--- /dev/null\n+++ b/rl/create-me\n@@ -0,0 +1,1 @@\n+busted\n";
    for p in [in_link, link_then_file, renamed_link] {
        for args in [&[][..], &["--index"], &["--cached"], &["--check"]] {
            case(p, args);
        }
    }
    let link_dir = |fx: Fixture| fx.symlink("sub", "ld").git(["add", "ld"]);
    let modify_beyond =
        "diff --git a/ld/other b/ld/other\n--- a/ld/other\n+++ b/ld/other\n@@ -1 +1 @@\n-o\n+p\n";
    case_with(modify_beyond, &[], link_dir, "");
    case_with(modify_beyond, &["--cached"], link_dir, "");
    let rej = "--- a/g\n+++ b/g\n@@ -1,2 +1,2 @@\n a\n-Q\n+c\n";
    case_with(rej, &["--reject"], |fx| fx.symlink("f", "g.rej"), "");
}

#[test]
fn unsafe_paths() {
    let add = |name: &str| {
        format!(
            "diff --git a/{name} b/{name}\nnew file mode 100644\n--- /dev/null\n+++ b/{name}\n@@ -0,0 +1 @@\n+evil\n"
        )
    };
    for name in ["../foo", "x/../../foo", ".git/foo", "a/./b"] {
        for args in [
            &[][..],
            &["--check"],
            &["--index"],
            &["--unsafe-paths", "--index"],
        ] {
            case(&add(name), args);
        }
    }
    case(&add("../foo"), &["--unsafe-paths", "--directory=inner"]);
}

#[test]
fn fake_ancestor() {
    let mut fx = base().write("f", lines(1..=19) + "twenty\n");
    let patch = fx.git_stdout(["diff"]);
    fx = fx.git(["checkout", "f"]).write("p.diff", patch);
    fx.compare([&APPLY[..], &["--build-fake-ancestor=anc", "p.diff"]].concat())
        .checks(STATE)
        .run();
    let mut fx = base().write("p.diff", MODIFY);
    fx.compare([&APPLY[..], &["--build-fake-ancestor", "anc", "p.diff"]].concat())
        .checks(STATE)
        .run();
}

#[test]
fn option_errors() {
    for args in [
        &["-p", "x"][..],
        &["-p", "2q"],
        &["-p", "-1"],
        &["-Cx"],
        &["--directory=../foo"],
        &["--whitespace=bogus"],
        &["--reject", "--3way"],
        &["--ours"],
        &["-h"],
    ] {
        case(MODIFY, args);
    }
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .write("p.diff", MODIFY);
    for args in [&["--index"][..], &["--cached"], &["--stat"], &["--check"]] {
        let mut argv = vec!["apply"];
        argv.extend_from_slice(args);
        argv.push("p.diff");
        fx.compare(&argv)
            .checks([
                Check::ExitCode,
                Check::Stdout,
                Check::Stderr,
                Check::Worktree,
            ])
            .run();
    }
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .write("p.diff", MODIFY)
        .write("f", lines(1..=20));
    fx.compare([&APPLY[..], &["-v", "p.diff"]].concat())
        .checks([
            Check::ExitCode,
            Check::Stdout,
            Check::Stderr,
            Check::Worktree,
        ])
        .run();
}
