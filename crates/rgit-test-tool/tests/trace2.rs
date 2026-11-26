// SPDX-License-Identifier: MIT

//! `trace2` (t0210, t0211, t0212, t0213) against the reference `test-tool`:
//! the same call with all three trace2 targets (brief) on both binaries,
//! comparing exit code, stdout, stderr and the three trace files after
//! replacing what differs between runs (times, session ids, process ids,
//! the program path, the version, `BUG:` source locations).

mod common;

use std::path::Path;
use std::process::Command;

use rgit_difftest::{Normalizer, unified_diff};

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    normal: String,
    perf: String,
    event: String,
}

fn run(tool: &Path, args: &[&str], dir: &Path, env: &[(&str, &str)]) -> Run {
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap_or_default();
    for f in ["normal", "perf", "event"] {
        let _ = std::fs::remove_file(dir.join(f));
    }
    let mut cmd = Command::new(tool);
    cmd.args(args).current_dir(dir);
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(k);
        }
    }
    cmd.env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TRACE2", dir.join("normal"))
        .env("GIT_TRACE2_BRIEF", "1")
        .env("GIT_TRACE2_PERF", dir.join("perf"))
        .env("GIT_TRACE2_PERF_BRIEF", "1")
        .env("GIT_TRACE2_EVENT", dir.join("event"))
        .env("GIT_TRACE2_EVENT_BRIEF", "1");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run test-tool");
    Run {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        normal: read("normal"),
        perf: read("perf"),
        event: read("event"),
    }
}

