// SPDX-License-Identifier: MIT
//! Differential tests of content conversion (#111) against the reference
//! git: `text`, `eol`, `crlf`, `core.autocrlf`, `core.eol`,
//! `core.safecrlf`, `ident` and `working-tree-encoding` in `add`,
//! `hash-object`, `update-index`, `status`, `diff`, `ls-files --eol` and
//! the checkout commands (`checkout --`, `checkout-index`, `read-tree -u`,
//! `reset --hard`, `switch`), with CR LF fixtures.

use rgit_difftest::{Check, Fixture};

const LF: &[u8] = b"$Id$\none\ntwo\n";
const CRLF: &[u8] = b"$Id$\r\none\r\ntwo\r\n";
const MIXED: &[u8] = b"$Id$\none\r\ntwo\n";
const LONE_CR: &[u8] = b"one\ntwo\rthree\n";
const NUL: &[u8] = b"one\r\ntw\0o\r\n";

const STATE: [Check; 5] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Index,
    Check::Objects,
];

/// Without the index: files whose conversion does not round-trip are
/// racily clean, and the reference git may be built with `USE_NSEC`
/// (then it keeps their cached size where rgit smudges it).
const CHECKOUT: [Check; 4] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Worktree,
];

/// Files of every kind committed without conversion.
fn committed() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("lf.txt", LF)
        .write("crlf.txt", CRLF)
        .write("mixed.txt", MIXED)
        .write("lonecr.txt", LONE_CR)
        .write("nul.txt", NUL)
        .write("sub/lf.c", LF)
        .write("sub/crlf.c", CRLF)
        .git(["add", "."])
        .git(["commit", "-q", "-m", "init"])
        .tick()
}

/// New (untracked) files of every kind.
fn untracked(attrs: &str) -> Fixture {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"]);
    let fx = if attrs.is_empty() {
        fx
    } else {
        fx.write(".gitattributes", attrs)
    };
    fx.write("lf.txt", LF)
        .write("crlf.txt", CRLF)
        .write("mixed.txt", MIXED)
        .write("lonecr.txt", LONE_CR)
        .write("nul.txt", NUL)
}

const ATTRS: &[&str] = &[
    "",
    "* text=auto\n",
    "* text\n",
    "* -text\n",
    "* eol=crlf\n",
    "* eol=lf\n",
    "* text=auto eol=crlf\n",
    "* crlf\n",
    "* -crlf\n",
    "* crlf=input\n",
    "* text ident\n",
    "*.txt text=auto\nmixed.txt -text\n",
];

const CONFIGS: &[&[&str]] = &[
    &[],
    &["-c", "core.autocrlf=true"],
    &["-c", "core.autocrlf=input"],
    &["-c", "core.eol=crlf"],
    &["-c", "core.autocrlf=true", "-c", "core.safecrlf=false"],
    &["-c", "core.autocrlf=input", "-c", "core.safecrlf=true"],
    &["-c", "core.autocrlf=true", "-c", "core.safecrlf=true"],
];

#[test]
fn add_new_files() {
    for attrs in ATTRS {
        for cfg in CONFIGS {
            let mut fx = untracked(attrs);
            let mut argv = cfg.to_vec();
            argv.extend(["add", "."]);
            fx.compare(&argv).checks(STATE).run();
        }
    }
}

#[test]
fn add_one_file_at_a_time_with_safecrlf() {
    for file in ["lf.txt", "crlf.txt", "mixed.txt", "lonecr.txt", "nul.txt"] {
        for cfg in CONFIGS {
            let mut fx = untracked("* text=auto\n*.txt ident\n");
            let mut argv = cfg.to_vec();
            argv.extend(["add", file]);
            fx.compare(&argv).checks(STATE).run();
        }
    }
}

#[test]
fn add_over_crlf_in_index() {
    // text=auto leaves files alone whose index blob has CR LF.
    for cfg in CONFIGS {
        let mut fx = committed()
            .write("crlf.txt", b"new\r\nline\r\n")
            .write("lf.txt", b"new\r\nline\r\n")
            .write("nul.txt", b"new\r\nline\r\n")
            .write("lonecr.txt", b"new\r\nline\r\n");
        let mut argv = cfg.to_vec();
        argv.extend(["add", "-u"]);
        fx.compare(&argv).checks(STATE).run();
    }
}

#[test]
fn renormalize() {
    for attrs in ["* text=auto\n", "* text\n", "*.c text eol=crlf\n"] {
        let mut fx = committed().write(".gitattributes", attrs);
        fx.compare(["add", "--renormalize", "."])
            .checks(STATE)
            .run();
        let mut fx = committed().write(".gitattributes", attrs);
        fx.compare(["-c", "core.autocrlf=true", "add", "--renormalize", "sub"])
            .checks(STATE)
            .run();
    }
}

