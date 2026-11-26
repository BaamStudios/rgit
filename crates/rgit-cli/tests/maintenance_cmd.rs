// SPDX-License-Identifier: MIT
//! `git maintenance` (run, is-needed, register, unregister, start, stop)
//! and `git for-each-repo` against the reference git.
//!
//! The schedulers never touch the machine: every `start`/`stop` sets
//! `GIT_TEST_MAINT_SCHEDULER` to scripts of the fixture, and `$HOME` is
//! the fixture's own.

use rgit_difftest::{Fixture, Normalizer};

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .git(["config", "maintenance.autoDetach", "false"])
        .write("a", "a\n")
        .git(["add", "a"])
        .git(["commit", "-q", "-m", "c0"])
        .tick()
}

/// Runs every case in a fresh fixture prepared by `prep` with `env`;
/// reports every differing case.
fn cases(prep: impl Fn(Fixture) -> Fixture, env: &[(&str, &str)], cases: &[&[&str]]) {
    let mut failed = Vec::new();
    for args in cases {
        let fx = prep(fixture());
        if fx.is_skipped() {
            return;
        }
        let mut fx = fx;
        let mut cmp = fx
            .compare(*args)
            .env("GIT_TEST_MAINT_SCHEDULER", "none:exit 1")
            .normalize(Normalizer::regex(r"/[^ ']*/state/work", "WORK"));
        for (k, v) in env {
            cmp = cmp.env(k, v);
        }
        if let Err(m) = cmp.try_run() {
            failed.push(m.to_string());
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

#[test]
fn usage_and_errors() {
    cases(
        |fx| fx,
        &[],
        &[
            &["maintenance"],
            &["maintenance", "-h"],
            &["maintenance", "barf"],
            &["maintenance", "run", "-h"],
            &["maintenance", "start", "-h"],
            &["maintenance", "stop", "-h"],
            &["maintenance", "register", "-h"],
            &["maintenance", "unregister", "-h"],
            &["maintenance", "is-needed", "-h"],
            &["maintenance", "run", "--auto", "--schedule=daily"],
            &["maintenance", "run", "--task=gc", "--schedule=daily"],
            &["maintenance", "run", "--schedule=annually"],
            &["maintenance", "run", "--task=bogus"],
            &["maintenance", "run", "--task=gc", "--task=gc"],
            &["maintenance", "run", "extra"],
            &["-c", "maintenance.strategy=unknown", "maintenance", "run"],
            &[
                "-c",
                "maintenance.strategy=unknown",
                "maintenance",
                "is-needed",
            ],
            &["maintenance", "is-needed", "--schedule=daily"],
            &["maintenance", "is-needed", "--task=bogus"],
            &["maintenance", "start", "--scheduler=foo"],
            &["maintenance", "start", "--no-scheduler"],
            &["maintenance", "start", "--scheduler=crontab"],
            &["maintenance", "start"],
            &["maintenance", "unregister"],
            &["maintenance", "unregister", "--force"],
            &["for-each-repo"],
            &["for-each-repo", "-h"],
            &["for-each-repo", "--config=a"],
            &["for-each-repo", "--config=a.b."],
            &[
                "for-each-repo",
                "--config=no.such",
                "--",
                "help",
                "--no-such",
            ],
        ],
    );
}

#[test]
fn outside_a_repository() {
    for sub in [
        "run",
        "stop",
        "start",
        "register",
        "unregister",
        "is-needed",
    ] {
        let mut fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"));
        if fx.is_skipped() {
            return;
        }
        fx.compare(["maintenance", sub])
            .env("GIT_TEST_MAINT_SCHEDULER", "none:exit 1")
            .run();
    }
}

/// `git maintenance <args>`, then the exit code and both configuration
/// files.
const SHOW: &str = "alias.m=!f() { git maintenance \"$@\"; echo rc=$?; \
                    cat \"$HOME/.gitconfig\" .git/config other 2>/dev/null; }; f";

#[test]
fn register_and_unregister() {
    cases(
        |fx| fx,
        &[],
        &[
            &["-c", SHOW, "m", "register"],
            &["-c", SHOW, "m", "register", "--config-file", "other"],
            &["-c", SHOW, "m", "unregister", "--config-file", "other"],
            &[
                "-c",
                SHOW,
                "m",
                "unregister",
                "--config-file",
                "other",
                "--force",
            ],
            &["-c", "maintenance.strategy=gc", "-c", SHOW, "m", "register"],
        ],
    );
    // Registered twice, then unregistered.
    cases(
        |fx| {
            fx.git(["maintenance", "register"])
                .git([
                    "config",
                    "--global",
                    "--add",
                    "maintenance.repo",
                    "/elsewhere",
                ])
                .git(["maintenance", "register"])
        },
        &[],
        &[
            &["-c", SHOW, "m", "unregister"],
            &["-c", SHOW, "m", "register"],
        ],
    );
    // A value-less maintenance.repo.
    cases(
        |fx| {
            let config = fx.work_dir().join(".git/config");
            let mut text = std::fs::read_to_string(&config).unwrap();
            text.push_str("[maintenance]\n\trepo\n");
            std::fs::write(&config, text).unwrap();
            fx
        },
        &[],
        &[
            &["-c", SHOW, "m", "register"],
            &["-c", SHOW, "m", "unregister"],
            &["-c", SHOW, "m", "unregister", "--force"],
        ],
    );
}

/// `is-needed --auto`: the conditions of the tasks.
#[test]
fn auto_conditions() {
    let task = |t: &'static str, cfg: &'static str| -> Vec<String> {
        vec![
            "-c".into(),
            cfg.into(),
            "maintenance".into(),
            "is-needed".into(),
            "--auto".into(),
            format!("--task={t}"),
        ]
    };
    let loose = |fx: Fixture| {
        fx.write("b", "b\n")
            .git(["add", "b"])
            .git(["commit", "-q", "-m", "c1"])
    };
    let mut all: Vec<Vec<String>> = Vec::new();
    for (t, cfgs) in [
        (
            "loose-objects",
            &[
                "maintenance.loose-objects.auto=0",
                "maintenance.loose-objects.auto=-1",
                "maintenance.loose-objects.auto=6",
                "maintenance.loose-objects.auto=7",
            ][..],
        ),
        (
            "commit-graph",
            &[
                "maintenance.commit-graph.auto=0",
                "maintenance.commit-graph.auto=2",
                "maintenance.commit-graph.auto=3",
                "core.commitGraph=false",
            ][..],
        ),
        (
            "worktree-prune",
            &["maintenance.worktree-prune.auto=-1", "x.y=z"][..],
        ),
        (
            "gc",
            &["gc.auto=0", "gc.auto=1", "gc.autoPackLimit=0", "x.y=z"][..],
        ),
        ("pack-refs", &["x.y=z"][..]),
        ("prefetch", &["x.y=z"][..]),
        (
            "incremental-repack",
            &[
                "maintenance.incremental-repack.auto=-1",
                "core.multiPackIndex=false",
                "x.y=z",
            ][..],
        ),
        (
            "reflog-expire",
            &[
                "maintenance.reflog-expire.auto=1",
                "maintenance.reflog-expire.auto=3",
                "maintenance.reflog-expire.auto=4",
            ][..],
        ),
        (
            "geometric-repack",
            &[
                "maintenance.geometric-repack.auto=1",
                "maintenance.geometric-repack.auto=-1",
            ][..],
        ),
        ("rerere-gc", &["maintenance.rerere-gc.auto=-1", "x.y=z"][..]),
    ] {
        for cfg in cfgs {
            all.push(task(t, cfg));
        }
    }
    all.push(vec!["maintenance".into(), "is-needed".into()]);
    all.push(vec![
        "maintenance".into(),
        "is-needed".into(),
        "--auto".into(),
    ]);
    all.push(vec![
        "-c".into(),
        "maintenance.strategy=none".into(),
        "maintenance".into(),
        "is-needed".into(),
    ]);
    let refs: Vec<Vec<&str>> = all
        .iter()
        .map(|a| a.iter().map(String::as_str).collect())
        .collect();
    let refs: Vec<&[&str]> = refs.iter().map(Vec::as_slice).collect();
    cases(loose, &[], &refs);
    // A worktree that is gone.
    cases(
        |fx| fx.write(".git/worktrees/gone", ""),
        &[],
        &[&[
            "maintenance",
            "is-needed",
            "--auto",
            "--task=worktree-prune",
        ]],
    );
}

/// Tasks whose child processes rgit has, run for real.
#[test]
fn run_tasks() {
    let refs = |fx: Fixture| {
        let mut fx = fx;
        for i in 0..20 {
            fx = fx.git(["branch", &format!("b{i}")]);
        }
        fx.write(".git/worktrees/gone", "")
    };
    cases(
        refs,
        &[],
        &[
            &["maintenance", "run", "--task=pack-refs"],
            &["maintenance", "run", "--task=pack-refs", "--auto"],
            &["maintenance", "run", "--task=reflog-expire"],
            &["maintenance", "run", "--task=worktree-prune"],
            &["maintenance", "run", "--task=worktree-prune", "--auto"],
            &["maintenance", "run", "--task=prefetch"],
            &["-c", "maintenance.strategy=none", "maintenance", "run"],
            &[
                "-c",
                "maintenance.strategy=gc",
                "maintenance",
                "run",
                "--schedule=hourly",
            ],
        ],
    );
    // A held lock skips the run.
    cases(
        |fx| fx.write(".git/objects/maintenance.lock", ""),
        &[],
        &[
            &["maintenance", "run", "--task=pack-refs", "--no-quiet"],
            &["maintenance", "run", "--task=pack-refs", "--quiet"],
            &[
                "maintenance",
                "run",
                "--task=pack-refs",
                "--auto",
                "--no-quiet",
            ],
        ],
    );
}

const CRONTAB: &str = "#!/bin/sh\n\
    if test \"$1\" = -l; then cat cron.txt 2>/dev/null; exit 0; fi\n\
    cp \"$1\" cron.txt\n";

const PRINT_ARGS: &str = "#!/bin/sh\nprintf '%s\\n' \"$*\" >>args\n";

fn schedule_normalizers() -> Normalizer {
    Normalizer::new("scheduler minute and binary", |s| {
        let s = regex::Regex::new(r"(?m)^\d+ (1-23|0) ")
            .unwrap()
            .replace_all(s, "M $1 ")
            .into_owned();
        let s = regex::Regex::new(r#""[^"]*" --exec-path="[^"]*""#)
            .unwrap()
            .replace_all(&s, "GIT")
            .into_owned();
        let s = regex::Regex::new(r":\d\d:00")
            .unwrap()
            .replace_all(&s, ":MM:00")
            .into_owned();
        regex::Regex::new(r"ExecStart=.*  for-each-repo")
            .unwrap()
            .replace_all(&s, "ExecStart=GIT for-each-repo")
            .into_owned()
    })
}

#[test]
fn schedulers() {
    let prep = |fx: Fixture| {
        fx.write_executable("crontab", CRONTAB)
            .write_executable("print-args", PRINT_ARGS)
            .write("cron.txt", "Important\n")
    };
    let crontab = "crontab:./crontab";
    let mut failed = Vec::new();
    for (env, args) in [
        (
            crontab,
            &["maintenance", "start", "--scheduler=crontab"][..],
        ),
        (crontab, &["maintenance", "start"][..]),
        (crontab, &["maintenance", "stop"][..]),
        (
            "systemctl:./print-args",
            &["maintenance", "start", "--scheduler=systemd"][..],
        ),
        ("systemctl:./print-args", &["maintenance", "stop"][..]),
        (
            "crontab:false",
            &["maintenance", "start", "--scheduler=crontab"][..],
        ),
        ("systemctl:false", &["maintenance", "start"][..]),
        (
            "crontab:./crontab,systemctl:./print-args",
            &["maintenance", "start"][..],
        ),
    ] {
        let mut fx = prep(fixture());
        if fx.is_skipped() {
            return;
        }
        // XDG_CONFIG_HOME inside the work tree shows the systemd units.
        let xdg = fx.work_dir().join("xdg");
        if let Err(m) = fx
            .compare(args)
            .env("GIT_TEST_MAINT_SCHEDULER", env)
            .env("XDG_CONFIG_HOME", &xdg)
            .normalize(schedule_normalizers())
            .normalize_state(schedule_normalizers())
            .normalize(Normalizer::regex(r"/[^ ']*/state/work", "WORK"))
            .try_run()
        {
            failed.push(m.to_string());
        }
    }
    // A started schedule is replaced and stopped.
    for args in [
        &["maintenance", "start", "--scheduler=crontab"][..],
        &["maintenance", "stop"][..],
    ] {
        let mut fx = prep(fixture())
            .env("GIT_TEST_MAINT_SCHEDULER", crontab)
            .git(["maintenance", "start", "--scheduler=crontab"]);
        if let Err(m) = fx
            .compare(args)
            .normalize(schedule_normalizers())
            .normalize_state(schedule_normalizers())
            .normalize(Normalizer::regex(r"/[^ ']*/state/work", "WORK"))
            .try_run()
        {
            failed.push(m.to_string());
        }
    }
    // A held lock.
    let mut fx = prep(fixture()).write(".git/objects/schedule.lock", "");
    if let Err(m) = fx
        .compare(["maintenance", "start"])
        .env("GIT_TEST_MAINT_SCHEDULER", crontab)
        .normalize(Normalizer::regex(r"/[^ ']*/state/work", "WORK"))
        .try_run()
    {
        failed.push(m.to_string());
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

#[test]
fn for_each_repo() {
    let prep = |fx: Fixture| {
        let one = fx.work_dir().join("one");
        let one = one.to_string_lossy().into_owned();
        fx.git(["init", "-q", "one"])
            .git(["-C", "one", "commit", "-q", "--allow-empty", "-m", "one"])
            .git(["config", "--global", "run.key", &one])
            .git(["config", "--global", "--add", "run.key", "missing"])
            .git(["config", "--global", "--add", "ok.key", &one])
            .git(["config", "--global", "--add", "ok.key", "~/home-repo"])
            .git(["init", "-q", "../home/home-repo"])
    };
    cases(
        prep,
        &[],
        &[
            &["for-each-repo", "--config=ok.key", "log", "--format=%s"],
            &[
                "for-each-repo",
                "--config=ok.key",
                "--",
                "rev-parse",
                "--is-bare-repository",
            ],
            &["for-each-repo", "--config=run.key", "status", "-s"],
            &[
                "for-each-repo",
                "--config=run.key",
                "--keep-going",
                "status",
                "-s",
            ],
            &[
                "for-each-repo",
                "--config=ok.key",
                "--keep-going",
                "--bogus",
            ],
            &[
                "for-each-repo",
                "--config=run.key",
                "commit",
                "--allow-empty",
                "-m",
                "ran",
            ],
        ],
    );
}
