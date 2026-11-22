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
