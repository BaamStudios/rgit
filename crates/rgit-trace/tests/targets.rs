// SPDX-License-Identifier: MIT
//! End-to-end checks of trace targets. Each test re-runs this test binary
//! with `RGIT_TRACE_CHILD` set, so the static keys see a fresh environment.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

const TRACE_VARS: &[&str] = &[
    "GIT_TRACE",
    "GIT_TRACE_SETUP",
    "GIT_TRACE_PERFORMANCE",
    "GIT_TRACE_BARE",
    "GIT_TRACE_PACKFILE",
];

/// What the child does when `RGIT_TRACE_CHILD` is set.
#[test]
fn child_entry() {
    let Ok(mode) = std::env::var("RGIT_TRACE_CHILD") else {
        return;
    };
    match mode.as_str() {
        "setup" => {
            rgit_trace::trace_setup(
                Path::new(".git"),
                Path::new(".git"),
                Some(Path::new("/w/sp ace")),
                Path::new("/w/sp ace"),
                Some(OsStr::new("s b/")),
            );
            rgit_trace::trace_setup(
                Path::new("."),
                Path::new("."),
                None,
                Path::new("/b.git"),
                None,
            );
        }
        "builtin" => {
            rgit_trace::trace_builtin(&["version", "a b", "it's"]);
            rgit_trace::trace!(rgit_trace::TRACE, "trace: exec: {}", "git-foo");
        }
        "perf" => {
            rgit_trace::trace_command_performance(Instant::now(), &["git", "status"]);
        }
        "raw" => {
            rgit_trace::TRACE_PACKFILE.write_raw(b"PACK\0\x01");
        }
        other => panic!("unknown child mode {other}"),
    }
}

fn run(mode: &str, env: &[(&str, &str)], shell_redirect: Option<&str>) -> Output {
    let exe = std::env::current_exe().unwrap();
    let mut cmd = match shell_redirect {
        Some(redir) => {
            let mut c = Command::new("sh");
            c.arg("-c")
                .arg(format!("exec \"$0\" \"$@\" {redir}"))
                .arg(&exe);
            c
        }
        None => Command::new(&exe),
    };
    cmd.args(["--exact", "child_entry", "--nocapture", "--test-threads=1"]);
    for v in TRACE_VARS {
        cmd.env_remove(v);
    }
    cmd.env("RGIT_TRACE_CHILD", mode);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{out:?}");
    out
}

/// Trace lines from stderr, dropping the test harness' own chatter.
fn stderr_lines(out: &Output) -> Vec<String> {
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .map(str::to_string)
        .collect()
}

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("rgit-trace");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

const SETUP_EXPECTED: &str = "setup: git_dir: .git
setup: git_common_dir: .git
setup: worktree: /w/sp ace
setup: cwd: /w/sp ace
setup: prefix: s b/
setup: git_dir: .
setup: git_common_dir: .
setup: worktree: (null)
setup: cwd: /b.git
setup: prefix: (null)
";

#[test]
fn setup_bare_to_stderr() {
    for on in ["1", "2", "true", "TRUE"] {
        let out = run(
            "setup",
            &[("GIT_TRACE_SETUP", on), ("GIT_TRACE_BARE", "1")],
            None,
        );
        assert_eq!(String::from_utf8_lossy(&out.stderr), SETUP_EXPECTED, "{on}");
    }
}

#[test]
fn disabled_values_write_nothing() {
    for off in ["", "0", "false", "FALSE"] {
        let out = run("setup", &[("GIT_TRACE_SETUP", off)], None);
        assert!(out.stderr.is_empty(), "{off}: {out:?}");
    }
}

