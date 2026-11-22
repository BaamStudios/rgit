// SPDX-License-Identifier: MIT
//! Differential tests of `rgit grep` against the reference git: output,
//! colours and exit codes for the work tree, the index, trees, untracked
//! files and `--no-index`. Every case runs in a temporary fixture
//! (rgit-difftest), so nothing outside it is touched.

use rgit_difftest::Fixture;

const HELLO_C: &str = "#include <assert.h>\n#include <stdio.h>\n\nint main(int argc, const char **argv)\n{\n\tprintf(\"Hello world.\\n\");\n\treturn 0;\n\t/* char ?? */\n}\n\n";

const HELLO_PS1: &str = "# No-op.\nfunction dummy() {}\n\n# Say hello.\nfunction hello() {\n  echo \"Hello world.\"\n  echo \"Hello again.\"\n} # hello\n\n# Still a no-op.\nfunction dummy() {}\n";

/// Function headers for `-p`/`-W`: `func` lines and other lines starting
/// with a letter, comments, blank lines.
const FUNCS: &str = "a1\n\n# c1\n# c2\nfunc one\n x\n\n\ncode\n# c3\nfunc two\n y\n\n";

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("hello.c", HELLO_C)
        .write(
            "file",
            "foo mmap bar\nfoo_mmap bar\nfoo_mmap bar mmap\nfoo mmap bar_mmap\nfoo_mmap bar mmap baz\n",
        )
        .write("hello_world", "Hello world\nHeLLo world\nHello_world\nHeLLo_world\n")
        .write("ab", "a+b*c\na+bc\nabc\n")
        .write("d0", "d\n0\n")
        .write("v", "vvv\n")
        .write("x", "x x xx x\n")
        .write("y", "y yy\n")
        .write("z", "zzz\n")
        .write("t/t", "test\n")
        .write("t/v", "vvv\n")
        .write("t/a/v", "vvv\n")
        .write(
            "space",
            "line without leading space1\n\tline with leading space1\n\tline with leading space2\n\tline with leading space3\nline without leading space2\n",
        )
        .write("hello.ps1", HELLO_PS1)
        .write("funcs", FUNCS)
        .write("invalid-utf8", b"\x80\nASCII\n")
        .write("\"unusual\" pathname", "unusual\n")
        .write("t/nested \"unusual\" pathname", "unusual\n")
        .write("bin", b"binary\0file\0m[*]c\0*\xc3\xa6\0\xc3\xb0\n")
        .write("double-dash", "--\n->\nother\n")
        .write(".gitignore", "*.log\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "initial"])
        .write("untracked.txt", "untr here\n")
        .write("ignored.log", "untr ignored\n")
        .write("attrs", "hello.c diff=custom\n")
        .write("attrs-binary", "hello_world -diff\nbin diff\n")
        .write("attrs-textconv", "bin diff=foo\n")
        .write_executable("q.sh", "#!/bin/sh\ntr '\\000' Q <\"$1\"\n")
}

/// Runs `grep <args>` (with `pre` before the command) in `dir` with both
/// binaries and compares stdout, stderr and the exit code.
fn each(dir: &str, pre: &[&str], cases: &[&[&str]]) {
    let mut fx = fixture();
    for args in cases {
        let mut argv: Vec<&str> = pre.to_vec();
        argv.push("grep");
        argv.extend_from_slice(args);
        fx.compare(&argv).in_dir(dir).output_only().run();
    }
}

#[test]
fn words_columns_and_expressions() {
    each(
        ".",
        &[],
        &[
            &["-n", "-w", "-e", "mmap"],
            &["-n", "-w", "-e", "mmap", "HEAD"],
            &["--column", "-w", "-e", "mmap"],
            &["--column", "-w", "-e", "mmap$", "--or", "-e", "baz"],
            &[
                "--column",
                "--invert-match",
                "-w",
                "-e",
                "baz",
                "--",
                "file",
            ],
            &[
                "--column",
                "-v",
                "-e",
                "ll",
                "--or",
                "--not",
                "-e",
                "_",
                "--",
                "hello_world",
            ],
            &[
                "--column", "-v", "--not", "-e", "_", "--and", "--not", "-e", "ll",
            ],
            &[
                "--column", "--not", "(", "--not", "-e", "foo", "--or", "--not", "-e", "baz", ")",
            ],
            &["--column", "-w", "-C1", "-e", "mmap"],
            &["-n", "--column", "-w", "-e", "bar", "-e", "mmap"],
            &["-n", "-w", "-e", "x xx* x"],
            &["-n", "-w", "-e", "^y y"],
            &["--column", "-n", "-o", "-e", "mmap"],
            &["-e", "foo mmap", "--and", "-e", "bar_mmap"],
            &[
                "(", "-e", "foo_", "--or", "-e", "baz", ")", "--and", "-e", " mmap",
            ],
            &["-e", "foo mmap", "--and", "--not", "-e", "bar_mmap"],
            &["-e", "A", "--not", "--or", "-e", "B", "--", "ab"],
            &["--all-match", "-e", "foo", "-e", "baz"],
            &["--all-match", "-e", "foo", "-e", "nothing"],
            &["-o", "-e", "He", "-e", "Hel", "hello_world"],
            &["-o", "-E", "-e", "H|He", "hello_world"],
            &["-o", "o\\|l*", "hello_world"],
            &["--column", "-e", "l*", "-e", "o", "hello_world"],
        ],
    );
}

