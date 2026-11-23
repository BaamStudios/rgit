// SPDX-License-Identifier: MIT
//! Differential tests of the trace2 normal and perf streams written to
//! standard error (#180): the event sequence of simple commands, child
//! processes of hooks and aliases, `def_param` and `def_repo`.

use rgit_difftest::{Check, Fixture, Normalizer};

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "repo"])
}

/// Fields that differ by nature: version, program path, timings, pids,
/// process ancestry.
fn normalize(c: rgit_difftest::Comparison<'_>) -> rgit_difftest::Comparison<'_> {
    c.normalize_stderr(Normalizer::regex(
        r"(?m)^(d0 \| main +\| )?cmd_ancestry.*\n",
        "",
    ))
    .normalize_stderr(Normalizer::regex(r"(?m)^version .*$", "version <v>"))
    .normalize_stderr(Normalizer::regex(r"(?m)\| 2\.56\.\S*$", "| <v>"))
    .normalize_stderr(Normalizer::regex(
        r"(?m)(start|\|) /\S+/(git|rgit) ",
        "$1 <git> ",
    ))
    .normalize_stderr(Normalizer::regex(r"[0-9]+\.[0-9]{6}", "<t>"))
    .normalize_stderr(Normalizer::regex(r"pid:[0-9]+", "pid:<p>"))
}

fn same(fx: &mut Fixture, env: &[(&str, &str)], args: &[&str]) {
    let mut c = fx
        .compare(args)
        .in_dir("repo")
        .checks(Check::OUTPUT)
        .env("GIT_TRACE2_EVENT_NESTING", "100");
    for (k, v) in env {
        c = c.env(k, v);
    }
    normalize(c).run();
}

#[test]
fn normal_and_perf_streams() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let normal = [("GIT_TRACE2", "2"), ("GIT_TRACE2_BRIEF", "1")];
    let perf = [("GIT_TRACE2_PERF", "2"), ("GIT_TRACE2_PERF_BRIEF", "1")];
    for env in [&normal[..], &perf[..]] {
        same(&mut fx, env, &["symbolic-ref", "HEAD"]);
        same(&mut fx, env, &["rev-parse", "--git-dir"]);
        same(&mut fx, env, &["check-ref-format", "refs/heads/x"]);
    }
    let mut params = normal.to_vec();
    params.push(("GIT_TRACE2_CONFIG_PARAMS", "a.*"));
    params.push(("GIT_TRACE2_ENV_VARS", "GIT_TRACE2_BRIEF"));
    same(&mut fx, &params, &["-c", "a.b=c", "symbolic-ref", "HEAD"]);
}

#[test]
fn hook_child_process() {
    let mut fx = fixture()
        .git(["-C", "repo", "config", "hook.h.event", "test-hook"])
        .git(["-C", "repo", "config", "hook.h.command", "true"]);
    if fx.is_skipped() {
        return;
    }
    same(
        &mut fx,
        &[("GIT_TRACE2_PERF", "2"), ("GIT_TRACE2_PERF_BRIEF", "1")],
        &["hook", "run", "--allow-unknown-hook-name", "test-hook"],
    );
}