#[test]
fn header_format() {
    let out = run("builtin", &[("GIT_TRACE", "1")], None);
    let lines = stderr_lines(&out);
    assert_eq!(lines.len(), 2, "{lines:?}");
    for l in &lines {
        let b = l.as_bytes();
        // HH:MM:SS.uuuuuu
        for i in [0, 1, 3, 4, 6, 7, 9, 10, 11, 12, 13, 14] {
            assert!(b[i].is_ascii_digit(), "{l}");
        }
        assert_eq!((b[2], b[5], b[8], b[15]), (b':', b':', b'.', b' '), "{l}");
        assert!(l[16..].starts_with("targets.rs:"), "{l}");
        assert_eq!(&l[39..41], " t", "{l}");
    }
    assert_eq!(
        &lines[0][40..],
        r"trace: built-in: git version 'a b' 'it'\''s'"
    );
    assert_eq!(&lines[1][40..], "trace: exec: git-foo");
}

#[test]
fn file_target_appends() {
    let path = scratch("append");
    let p = path.to_str().unwrap();
    std::fs::write(&path, "existing\n").unwrap();
    for _ in 0..2 {
        let out = run(
            "setup",
            &[("GIT_TRACE_SETUP", p), ("GIT_TRACE_BARE", "1")],
            None,
        );
        assert!(out.stderr.is_empty(), "{out:?}");
    }
    let got = std::fs::read_to_string(&path).unwrap();
    assert_eq!(got, format!("existing\n{SETUP_EXPECTED}{SETUP_EXPECTED}"));
}

#[test]
fn fd_target() {
    let path = scratch("fd");
    let redir = format!("7>>'{}'", path.display());
    let out = run(
        "builtin",
        &[("GIT_TRACE", "7"), ("GIT_TRACE_BARE", "true")],
        Some(&redir),
    );
    assert!(out.stderr.is_empty(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "trace: built-in: git version 'a b' 'it'\\''s'\ntrace: exec: git-foo\n"
    );
}

#[test]
fn bad_fd_warns_once() {
    let out = run("setup", &[("GIT_TRACE_SETUP", "8")], Some("8>&-"));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "warning: unable to write trace for GIT_TRACE_SETUP: Bad file descriptor\n"
    );
}

#[test]
fn unknown_value_warns() {
    let out = run("builtin", &[("GIT_TRACE", "10")], None);
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "warning: unknown trace value for 'GIT_TRACE': 10\n         \
         If you want to trace into a file, then please set GIT_TRACE\n         \
         to an absolute pathname (starting with /)\n"
    );
}

#[test]
fn unopenable_file_warns() {
    let out = run(
        "setup",
        &[("GIT_TRACE_SETUP", "/nonexistent-rgit-dir/f")],
        None,
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "warning: could not open '/nonexistent-rgit-dir/f' for tracing: No such file or directory\n"
    );
}

#[test]
fn bare_value_is_a_target() {
    // An unknown GIT_TRACE_BARE value warns and leaves headers on.
    let out = run(
        "builtin",
        &[("GIT_TRACE", "1"), ("GIT_TRACE_BARE", "yes")],
        None,
    );
    let lines = stderr_lines(&out);
    assert!(lines[0].starts_with("warning: unknown trace value for 'GIT_TRACE_BARE': yes"));
    assert!(lines[3].ends_with("trace: built-in: git version 'a b' 'it'\\''s'"));
    assert_ne!(lines[3].as_bytes()[0], b't');
}

#[test]
fn performance_line() {
    let out = run(
        "perf",
        &[("GIT_TRACE_PERFORMANCE", "1"), ("GIT_TRACE_BARE", "1")],
        None,
    );
    let s = String::from_utf8_lossy(&out.stderr);
    let rest = s.strip_prefix("performance: ").unwrap();
    let (secs, tail) = rest.split_once(" s: ").unwrap();
    assert_eq!(tail, "git command: git status\n");
    let (whole, frac) = secs.split_once('.').unwrap();
    assert!(whole.chars().all(|c| c.is_ascii_digit()));
    assert_eq!(frac.len(), 9);
}

#[test]
fn packfile_is_verbatim() {
    let path = scratch("pack");
    let out = run(
        "raw",
        &[("GIT_TRACE_PACKFILE", path.to_str().unwrap())],
        None,
    );
    assert!(out.stderr.is_empty(), "{out:?}");
    assert_eq!(std::fs::read(&path).unwrap(), b"PACK\0\x01");
}
