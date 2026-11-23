// SPDX-License-Identifier: MIT
//! Differential tests of diff drivers against the reference git: the
//! `diff` attribute (`-diff`, `diff`, `diff=<driver>`), hunk headers from
//! `diff.<driver>.xfuncname` / `.funcname` and the built-in drivers,
//! textconv, external diff programs, blob diffs, `--output` and
//! `cat-file --textconv` / `--filters`.

use rgit_difftest::{Check, Fixture, Normalizer};

/// Numbered filler lines so that hunks start well below the header line.
fn filler(n: usize) -> String {
    (1..=n).map(|i| format!("    step({i});\n")).collect()
}

/// One small source file per language: a header line, a body, and the
/// line `CHANGE` that the test edits.
fn samples() -> Vec<(&'static str, String)> {
    let body = filler(6);
    vec![
        (
            "a.c",
            format!("int main(void)\n{{\n{body}    CHANGE;\n}}\n"),
        ),
        (
            "b.cpp",
            format!(
                "namespace ns {{\nclass Shape : public Base\n{{\npublic:\n{body}    CHANGE;\n}};\n}}\n"
            ),
        ),
        (
            "c.py",
            format!("class Thing:\n    def run(self, x):\n{body}        CHANGE\n"),
        ),
        (
            "d.rs",
            format!(
                "impl<T> Holder<T> {{\n    pub fn take(&mut self) -> T {{\n{body}        CHANGE\n    }}\n}}\n"
            ),
        ),
        ("e.sh", format!("setup_env() {{\n{body}    CHANGE\n}}\n")),
        (
            "f.go",
            format!("func (s *Server) Serve(addr string) error {{\n{body}    CHANGE\n}}\n"),
        ),
        (
            "g.java",
            format!(
                "public class Main {{\n    public static void go(String[] args) {{\n{body}        CHANGE;\n    }}\n}}\n"
            ),
        ),
        (
            "h.md",
            format!("# Title\n\nintro\n\n## Usage\n\n{body}CHANGE\n"),
        ),
        ("i.tex", format!("\\section{{Intro}}\n{body}CHANGE\n")),
        (
            "j.css",
            format!("div.box > p {{\n{body}    color: CHANGE;\n}}\n"),
        ),
        ("k.pl", format!("sub handler {{\n{body}    CHANGE;\n}}\n")),
        (
            "l.rb",
            format!("module Outer\n  def greet(name)\n{body}    CHANGE\n  end\nend\n"),
        ),
        ("m.ini", format!("[section \"one\"]\n{body}key = CHANGE\n")),
        (
            "n.php",
            format!(
                "class Foo {{\n    public function bar() {{\n{body}        CHANGE;\n    }}\n}}\n"
            ),
        ),
        ("o.scm", format!("(define (square x)\n{body}  CHANGE)\n")),
        (
            "p.cs",
            format!(
                "class Program\n{{\n    static void Main(string[] args)\n    {{\n{body}        CHANGE;\n    }}\n}}\n"
            ),
        ),
    ]
}

const ATTRIBUTES: &str = "*.c diff=cpp\n*.cpp diff=cpp\n*.py diff=python\n*.rs diff=rust\n\
*.sh diff=bash\n*.go diff=golang\n*.java diff=java\n*.md diff=markdown\n*.tex diff=tex\n\
*.css diff=css\n*.pl diff=perl\n*.rb diff=ruby\n*.ini diff=ini\n*.php diff=php\n\
*.scm diff=scheme\n*.cs diff=csharp\n";

fn fixture() -> Fixture {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write(".gitattributes", ATTRIBUTES)
        .write("bin", b"one\n\0two\n".as_slice())
        .write("plain.txt", format!("first line\n{}", filler(8)))
        .write_executable("hex", "#!/bin/sh\ntr '\\000' 'Z' <\"$1\"\n")
        .write_executable("ext", "#!/bin/sh\necho \"$# $1 $3 $4 $6 $7 $8\"\n");
    for (name, text) in samples() {
        fx = fx.write(name, text);
    }
    let mut fx = fx
        .git(["add", "."])
        .git(["commit", "-q", "-m", "first"])
        .tick()
        .write("bin", b"one\n\0three\n".as_slice())
        .write(
            "plain.txt",
            format!(
                "first line\n{}",
                filler(8).replace("step(7)", "step(seven)")
            ),
        );
    for (name, text) in samples() {
        fx = fx.write(name, text.replace("CHANGE", "CHANGED"));
    }
    fx
}