#[test]
fn pattern_types_and_errors() {
    each(
        ".",
        &["-c", "grep.extendedRegexp=true"],
        &[&["a+b*c", "ab"]],
    );
    each(
        ".",
        &["-c", "grep.patternType=fixed"],
        &[&["a+b*c", "ab"], &["-G", "a+b", "ab"]],
    );
    each(
        ".",
        &[
            "-c",
            "grep.patternType=extended",
            "-c",
            "grep.patternType=default",
        ],
        &[&["a+b*c", "ab"]],
    );
    each(
        ".",
        &[],
        &[
            &["-G", "-E", "-F", "a+b*c", "ab"],
            &["-E", "-F", "-G", "a+b*c", "ab"],
            &["-F", "-G", "-E", "a+b*c", "ab"],
            &["-Fi", "CHAR *"],
            &["-c", "-E", "z{,2}"],
            &["-c", "-E", "a{1,}"],
            &["-G", "a["],
            &["-E", "a["],
            &["("],
            &["-e", "A", "--and"],
            &["--and", "-e", "A"],
            &["-e", "A", ")"],
            &["--not"],
            &[],
            &["--threads=-1", "o"],
            &["-C", "x", "o"],
            &["--color=foo", "o"],
            &["--cached", "--untracked", "o"],
            &["--cached", "o", "HEAD"],
            &["--untracked", "o", "HEAD"],
            &["o", "does-not-exist"],
            &["o", "hello.c", "--"],
            &["o", "HEAD:nonexist"],
            &["--max-count", "0", "foo"],
        ],
    );
}

#[test]
fn paths_depth_and_trees() {
    each(
        ".",
        &[],
        &[
            &["-n", "-e", "test", "HEAD"],
            &["--max-depth", "-1", "-n", "-e", "vvv"],
            &["--max-depth", "0", "-n", "-e", "vvv", "HEAD"],
            &["--no-recursive", "-n", "-e", "vvv", "--", "*"],
            &["--max-depth", "1", "-n", "-e", "vvv"],
            &["--max-depth", "0", "-n", "-e", "vvv", "--", "t", "."],
            &["unusual"],
            &["-z", "unusual", "HEAD"],
            &["vvv", "HEAD:t"],
            &["vvv", "HEAD:t/a/v"],
            &[
                "-e",
                "vvv",
                "-e",
                "Hello",
                "HEAD",
                "main",
                "--",
                "v",
                "hello_world",
            ],
            &["-c", "vvv", "HEAD^{tree}"],
            &["--", "->"],
            &["--", "->", "--", "double-dash"],
            &["-e", "--", "--", "double-dash"],
            &["--cached", "-L", "nothing"],
            &["--untracked", "untr"],
            &["--untracked", "--no-exclude-standard", "untr"],
            &["--no-index", "untr"],
            &["--no-index", "--exclude-standard", "-l", "untr"],
            &["--no-index", "o", "main", "--"],
        ],
    );
    each(
        "t",
        &[],
        &[
            &["-n", "-e", "test"],
            &["--full-name", "-n", "-e", "test", "HEAD"],
            &["unusual"],
            &["vvv", ".."],
            &["vvv", "HEAD", "--", ".."],
            &["-l", "-e", "test", "-e", "vvv", "--", "../v", "a"],
            &["-h", "vvv"],
        ],
    );
}

#[test]
fn output_modes() {
    each(
        ".",
        &[],
        &[
            &["-l", "-C1", "foo"],
            &["-c", "-C1", "foo"],
            &["-L", "-C1", "nonexistent_string"],
            &["--files-without-match", "--quiet", "nonexistent_string"],
            &["--max-count", "3", "foo"],
            &["--max-count", "1", "--context", "1", "foo_mmap"],
            &["--max-count", "1", "--show-function", "Hello", "hello.ps1"],
            &["--max-count", "1", "-e", "o", "--", "hello.*"],
            &[
                "--max-count",
                "1",
                "--context",
                "1",
                "-e",
                "o",
                "--",
                "hello.*",
            ],
            &["-q", "mmap"],
            &["-q", "qfwfq"],
            &["-C1", "^[yz]"],
            &[
                "--break",
                "-A1",
                "-e",
                "char",
                "-e",
                "lo_w",
                "hello.c",
                "hello_world",
            ],
            &[
                "--heading",
                "-e",
                "char",
                "-e",
                "lo_w",
                "hello.c",
                "hello_world",
            ],
            &["-C1", "--heading", "-n", "^[yz]"],
            &["-z", "-n", "--column", "^[yz]"],
            &["-z", "-c", "^[yz]"],
            &["-z", "--heading", "-n", "^[yz]"],
            &["-c", "^$"],
            &["-3n", "mmap", "file"],
            &["A.", "invalid-utf8"],
        ],
    );
}