fn normalizers(tools: &[&Path], dir: &Path) -> Vec<Normalizer> {
    let mut n: Vec<Normalizer> = tools
        .iter()
        .map(|t| Normalizer::replace(t.to_string_lossy(), "TEST-TOOL"))
        .collect();
    n.extend([
        Normalizer::replace(dir.to_string_lossy(), "DIR"),
        Normalizer::regex(r#""sid":"[^"]*""#, r#""sid":"SID""#),
        Normalizer::regex(r#""time":"[^"]*""#, r#""time":"TIME""#),
        Normalizer::regex(
            r#""(t_abs|t_rel|t_total|t_min|t_max|pid)":-?[0-9.]+"#,
            r#""$1":N"#,
        ),
        Normalizer::regex(r"\b(elapsed|total|min|max|pid):-?[0-9.]+", "$1:N"),
        Normalizer::regex(r"\| +[0-9]+\.[0-9]+ \|", "| N |"),
        Normalizer::regex(r"\| +[0-9]+\.[0-9]+ \|", "| N |"),
        Normalizer::regex(r"2\.56\.1\.rgit\.[0-9.]+", "2.56.1"),
        Normalizer::regex(r"(?m)^BUG: [^ ]+: ", "BUG: LOC: "),
        // The threads of the threaded verbs announce themselves in any
        // order.
        Normalizer::regex(r"th[0-9]{2}:ut_", "thNN:ut_"),
    ]);
    n
}

fn normalize(n: &[Normalizer], s: &str, sort: bool) -> String {
    let mut s = n.iter().fold(s.to_string(), |acc, n| n.apply(&acc));
    if sort {
        let mut lines: Vec<&str> = s.lines().collect();
        lines.sort_unstable();
        s = lines.join("\n");
    }
    s
}

/// Compare one call; `sort`: compare the trace lines in any order (threads).
fn compare(reference: &Path, tool: &Path, args: &[&str], env: &[(&str, &str)], sort: bool) {
    let dir = tempfile::tempdir().expect("temp dir");
    let dir = dir.path().canonicalize().expect("canonical temp dir");
    let a = run(reference, args, &dir, env);
    let b = run(tool, args, &dir, env);
    let n = normalizers(&[reference, tool], &dir);
    let mut report = String::new();
    if a.code != b.code {
        report.push_str(&format!("exit code: {:?} vs {:?}\n", a.code, b.code));
    }
    for (label, x, y, sorted) in [
        ("stdout", &a.stdout, &b.stdout, false),
        ("stderr", &a.stderr, &b.stderr, false),
        ("normal", &a.normal, &b.normal, sort),
        ("perf", &a.perf, &b.perf, sort),
        ("event", &a.event, &b.event, sort),
    ] {
        let (x, y) = (normalize(&n, x, sorted), normalize(&n, y, sorted));
        if x != y {
            report.push_str(&unified_diff(&x, &y, label, label));
        }
    }
    assert!(report.is_empty(), "test-tool {args:?}:\n{report}");
}

const CASES: &[&[&str]] = &[
    &["trace2"],
    &["trace2", "bogus"],
    &["trace2", "-h"],
    &["trace2", "001return", "0"],
    &["trace2", "001return", "1"],
    &["trace2", "001return"],
    &["trace2", "001return", "x"],
    &["trace2", "001return", "-1"],
    &["trace2", "001return", "300"],
    &["trace2", "002exit", "0"],
    &["trace2", "002exit", "5"],
    &["trace2", "003error", "hello world", "this is a test"],
    &["trace2", "003error"],
    &["trace2", "004child"],
    &["trace2", "004child", "nonexistent-command-for-t0210"],
    &["trace2", "004child", "true"],
    &["trace2", "004child", "sh", "-c", "exit 3"],
    &[
        "trace2",
        "006data",
        "test_category",
        "k1",
        "v1",
        "test_category",
        "k2",
        "v2",
    ],
    &["trace2", "006data", "a", "b"],
    &["trace2", "007bug"],
    &["trace2", "008bug"],
    &["trace2", "009bug_BUG"],
    &["trace2", "010bug_BUG"],
    &["trace2", "100timer", "3", "2"],
    &["trace2", "100timer", "2"],
    &["trace2", "101timer", "1", "1"],
    &["trace2", "200counter", "1", "2", "3", "4", "5"],
    &["trace2", "200counter"],
    &["trace2", "200counter", "x"],
    &["trace2", "200counter", "-3"],
    &["trace2", "201counter", "1", "2"],
    &["trace2", "300redact_start"],
    &[
        "trace2",
        "300redact_start",
        "x",
        "clone",
        "https://user:pwd@example.com/",
        "clone2",
    ],
    &[
        "trace2",
        "301redact_child_start",
        "x",
        "clone",
        "https://user:pwd@example.com/",
        "clone2",
    ],
    &[
        "trace2",
        "302redact_exec",
        "x",
        "clone",
        "https://user:pwd@example.com/",
        "clone2",
    ],
    &["trace2", "302redact_exec", "x"],
    &[
        "trace2",
        "303redact_def_param",
        "url",
        "https://user:pwd@example.com/",
    ],
    &["trace2", "303redact_def_param", "a"],
    &["trace2", "400ancestry"],
    &["trace2", "400ancestry", "bogus", "x", "true"],
];

#[test]
fn trace2_matches_reference() {
    let Some(reference) = common::reference_test_tool() else {
        return;
    };
    let tool = Path::new(env!("CARGO_BIN_EXE_rgit-test-tool"));
    for args in CASES {
        compare(&reference, tool, args, &[], false);
    }
    // Threads: the same events, in any order.
    compare(
        &reference,
        tool,
        &["trace2", "101timer", "2", "1", "3"],
        &[],
        true,
    );
    compare(
        &reference,
        tool,
        &["trace2", "201counter", "7", "13", "3"],
        &[],
        true,
    );
    // def_param from trace2.envVars.
    compare(
        &reference,
        tool,
        &["trace2", "001return", "0"],
        &[
            ("GIT_TRACE2_ENV_VARS", "A_VAR,OTHER_VAR,MISSING"),
            ("A_VAR", "1"),
            ("OTHER_VAR", "hello world"),
        ],
        false,
    );
}

/// `400ancestry` gives the child only the chosen target (brief normal
/// format); the child here is a shell that reports its environment.
#[test]
fn ancestry_child_environment() {
    let Some(reference) = common::reference_test_tool() else {
        return;
    };
    let tool = Path::new(env!("CARGO_BIN_EXE_rgit-test-tool"));
    for target in ["normal", "perf", "event"] {
        compare(
            &reference,
            tool,
            &[
                "trace2",
                "400ancestry",
                target,
                "out",
                "sh",
                "-c",
                "env | grep '^GIT_TRACE2' | grep -v PARENT | sort",
            ],
            &[],
            false,
        );
    }
}