#[test]
fn checkout_paths() {
    let attrs = [
        "",
        "* text=auto\n",
        "* text\n",
        "* eol=crlf\n",
        "* text=auto eol=crlf\n",
        "* -text\n",
        "* text eol=crlf ident\n",
        "*.c crlf=input\n*.txt crlf\n",
    ];
    let configs: &[&[&str]] = &[
        &[],
        &["-c", "core.autocrlf=true"],
        &["-c", "core.autocrlf=input"],
        &["-c", "core.eol=crlf"],
        &["-c", "core.eol=native"],
    ];
    for a in attrs {
        for cfg in configs {
            let mut fx = committed()
                .write(".gitattributes", a)
                .remove("lf.txt")
                .remove("crlf.txt")
                .remove("mixed.txt")
                .remove("lonecr.txt")
                .remove("nul.txt")
                .remove("sub");
            let mut argv = cfg.to_vec();
            argv.extend(["checkout", "--", "."]);
            fx.compare(&argv).checks(CHECKOUT).run();
        }
    }
}

#[test]
fn checkout_commands() {
    let cases: &[&[&str]] = &[
        &["-c", "core.autocrlf=true", "checkout-index", "-f", "-a"],
        &[
            "-c",
            "core.autocrlf=true",
            "checkout-index",
            "-f",
            "-u",
            "lf.txt",
            "sub/lf.c",
        ],
        &[
            "-c",
            "core.autocrlf=true",
            "read-tree",
            "--reset",
            "-u",
            "HEAD",
        ],
        &["-c", "core.autocrlf=true", "reset", "--hard"],
        &["-c", "core.eol=crlf", "reset", "--hard"],
        &["-c", "core.autocrlf=true", "restore", "."],
    ];
    for args in cases {
        let mut fx = committed()
            .write(".gitattributes", "*.c text\nlf.txt ident\n")
            .remove("lf.txt")
            .remove("sub/lf.c")
            .remove("crlf.txt");
        fx.compare(*args).checks(CHECKOUT).run();
    }
}