#[test]
fn functions() {
    each(
        ".",
        &[],
        &[
            &["-p", "return"],
            &["-p", "-B5", "return"],
            &["-n", "-p", "-B1", "return", "hello.c"],
            &["-W", "return"],
            &["-W", "stdio"],
            &["-n", "-W", "echo", "hello.ps1"],
            &["-n", "-W", "x", "funcs"],
            &["-n", "-W", "-e", "x", "-e", "y", "funcs"],
            &["-n", "-p", "-e", "one", "-e", "y", "funcs"],
            &["-n", "-p", "-W", "func one", "funcs"],
        ],
    );
    each(
        ".",
        &[
            "-c",
            "diff.custom.funcname=^#",
            "-c",
            "core.attributesFile=attrs",
        ],
        &[&["-p", "return"]],
    );
}

#[test]
fn colors() {
    each(
        ".",
        &[],
        &[
            &[
                "--color=always",
                "-A1",
                "-e",
                "char",
                "-e",
                "lo_w",
                "hello.c",
                "hello_world",
            ],
            &[
                "--color=always",
                "-n",
                "-p",
                "-C1",
                "-e",
                "char",
                "-e",
                "lo_w",
            ],
            &[
                "--break",
                "--heading",
                "-n",
                "--color",
                "-e",
                "char",
                "-e",
                "lo_w",
            ],
            &[
                "--color=always",
                "-p",
                "-C3",
                "-e",
                "int",
                "--and",
                "-e",
                "Hello",
                "hello.c",
            ],
            &["--color=always", "-o", "-n", "--column", "char", "hello.c"],
            &["--color=always", "-c", "char"],
            &["--color=always", "-z", "-n", "char", "hello.c"],
            &["--color=always", "-l", "char"],
            &["--color=always", "ina"],
            &["--color=always", "l*", "hello_world"],
        ],
    );
    each(
        ".",
        &[
            "-c",
            "color.grep.selected=blue",
            "-c",
            "color.grep.context=yellow",
            "-c",
            "color.grep.matchContext=reverse",
            "-c",
            "color.grep.function=red",
            "-c",
            "color.grep.separator=green",
        ],
        &[
            &["--color=always", "-e", "^foo", "-e", "bar$", "file"],
            &["--color=always", "-v", "-C1", "-e", "^a+b", "ab"],
            &[
                "--color=always",
                "-n",
                "--column",
                "-p",
                "-C1",
                "-e",
                "lo_w",
                "-e",
                "char",
            ],
        ],
    );
}

#[test]
fn binary_files() {
    each(
        ".",
        &[],
        &[
            &["ina", "bin"],
            &["-ah", "ina", "bin"],
            &["-I", "ina", "bin"],
            &["-c", "ina", "bin"],
            &["-l", "ina", "bin"],
            &["-L", "bar", "bin"],
            &["-F", "ile", "bin"],
            &[".fi", "bin"],
            &["ile", "bin"],
            &["-n", "-A1", "ina"],
            &["-A1", "-e", "ina", "-e", "mmap", "--", "bin", "file"],
            &["-o", "ina"],
        ],
    );
    each(
        ".",
        &["-c", "core.attributesFile=attrs-binary"],
        &[
            &["Hello", "hello_world"],
            &["--cached", "Hello", "hello_world"],
            &["ina", "bin"],
        ],
    );
}

#[test]
fn textconv() {
    let mut fx = fixture();
    let dir = fx.work_dir();
    let conv = format!("diff.foo.textconv={}/q.sh", dir.display());
    for args in [
        &["--textconv", "Qfile"][..],
        &["Qfile"],
        &["--no-textconv", "Qfile"],
        &["--textconv", "Qfile", "HEAD:bin"],
    ] {
        let mut argv = vec![
            "-c",
            "core.attributesFile=attrs-textconv",
            "-c",
            &conv,
            "grep",
        ];
        argv.extend_from_slice(args);
        fx.compare(&argv).output_only().run();
    }
}

#[test]
fn index_flags() {
    let mut fx = fixture()
        .write("cache-this", "only in the index\n")
        .git(["add", "cache-this"])
        .remove("cache-this")
        .write("ita", "intend to add this\n")
        .git(["add", "-N", "ita"])
        .git(["update-index", "--assume-unchanged", "t/t"])
        .remove("t/t");
    for args in [
        &["test"][..],
        &["only in the index"],
        &["--cached", "only in the index"],
        &["intend to add"],
        &["--cached", "intend to add"],
        &["-L", "--cached", "nothing"],
        &["-L", "nothing"],
    ] {
        let mut argv = vec!["grep"];
        argv.extend_from_slice(args);
        fx.compare(&argv).output_only().run();
    }
}
