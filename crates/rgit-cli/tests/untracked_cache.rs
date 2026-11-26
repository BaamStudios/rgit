// SPDX-License-Identifier: MIT
//! The untracked cache (`UNTR`) between rgit and the reference git: a cache
//! filled by one is used by the other without reading any directory again,
//! and both list the same untracked files.

use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

use rgit_difftest::Fixture;

/// Runs rgit in the fixture's work tree with its isolated configuration.
fn rgit(fx: &Fixture, args: &[&str], trace: Option<&Path>) -> Output {
    let home = fx.home_dir();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rgit"));
    cmd.args(args)
        .current_dir(fx.work_dir())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    if let Some(t) = trace {
        cmd.env("GIT_TRACE2_PERF", t);
    }
    cmd.output().expect("run rgit")
}

/// The untracked cache counters of a trace2 perf file.
fn counters(trace: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(trace).unwrap_or_default();
    let _ = std::fs::remove_file(trace);
    text.lines()
        .filter(|l| l.contains("| data ") && l.contains("read_directo"))
        .filter_map(|l| l.rsplit('|').next())
        .map(|f| f.trim().trim_start_matches('.').to_string())
        .filter(|f| !f.contains("visited"))
        .collect()
}

fn age(fx: &Fixture, paths: &[&str]) {
    let old = SystemTime::now() - Duration::from_secs(300);
    for p in paths {
        let f = std::fs::File::open(fx.work_dir().join(p)).unwrap();
        f.set_modified(old).unwrap();
    }
}

fn setup() -> Option<Fixture> {
    let fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"));
    if fx.is_skipped() {
        return None;
    }
    let fx = fx
        .git(["init", "-q"])
        .write("one", "")
        .write("two", "")
        .write("three", "")
        .write("done/one", "")
        .write("dtwo/two", "")
        .write("dthree/three", "")
        .write("dthree/sub/four", "")
        .write("ignored/x", "")
        .write(".gitignore", "ignored/\n*.o\n")
        .write("dtwo/a.o", "");
    age(
        &fx,
        &[
            "one",
            "two",
            "three",
            "done/one",
            "dtwo/two",
            "dthree/three",
            "dthree/sub/four",
            "dthree/sub",
            "done",
            "dtwo",
            "dthree",
            "ignored",
            ".gitignore",
            "dtwo/a.o",
            ".",
        ],
    );
    Some(
        fx.git(["add", "one", "two", "done/one"])
            .git(["update-index", "--untracked-cache"]),
    )
}

fn status_of(o: &Output) -> String {
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout.clone()).unwrap()
}

#[test]
fn rgit_fills_git_uses() {
    for all in [false, true] {
        let Some(fx) = setup() else { return };
        let u = if all { "-uall" } else { "-unormal" };
        let fx = if all {
            fx.git(["config", "status.showUntrackedFiles", "all"])
        } else {
            fx
        };
        let ours = status_of(&rgit(&fx, &["status", "--porcelain", u], None));
        let theirs = String::from_utf8(fx.git_stdout(["status", "--porcelain", u])).unwrap();
        assert_eq!(ours, theirs, "{u}");
        // git takes everything from rgit's cache.
        let trace = fx.work_dir().join("../perf");
        let fx = fx.env("GIT_TRACE2_PERF", &trace);
        let _ = fx.git_output(["status", "--porcelain", u]);
        assert_eq!(
            counters(&trace),
            [
                "path:",
                "node-creation:0",
                "gitignore-invalidation:0",
                "directory-invalidation:0",
                "opendir:0"
            ],
            "{u}"
        );
    }
}

#[test]
fn git_fills_rgit_uses() {
    let Some(fx) = setup() else { return };
    let theirs = String::from_utf8(fx.git_stdout(["status", "--porcelain"])).unwrap();
    let trace = fx.work_dir().join("../perf");
    let ours = status_of(&rgit(&fx, &["status", "--porcelain"], Some(&trace)));
    assert_eq!(ours, theirs);
    assert_eq!(
        counters(&trace),
        [
            "path:",
            "node-creation:0",
            "gitignore-invalidation:0",
            "directory-invalidation:0",
            "opendir:0"
        ]
    );
    // Adding a file invalidates its directories; both see the change.
    let fx = fx.write("done/new", "");
    let _ = rgit(&fx, &["add", "dtwo/two"], None);
    let ours = status_of(&rgit(&fx, &["status", "--porcelain"], None));
    let theirs = String::from_utf8(fx.git_stdout(["status", "--porcelain"])).unwrap();
    assert_eq!(ours, theirs);
}

#[test]
fn update_index_options() {
    for (config, args) in [
        ("false", &["update-index", "--untracked-cache"][..]),
        ("true", &["update-index", "--no-untracked-cache"]),
        ("keep", &["update-index", "--force-untracked-cache"]),
        ("keep", &["update-index", "--no-untracked-cache"]),
    ] {
        let Some(fx) = setup() else { return };
        let mut fx = fx.git(["config", "core.untrackedCache", config]);
        fx.compare(args)
            .checks([
                rgit_difftest::Check::ExitCode,
                rgit_difftest::Check::Stdout,
                rgit_difftest::Check::Stderr,
                rgit_difftest::Check::Index,
            ])
            .run();
    }
    let Some(fx) = setup() else { return };
    let o = rgit(&fx, &["update-index", "--test-untracked-cache"], None);
    assert!(o.status.success());
    let err = String::from_utf8(o.stderr).unwrap();
    assert!(
        err.starts_with("Testing mtime in '") && err.ends_with(" OK\n"),
        "{err}"
    );
}