#[test]
fn switch_uses_index_attributes() {
    // The new commit's .gitattributes decides, not the old work tree file.
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("lf.txt", LF)
        .write("sub/lf.c", LF)
        .git(["add", "."])
        .git(["commit", "-q", "-m", "init"])
        .git(["checkout", "-q", "-b", "other"])
        .write(".gitattributes", "*.txt eol=crlf\n")
        .write("new.txt", "a\nb\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "attrs"])
        .git(["checkout", "-q", "main"]);
    fx.compare(["switch", "other"]).checks(CHECKOUT).run();
}

#[test]
fn status_and_diff_after_config_change() {
    let cases: &[&[&str]] = &[
        &["-c", "core.autocrlf=true", "status", "--porcelain"],
        &["-c", "core.autocrlf=input", "status", "--porcelain"],
        &["-c", "core.autocrlf=true", "diff", "--stat"],
        &["-c", "core.autocrlf=input", "diff"],
        &[
            "-c",
            "core.autocrlf=input",
            "-c",
            "core.safecrlf=true",
            "diff",
        ],
        &["-c", "core.autocrlf=true", "diff-files", "--name-only"],
    ];
    for attrs in ["", "* text\n", "* text=auto\n"] {
        for args in cases {
            let mut fx = committed()
                .write(".gitattributes", attrs)
                .write("mixed.txt", b"$Id$\none\r\ntwo\nthree\n")
                .write("crlf.txt", b"$Id$\none\ntwo\n");
            fx.compare(*args).checks(Check::OUTPUT).run();
        }
    }
}

#[test]
fn ls_files_eol() {
    let cases: &[&[&str]] = &[
        &["ls-files", "--eol"],
        &["ls-files", "--eol", "-s"],
        &["ls-files", "--eol", "-o"],
        &["ls-files", "--eol", "-d", "-t"],
        &["-c", "core.autocrlf=true", "ls-files", "--eol", "-z"],
    ];
    for args in cases {
        let mut fx = committed()
            .write(
                ".gitattributes",
                "lf.txt eol=lf\ncrlf.txt -text eol=crlf\nmixed.txt text=auto eol=crlf\n\
                 sub/* crlf=input\nnul.txt text=foo\nnew* text\n",
            )
            .write("new-crlf", "a\r\n")
            .write("new-bin", b"\x1aa\nb")
            .write("new-eof", b"a\nb\x1a")
            .write("empty", "")
            .remove("lonecr.txt")
            .symlink("lf.txt", "link");
        fx.compare(*args).checks(Check::OUTPUT).run();
    }
}

#[test]
fn hash_object() {
    let cases: &[&[&str]] = &[
        &[
            "-c",
            "core.autocrlf=true",
            "hash-object",
            "crlf.txt",
            "mixed.txt",
        ],
        &[
            "-c",
            "core.autocrlf=true",
            "hash-object",
            "--no-filters",
            "crlf.txt",
        ],
        &[
            "-c",
            "core.autocrlf=true",
            "-c",
            "core.safecrlf=true",
            "hash-object",
            "lf.txt",
        ],
        &[
            "-c",
            "core.autocrlf=true",
            "-c",
            "core.safecrlf=true",
            "hash-object",
            "-w",
            "lf.txt",
        ],
        &[
            "-c",
            "core.autocrlf=true",
            "-c",
            "core.safecrlf=warn",
            "hash-object",
            "-w",
            "lf.txt",
        ],
        &["hash-object", "--path=x.c", "--stdin"],
        &["hash-object", "--stdin"],
        &["hash-object", "-w", "--stdin-paths"],
        &["hash-object", "-t", "commit", "--literally", "crlf.txt"],
    ];
    for args in cases {
        let mut fx = untracked("*.c text ident\n*.txt text=auto\n");
        let input: &[u8] = if args.contains(&"--stdin-paths") {
            b"crlf.txt\nmixed.txt\n"
        } else {
            b"$Id: x $\r\nb\r\n"
        };
        fx.compare(*args).stdin(input.to_vec()).checks(STATE).run();
    }
}

#[test]
fn update_index() {
    let cases: &[&[&str]] = &[
        &[
            "-c",
            "core.autocrlf=input",
            "update-index",
            "--add",
            "crlf.txt",
            "mixed.txt",
        ],
        &[
            "-c",
            "core.autocrlf=true",
            "-c",
            "core.safecrlf=true",
            "update-index",
            "--add",
            "lf.txt",
        ],
        &[
            "-c",
            "core.autocrlf=input",
            "update-index",
            "--add",
            "--info-only",
            "crlf.txt",
        ],
    ];
    for args in cases {
        let mut fx = untracked("");
        fx.compare(*args).checks(STATE).run();
    }
}

#[test]
fn apply_converts() {
    // t0020 "apply patch": the patch applies to the blob form.
    for cfg in ["core.autocrlf=true", "core.autocrlf=input"] {
        let mut fx = committed()
            .write(
                "p.diff",
                "--- a/sub/lf.c\n+++ b/sub/lf.c\n@@ -2,2 +2,3 @@\n one\n two\n+three\n",
            )
            .git(["-c", cfg, "reset", "-q", "--hard"])
            .git(["-c", cfg, "rm", "-q", "--cached", "sub/lf.c"])
            .git(["-c", cfg, "checkout", "HEAD", "--", "sub/lf.c"]);
        fx.compare(["-c", cfg, "apply", "p.diff"])
            .checks([
                Check::ExitCode,
                Check::Stdout,
                Check::Stderr,
                Check::Worktree,
            ])
            .run();
    }
}

#[test]
fn working_tree_encoding() {
    let utf16le = |s: &str| -> Vec<u8> { s.encode_utf16().flat_map(u16::to_le_bytes).collect() };
    let mut bom16 = vec![0xff, 0xfe];
    bom16.extend(utf16le("h\u{e4}llo\nw\u{f6}rld\n"));
    let mut be_bom = vec![0xfe, 0xff, 0, b'a'];
    be_bom.extend([0, b'\n']);
    let attrs = "*.u16 text working-tree-encoding=UTF-16\n\
                 *.le text working-tree-encoding=UTF-16LE eol=crlf\n\
                 *.lebom working-tree-encoding=UTF-16LE-BOM\n\
                 *.u32 working-tree-encoding=utf-32\n\
                 *.lat working-tree-encoding=ISO-8859-1\n\
                 *.sj working-tree-encoding=SHIFT-JIS\n\
                 *.set working-tree-encoding\n\
                 *.bad working-tree-encoding=no-such-encoding\n";
    let base = || {
        Fixture::new()
            .rgit(env!("CARGO_BIN_EXE_rgit"))
            .git(["init", "-q"])
            .write(".gitattributes", attrs)
            .write("a.u16", &bom16)
            .write("b.le", utf16le("x\r\ny\r\n"))
            .write("c.lebom", &bom16)
            .write("d.lat", b"gr\xfc\xdf\n")
            .write("e.sj", b"\x82\xa0\n")
            .write("f.u16", &be_bom)
    };
    for file in ["a.u16", "b.le", "c.lebom", "d.lat", "e.sj", "f.u16"] {
        let mut fx = base();
        fx.compare(["add", file]).checks(STATE).run();
    }
    // Errors: BOM rules, unknown and value-less encodings.
    let errors: &[(&str, &[u8])] = &[
        ("g.u16", b"\0a\0b"),
        ("h.le", b"\xff\xfea\0"),
        ("i.u32", b"\0\0\0a"),
        ("j.set", b"x"),
        ("k.bad", b"x"),
        ("l.le", b"a\0\0\xd8"),
    ];
    for (file, data) in errors {
        let mut fx = base().write(file, data);
        fx.compare(["add", file]).checks(STATE).run();
    }
    // Checkout re-encodes.
    let mut fx = base()
        .git(["add", "."])
        .git(["commit", "-q", "-m", "enc"])
        .remove("a.u16")
        .remove("b.le")
        .remove("c.lebom")
        .remove("d.lat")
        .remove("e.sj");
    fx.compare(["checkout", "--", "."]).checks(CHECKOUT).run();
    let mut fx = base().git(["add", "."]).git(["commit", "-q", "-m", "enc"]);
    fx.compare(["status", "--porcelain"])
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn filter_drivers_are_not_run_but_reported() {
    // Unconfigured drivers are ignored by git as well: no message.
    let mut fx = untracked("*.txt filter=nosuch\n");
    fx.compare(["add", "lf.txt"]).checks(STATE).run();
}
