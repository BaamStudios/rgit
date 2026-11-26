// SPDX-License-Identifier: MIT
//! Differential tests of filter drivers (gitattributes(5) `filter`, #112)
//! against the reference git: single-file `clean` / `smudge` commands with
//! `%f`, `required` drivers, failures, empty overrides, and long-running
//! `process` filters (a small filter written for these tests speaks the
//! protocol of long-running-process-protocol.adoc and logs what it gets).

use rgit_difftest::{Check, Fixture};

const STATE: [Check; 5] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Index,
    Check::Objects,
];

const CHECKOUT: [Check; 4] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Worktree,
];

const ROT13: &str = "#!/bin/sh\ntr 'a-zA-Z' 'n-za-mN-ZA-M'\n";

const ARGS: &str = "#!/bin/sh\ncat >/dev/null\necho \"args: $# $*\"\n";

/// A long-running filter: rot13 for `clean` and `smudge`, a log of every
/// request in `filter.log` (key=value lines and content size), `error`
/// for content starting with "error", `abort` for "abort". With `delay`
/// it delays paths containing "delay" (and "missing", which it never
/// delivers) and names "unasked" as available once.
const PROCESS: &str = r#"#!/usr/bin/env python3
import sys, os
inp, out = sys.stdin.buffer, sys.stdout.buffer
log = open(os.environ.get("FILTER_LOG", "filter.log"), "a")
caps = sys.argv[1:]
def read_pkt():
    n = inp.read(4)
    if len(n) < 4:
        sys.exit(0)
    n = int(n, 16)
    return None if n == 0 else inp.read(n - 4)
def write_pkt(d):
    out.write(b"%04x" % (len(d) + 4) + d)
def flush():
    out.write(b"0000")
    out.flush()
def read_list():
    r = []
    while True:
        p = read_pkt()
        if p is None:
            return r
        r.append(p.rstrip(b"\n").decode())
def rot13(b):
    t = bytes.maketrans(b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ",
                        b"nopqrstuvwxyzabcdefghijklmNOPQRSTUVWXYZABCDEFGHIJKLM")
    return b.translate(t)
hello = read_list()
assert hello == ["git-filter-client", "version=2"], hello
write_pkt(b"git-filter-server\n"); write_pkt(b"version=2\n"); flush()
offered = read_list()
for c in caps:
    if "capability=" + c in offered:
        write_pkt(("capability=%s\n" % c).encode())
flush()
log.write("START\n"); log.flush()
delayed = {}
unasked = True
while True:
    keys = read_list()
    if keys == ["command=list_available_blobs"]:
        ready = sorted(p for p, v in delayed.items() if "missing" not in p and not isinstance(v, tuple))
        if any("unasked" in p for p in delayed) and unasked:
            ready.append("unasked")
            unasked = False
        log.write("IN: list %s\n" % " ".join(ready)); log.flush()
        for p in ready:
            write_pkt(("pathname=%s\n" % p).encode())
        flush()
        write_pkt(b"status=success\n"); flush()
        for p in ready:
            if p in delayed:
                delayed[p] = (delayed[p], "listed")
        continue
    data = b""
    while True:
        p = read_pkt()
        if p is None:
            break
        data += p
    log.write("IN: %s %d\n" % (" ".join(keys), len(data))); log.flush()
    path = [k[9:] for k in keys if k.startswith("pathname=")][0]
    if "can-delay=1" in keys and ("delay" in path or "missing" in path):
        delayed[path] = data
        write_pkt(b"status=delayed\n"); flush(); continue
    if path in delayed and isinstance(delayed[path], tuple):
        data = delayed.pop(path)[0]
    if data.startswith(b"error"):
        write_pkt(b"status=error\n"); flush(); continue
    if data.startswith(b"abort"):
        write_pkt(b"status=abort\n"); flush(); continue
    write_pkt(b"status=success\n"); flush()
    res = rot13(data)
    for i in range(0, len(res), 65516):
        write_pkt(res[i:i + 65516])
    flush(); flush()