#[test]
fn builtin_hunk_headers_match_git() {
    let mut fx = fixture();
    let mut failures = Vec::new();
    for (name, _) in samples() {
        for args in [&["diff", "-U1"][..], &["diff", "-U3"], &["diff", "-U0"]] {
            let mut argv = args.to_vec();
            argv.push(name);
            if let Err(m) = fx.compare(&argv).checks(Check::OUTPUT).try_run() {
                failures.push(format!("{argv:?}\n{}", m.report()));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn attributes_and_configured_patterns_match_git() {
    let mut fx = fixture();
    let mut failures = Vec::new();
    let cases: &[&[&str]] = &[
        &["diff"],
        &["diff", "--stat"],
        &["diff", "--numstat"],
        &["diff", "-a", "bin"],
        &["diff", "--text", "--stat", "bin"],
        &["-c", "diff.cpp.xfuncname=^(int) main", "diff", "a.c"],
        &["-c", "diff.cpp.funcname=^\\(int\\) main", "diff", "a.c"],
        &["-c", "diff.cpp.xfuncname=!^int\nstep", "diff", "a.c"],
        &["-c", "diff.cpp.xfuncname=(", "diff", "a.c"],
        &["-c", "diff.cpp.xfuncname=^int\n!main", "diff", "a.c"],
        &["-c", "diff.cpp.algorithm=patience", "diff", "a.c"],
        &["diff-files", "-p"],
        &["diff-index", "-p", "HEAD"],
        &["diff", "HEAD:a.c", "HEAD:c.py"],
        &["diff", "--stat", "HEAD:a.c", "HEAD:c.py"],
        &["diff", "-R", "HEAD:a.c", "HEAD:c.py"],
    ];
    for argv in cases {
        if let Err(m) = fx.compare(*argv).checks(Check::OUTPUT).try_run() {
            failures.push(format!("{argv:?}\n{}", m.report()));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn diff_attribute_values_match_git() {
    let mut failures = Vec::new();
    for attrs in [
        "plain.txt -diff\n",
        "bin diff\n",
        "plain.txt diff=nobody\n",
        "* binary\n",
    ] {
        let mut fx = fixture().write(".gitattributes", attrs);
        for argv in [&["diff"][..], &["diff", "--stat"], &["diff", "--numstat"]] {
            if let Err(m) = fx.compare(argv).checks(Check::OUTPUT).try_run() {
                failures.push(format!("{attrs:?} {argv:?}\n{}", m.report()));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn textconv_matches_git() {
    let mut fx = fixture().write(".gitattributes", "bin diff=hex\nplain.txt diff=up\n");
    let mut failures = Vec::new();
    let cases: &[&[&str]] = &[
        &["-c", "diff.hex.textconv=./hex", "diff"],
        &["-c", "diff.hex.textconv=./hex", "diff", "--stat"],
        &["-c", "diff.hex.textconv=./hex", "diff", "--no-textconv"],
        &[
            "-c",
            "diff.hex.textconv=./hex",
            "-c",
            "diff.hex.binary=true",
            "diff",
        ],
        &["-c", "diff.hex.textconv=./hex", "diff-files", "-p"],
        &[
            "-c",
            "diff.hex.textconv=./hex",
            "diff-files",
            "-p",
            "--textconv",
        ],
        &["-c", "diff.up.textconv=tr a-z A-Z <", "diff", "plain.txt"],
        &[
            "-c",
            "diff.up.textconv=sed s/seven/7/ <",
            "diff",
            "--exit-code",
            "plain.txt",
        ],
        &[
            "-c",
            "diff.up.textconv=sed s/seven/7/ <",
            "diff",
            "--quiet",
            "plain.txt",
        ],
        &["-c", "diff.hex.textconv=false", "diff"],
        &[
            "-c",
            "diff.hex.textconv=./hex",
            "cat-file",
            "--textconv",
            "HEAD:bin",
        ],
        &[
            "-c",
            "diff.hex.textconv=./hex",
            "cat-file",
            "--textconv",
            ":bin",
        ],
        &[
            "-c",
            "diff.hex.textconv=./hex",
            "cat-file",
            "--textconv",
            "HEAD:a.c",
        ],
        &["cat-file", "--textconv", "HEAD"],
        &["cat-file", "--textconv", "HEAD:nothere"],
        &["cat-file", "--filters", "HEAD:plain.txt"],
    ];
    for argv in cases {
        if let Err(m) = fx.compare(*argv).checks(Check::OUTPUT).try_run() {
            failures.push(format!("{argv:?}\n{}", m.report()));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Arguments and environment of one external-diff case.
type ExtCase<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)]);

#[test]
fn external_diff_matches_git() {
    let mut fx = fixture();
    let mut failures = Vec::new();
    // Temporary file names differ between the programs.
    let cases: &[ExtCase] = &[
        (&["diff"], &[("GIT_EXTERNAL_DIFF", "./ext")]),
        (
            &["diff", "--no-ext-diff", "plain.txt"],
            &[("GIT_EXTERNAL_DIFF", "./ext")],
        ),
        (
            &["diff-files", "-p", "plain.txt"],
            &[("GIT_EXTERNAL_DIFF", "./ext")],
        ),
        (
            &["diff-files", "-p", "--ext-diff"],
            &[("GIT_EXTERNAL_DIFF", "./ext")],
        ),
        (&["diff"], &[("GIT_EXTERNAL_DIFF", "false")]),
        (&["diff", "--quiet"], &[("GIT_EXTERNAL_DIFF", "false")]),
        (
            &["diff", "--quiet"],
            &[
                ("GIT_EXTERNAL_DIFF", "false"),
                ("GIT_EXTERNAL_DIFF_TRUST_EXIT_CODE", "true"),
            ],
        ),
        (&["-c", "diff.external=./ext", "diff", "HEAD"], &[]),
        (&["-c", "diff.cpp.command=./ext", "diff"], &[]),
        (
            &["-c", "diff.cpp.command=./ext", "diff", "a.c"],
            &[("GIT_EXTERNAL_DIFF", "echo")],
        ),
    ];
    for (argv, env) in cases {
        let mut c = fx.compare(*argv).checks(Check::OUTPUT);
        for (k, v) in *env {
            c = c.env(k, v);
        }
        if let Err(m) = c.try_run() {
            failures.push(format!("{env:?} {argv:?}\n{}", m.report()));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn external_diff_of_rename() {
    fixture()
        .git(["mv", "plain.txt", "moved.txt"])
        .compare(["diff", "--cached", "-M"])
        .env("GIT_EXTERNAL_DIFF", "./ext")
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn output_option() {
    let mut fx = fixture();
    fx.compare(["diff", "--output=out.txt", "--stat"])
        .normalize_state(Normalizer::drop_lines("^index "))
        .run();
    fx.compare(["diff", "--output=out.txt"])
        .env("GIT_EXTERNAL_DIFF", "./ext")
        .run();
}

/// `diff.<driver>.cachetextconv` (t4042): the converted text of blobs is
/// kept in `refs/notes/textconv/<driver>`; a cache written by git is
/// used by rgit without running the filter, a changed command line
/// invalidates it, and work-tree files are not cached.
#[test]
fn textconv_cache_matches_git() {
    let helper = "#!/bin/sh\necho \"converted: $(cat \"$1\")\"\necho ran >>helper.out\n";
    let with = |cmd: &'static str, args: &[&'static str]| -> Vec<&'static str> {
        let mut v = vec!["-c", cmd, "-c", "diff.magic.cachetextconv=true"];
        v.extend_from_slice(args);
        v
    };
    let conv = "diff.magic.textconv=./helper";
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write_executable("helper", helper)
        .write("foo.bin", "foo content 1\n")
        .write("bar.bin", "bar content 1\n")
        .git(["add", "foo.bin", "bar.bin"])
        .git(["commit", "-q", "-m", "one"])
        .tick()
        .write("foo.bin", "foo content 2\n")
        .write("bar.bin", "bar content 2\n")
        .git(["commit", "-q", "-a", "-m", "two"])
        .write(".gitattributes", "*.bin diff=magic\n");
    // The first run fills the cache (the same notes commit as git's).
    fx = fx.same(with(conv, &["diff", "HEAD^", "HEAD"]));
    // A cache git wrote is used: the helper does not run again.
    fx = fx.same(with(conv, &["diff", "HEAD^", "HEAD"]));
    fx = fx.same(with(
        conv,
        &[
            "log",
            "--no-walk",
            "-p",
            "refs/notes/textconv/magic",
            "HEAD",
        ],
    ));
    // A work-tree file is converted but not cached.
    fx = fx.write("foo.bin", "foo content 3\n");
    fx = fx.same(with(conv, &["diff"]));
    // Another command line starts a new cache.
    let _ = fx.same(with(
        "diff.magic.textconv=sh ./helper",
        &["diff", "HEAD^", "HEAD"],
    ));
}

/// Outside a repository `diff --no-index` still applies diff drivers
/// from the global attributes file (t4042 "caching is silently ignored
/// outside repo").
#[test]
fn no_index_outside_repository_uses_global_attributes() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .write("attr", "* diff=test\n")
        .write("one", "one\n")
        .write("two", "two\n");
    let work = fx.work_dir();
    let attr = format!("core.attributesFile={}", work.join("attr").display());
    let ceiling = work.parent().expect("parent").to_path_buf();
    for extra in [&[][..], &["-c", "diff.test.cachetextconv=true"]] {
        let mut argv = vec!["-c", attr.as_str(), "-c", "diff.test.textconv=tr a-z A-Z <"];
        argv.extend_from_slice(extra);
        argv.extend(["diff", "--no-index", "one", "two"]);
        fx.compare(&argv)
            .checks(Check::OUTPUT)
            .env("GIT_CEILING_DIRECTORIES", &ceiling)
            .run();
    }
}
