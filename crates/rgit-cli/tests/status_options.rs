// SPDX-License-Identifier: MIT
//! Differential tests of the long-format options of `rgit status`:
//! `-v`/`-vv`, `--show-stash`/`status.showStash`,
//! `status.displayCommentPrefix`, boolean `-u` values, `status.branch`
//! with `--porcelain`, the untracked-files delay advice and the index
//! refresh (with and without `--no-optional-locks`).

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a", "a\n")
        .write("b", "b\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .tick()
        .write("a", "stashed\n")
        .git(["stash", "-q"])
        .write("a", "a2\n")
        .write("c", "c\n")
        .git(["add", "c"])
        .write("b", "b2\n")
        .write("new", "n\n")
}

#[test]
fn long_format_options() {
    for args in [
        &["status", "-v"][..],
        &["status", "-vv"],
        &["status", "-v", "-s"],
        &["status", "--show-stash"],
        &["status", "--show-stash", "--no-show-stash"],
        &["-c", "status.showStash=true", "status"],
        &["-c", "status.showStash=true", "status", "--no-show-stash"],
        &["status", "--show-stash", "--porcelain=v2"],
        &["status", "--show-stash", "--porcelain=v2", "--branch"],
        &["status", "--show-stash", "-s"],
        &["-c", "status.displayCommentPrefix=true", "status"],
        &[
            "-c",
            "status.displayCommentPrefix=true",
            "-c",
            "core.commentChar=;",
            "status",
        ],
        &["status", "-ufalse"],
        &["status", "-utrue"],
        &["status", "-ubogus"],
        &["-c", "status.branch=true", "status", "--porcelain"],
        &["-c", "status.branch=true", "status", "-s"],
    ] {
        let mut fx = fixture();
        if fx.is_skipped() {
            return;
        }
        fx.compare(args).checks(Check::OUTPUT).run();
    }
    // No stash: nothing about it.
    let mut fx = fixture().git(["stash", "clear"]);
    fx.compare(["status", "--show-stash"])
        .checks(Check::OUTPUT)
        .run();
    // The delay advice (GIT_TEST_UF_DELAY_WARNING pretends a slow walk).
    for cfg in [
        &["-c", "advice.statusUoption=true"][..],
        &["-c", "advice.statusUoption=false"],
        // (Not with core.fsmonitor=true: the reference git would start
        // its fsmonitor daemon.)
        &[
            "-c",
            "advice.statusUoption=true",
            "-c",
            "core.untrackedCache=true",
        ],
    ] {
        let mut fx = fixture();
        let mut args = cfg.to_vec();
        args.push("status");
        fx.compare(&args)
            .env("GIT_TEST_UF_DELAY_WARNING", "1")
            .checks(Check::OUTPUT)
            .run();
        let mut args = cfg.to_vec();
        args.extend(["status", "-uno"]);
        fx.compare(&args)
            .env("GIT_TEST_UF_DELAY_WARNING", "1")
            .checks(Check::OUTPUT)
            .run();
    }
}

/// `status` writes the refreshed index (a touched file is no longer
/// stat-dirty for `diff-files`); `--no-optional-locks` leaves it alone.
#[test]
fn refreshes_the_index() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let touch = |fx: &Fixture| {
        let path = fx.work_dir().join("c");
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(path)
            .and_then(|f| f.set_modified(later))
            .expect("touch c");
    };
    for args in [&["status"][..], &["--no-optional-locks", "status"]] {
        let mut fx = fixture();
        touch(&fx);
        fx.compare(args).checks(Check::OUTPUT).run();
        let mut fx = fixture();
        touch(&fx);
        let mut all = args.to_vec();
        all.extend(["-s"]);
        fx.compare(&all)
            .checks([Check::ExitCode, Check::Stdout, Check::Stderr, Check::Index])
            .run();
    }
}