"#;

fn base() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write_executable("rot13.sh", ROT13)
        .write_executable("args.sh", ARGS)
        .git(["config", "filter.rot13.clean", "./rot13.sh"])
        .git(["config", "filter.rot13.smudge", "./rot13.sh"])
}

#[test]
fn clean_and_smudge() {
    let fx = base()
        .write(".gitattributes", "*.r filter=rot13\n")
        .write("a.r", "hello world\n")
        .write("sub/b.r", "second\n")
        .write("plain", "hello\n");
    let mut fx = fx;
    fx.compare(["add", "."]).checks(STATE).run();
    let mut fx = fx
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .remove("a.r")
        .remove("sub/b.r");
    fx.compare(["checkout", "--", "."]).checks(CHECKOUT).run();
    fx.compare(["checkout-index", "--prefix=out/", "-a"])
        .checks(CHECKOUT)
        .run();
    fx.compare(["cat-file", "--filters", "HEAD:a.r"])
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["hash-object", "a.r", "plain"])
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["hash-object", "--no-filters", "a.r"])
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["hash-object", "--path=x.r", "plain"])
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["status", "--porcelain"])
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn switch_and_reset() {
    let fx = base()
        .write(".gitattributes", "*.r filter=rot13\n")
        .write("a.r", "one\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .git(["branch", "first"])
        .write("a.r", "two\n")
        .write("b.r", "new\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "two"]);
    let mut fx = fx;
    fx.compare(["checkout", "-q", "first"])
        .checks(CHECKOUT)
        .run();
    fx.compare(["reset", "-q", "--hard", "first"])
        .checks(CHECKOUT)
        .run();
    let mut fx = fx.git(["checkout", "-q", "first"]);
    fx.compare(["merge", "-q", "main"]).checks(CHECKOUT).run();
}

#[test]
fn path_substitution() {
    let mut fx = base()
        .write(".gitattributes", "name* filter=args\n")
        .write("name-plain", "x\n")
        .write("name  with 'sq' and $x", "x\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .git(["config", "filter.args.smudge", "sh ./args.sh %f --extra"])
        .remove("name-plain")
        .remove("name  with 'sq' and $x");
    fx.compare(["checkout", "--", "."]).checks(CHECKOUT).run();
}

#[test]
fn failures() {
    let fx = base()
        .write(
            ".gitattributes",
            "*.f filter=fail\n*.q filter=req\n*.c filter=cleanonly\n*.m filter=missing\n",
        )
        .git(["config", "filter.fail.clean", "false"])
        .git(["config", "filter.fail.smudge", "false"])
        .git(["config", "filter.req.clean", "false"])
        .git(["config", "filter.req.required", "true"])
        .git(["config", "filter.cleanonly.smudge", "cat"])
        .git(["config", "filter.cleanonly.required", "true"])
        .git(["config", "filter.missing.clean", "/nonexistent/filter"])
        .write("a.f", "a\n")
        .write("a.q", "a\n")
        .write("a.c", "a\n")
        .write("a.m", "a\n");
    let mut fx = fx;
    for path in ["a.f", "a.q", "a.c", "a.m"] {
        fx.compare(["add", path]).checks(STATE).run();
    }
    fx.compare(["-c", "filter.fail.clean=", "add", "a.f"])
        .checks(STATE)
        .run();
    let mut fx = fx
        .git(["-c", "filter.fail.clean=", "add", "a.f"])
        .git(["-c", "filter.req.clean=cat", "add", "a.q"])
        .git([
            "-c",
            "filter.fail.clean=",
            "-c",
            "filter.req.clean=cat",
            "commit",
            "-q",
            "-m",
            "x",
        ])
        .git(["config", "filter.req.smudge", "false"])
        .remove("a.f")
        .remove("a.q");
    fx.compare(["-c", "filter.fail.clean=", "checkout", "--", "a.f"])
        .checks(CHECKOUT)
        .run();
    fx.compare(["-c", "filter.req.clean=cat", "checkout", "--", "a.q"])
        .checks(CHECKOUT)
        .run();
}

#[test]
fn filter_that_does_not_read() {
    let big = "0123456789abcdef".repeat(16 * 1024);
    let mut fx = base()
        .write(".gitattributes", "big filter=epipe\n")
        .git(["config", "filter.epipe.clean", "echo xyzzy"])
        .write("big", big);
    fx.compare(["add", "big"]).checks(STATE).run();
}

fn have_python() -> bool {
    std::process::Command::new("python3")
        .arg("-c")
        .arg("pass")
        .status()
        .is_ok_and(|s| s.success())
}

fn process_fixture(caps: &str) -> Fixture {
    base()
        .write_executable("proc.py", PROCESS)
        .git([
            "config",
            "filter.p.process",
            &format!("python3 ./proc.py {caps}"),
        ])
        .git(["config", "filter.p.required", "true"])
        .write(".gitattributes", "*.r filter=p\n")
}

#[test]
fn process_filter() {
    if !have_python() {
        eprintln!("skipped: no python3");
        return;
    }
    let big = "abcdefghij".repeat(14_000);
    let fx = process_fixture("clean smudge")
        .write("a.r", "hello\n")
        .write("sub/b.r", "world\n")
        .write("empty.r", "")
        .write("big.r", big)
        .write("plain", "x\n");
    let mut fx = fx;
    fx.compare(["add", "."]).checks(STATE).run();
    let mut fx = fx
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .git(["branch", "other"])
        .remove("filter.log")
        .remove("a.r")
        .remove("sub/b.r");
    fx.compare(["checkout", "--", "."]).checks(CHECKOUT).run();
    let mut fx = fx
        .git(["checkout", "-q", "--", "."])
        .git(["rm", "-q", "--cached", "a.r", "sub/b.r", "empty.r", "big.r"])
        .git(["commit", "-q", "-m", "two"])
        .remove("filter.log");
    fx.compare(["checkout", "-q", "other"])
        .checks(CHECKOUT)
        .run();
}

#[test]
fn process_filter_errors() {
    if !have_python() {
        eprintln!("skipped: no python3");
        return;
    }
    let fx = process_fixture("clean smudge")
        .git(["config", "filter.p.required", "false"])
        .write("a.r", "abort here\n")
        .write("b.r", "fine\n")
        .write("c.r", "error here\n");
    let mut fx = fx;
    fx.compare(["add", "."]).checks(STATE).run();
    let mut fx = fx.git(["config", "filter.p.required", "true"]);
    fx.compare(["add", "c.r"]).checks(STATE).run();
    // Only `clean`: checkout leaves the content alone.
    let mut fx = process_fixture("clean")
        .write("a.r", "hello\n")
        .git(["add", "a.r"])
        .git(["config", "filter.p.required", "false"])
        .remove("a.r")
        .remove("filter.log");
    fx.compare(["checkout", "--", "a.r"]).checks(CHECKOUT).run();
    // Not a filter process at all.
    let mut fx = base()
        .write(".gitattributes", "*.r filter=p\n")
        .git(["config", "filter.p.process", "cat"])
        .git(["config", "filter.p.required", "true"])
        .write("a.r", "hello\n");
    fx.compare(["add", "a.r"]).checks(STATE).run();
}

#[test]
fn delayed_checkout() {
    if !have_python() {
        eprintln!("skipped: no python3");
        return;
    }
    let mut fx = process_fixture("clean smudge delay")
        .write("a.r", "hello\n")
        .write("delay1.r", "one\n")
        .write("delay2.r", "two\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .remove("filter.log")
        .remove("a.r")
        .remove("delay1.r")
        .remove("delay2.r");
    fx.compare(["checkout", "--", "."]).checks(CHECKOUT).run();
    let mut fx = fx
        .git(["checkout", "-q", "-b", "side"])
        .git(["rm", "-q", "a.r", "delay1.r", "delay2.r"])
        .git(["commit", "-q", "-m", "gone"])
        .remove("filter.log");
    fx.compare(["checkout", "-q", "main"])
        .checks(CHECKOUT)
        .run();
    // The `Filtering content` meter over the delayed paths.
    for args in [
        &["checkout", "--progress", "--", "."][..],
        &["checkout", "--quiet", "--progress", "--", "."],
        &["checkout", "--no-progress", "--", "."],
        &["checkout", "--", "."],
    ] {
        fx = fx
            .remove("filter.log")
            .remove("a.r")
            .remove("delay1.r")
            .remove("delay2.r");
        fx.compare(args)
            .env("GIT_PROGRESS_DELAY", "0")
            .checks(CHECKOUT)
            .normalize_stderr(throughput())
            .run();
    }
    // Branch checkout: git rewrites every entry with `-f` (its `Updating
    // files` total differs), so only the filter meter is checked here.
    for (args, shown) in [
        (&["checkout", "--progress", "-f", "HEAD"][..], true),
        (&["checkout", "--quiet", "--progress", "-f", "HEAD"], true),
        (&["checkout", "-f", "HEAD"], false),
    ] {
        fx = fx.remove("delay1.r").remove("delay2.r");
        let home = fx.home_dir();
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_rgit"))
            .args(args)
            .current_dir(fx.work_dir())
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_PROGRESS_DELAY", "0")
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            err.contains("Filtering content: 100% (2/2), 8 bytes"),
            shown,
            "{args:?}: {err}"
        );
    }
}

/// The rate in a throughput meter (`8 bytes | 8.00 KiB/s`) is the bytes
/// over the time the command took, which differs from run to run; the
/// byte count stays checked.
fn throughput() -> rgit_difftest::Normalizer {
    rgit_difftest::Normalizer::regex(r" \| [0-9.]+ (bytes|KiB|MiB|GiB)/s", " | <RATE>")
}

#[test]
fn delayed_checkout_failures() {
    if !have_python() {
        eprintln!("skipped: no python3");
        return;
    }
    for name in ["missing.r", "delay-unasked.r"] {
        let mut fx = process_fixture("clean smudge delay")
            .write(name, "content\n")
            .git(["add", "."])
            .git(["commit", "-q", "-m", "one"])
            .remove("filter.log")
            .remove(name);
        fx.compare(["checkout", "--", "."]).checks(CHECKOUT).run();
    }
}

/// `archive` tells a `process` filter the ref its argument stands for and
/// the commit (for a tree: the tree) it packs (`ref=`, `treeish=` in
/// `filter.log`).
#[test]
fn archive_tells_ref_and_treeish() {
    if !have_python() {
        eprintln!("skipped: no python3");
        return;
    }
    let mut fx = process_fixture("clean smudge")
        .write("a.r", "hello\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .git(["tag", "-a", "-m", "t", "v1"])
        .git(["branch", "other"]);
    if fx.is_skipped() {
        return;
    }
    for name in [
        "HEAD",
        "@",
        "other",
        "heads/other",
        "v1",
        "refs/tags/v1",
        "HEAD~0",
        "v1^{commit}",
        "HEAD^{tree}",
    ] {
        for file in ["filter.log", "out.tar"] {
            let _ = std::fs::remove_file(fx.work_dir().join(file));
        }
        fx.compare(["archive", "-o", "out.tar", name])
            .checks(CHECKOUT)
            .run();
    }
    // A detached HEAD is the ref `HEAD`.
    let mut fx = fx.git(["checkout", "-q", "--detach"]);
    let _ = std::fs::remove_file(fx.work_dir().join("filter.log"));
    fx.compare(["archive", "-o", "out.tar", "HEAD"])
        .checks(CHECKOUT)
        .run();
}
