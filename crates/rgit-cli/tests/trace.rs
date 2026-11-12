// SPDX-License-Identifier: MIT
//! Differential tests of `GIT_TRACE` / `GIT_TRACE_PERFORMANCE` lines written
//! by the binary, and of invalid `-c` settings (#177, #73).

use rgit_difftest::{Check, Fixture, Normalizer};

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "repo"])
}

fn same(fx: &mut Fixture, env: &[(&str, &str)], args: &[&str]) {
    let mut c = fx
        .compare(args)
        .in_dir("repo")
        .checks(Check::OUTPUT)
        .env("GIT_TRACE_BARE", "1")
        // Timing and the program path differ by nature.
        .normalize_stderr(Normalizer::regex(
            r"(?m)^performance: [0-9.]+ s: git command: \S+",
            "performance: <t> s: git command: <git>",
        ));
    for (k, v) in env {
        c = c.env(k, v);
    }
    c.run();
}

#[test]
fn builtin_and_performance_lines() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let trace = [("GIT_TRACE", "1"), ("GIT_TRACE_PERFORMANCE", "2")];
    same(&mut fx, &trace, &["symbolic-ref", "HEAD"]);
    same(
        &mut fx,
        &trace,
        &["symbolic-ref", "--short", "HEAD", "it's"],
    );
    same(
        &mut fx,
        &trace,
        &["-C", ".", "-c", "a.b=c", "rev-parse", "--git-dir"],
    );
    same(&mut fx, &trace, &["config", "get", "no.such"]);
    same(
        &mut fx,
        &[("GIT_TRACE", "1")],
        &["check-ref-format", "refs/heads/x"],
    );
}

#[test]
fn invalid_command_line_config() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    for spec in ["foo", "a.b.=c", "=x", "include.path=x"] {
        same(&mut fx, &[], &["-c", spec, "rev-parse", "--git-dir"]);
        same(&mut fx, &[], &["-c", spec, "symbolic-ref", "HEAD"]);
    }
    // Commands that never read the configuration do not notice.
    same(
        &mut fx,
        &[],
        &["-c", "foo", "check-ref-format", "refs/heads/x"],
    );
    same(
        &mut fx,
        &[("GIT_CONFIG_PARAMETERS", "'foo'")],
        &["rev-parse", "--git-dir"],
    );
}
