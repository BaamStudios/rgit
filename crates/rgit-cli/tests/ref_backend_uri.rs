// SPDX-License-Identifier: MIT
//! Differential tests of references kept outside the git directory
//! (`extensions.refStorage = <format>://<dir>`, `GIT_REFERENCE_BACKEND`,
//! #621) against the reference git.

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"))
}

#[test]
fn init_and_use_an_alternate_ref_directory() {
    for format in ["files", "reftable"] {
        let mut fx = fixture();
        if fx.is_skipped() {
            return;
        }
        // The directory must exist.
        fx.compare(["init", "-q", "repo"])
            .env("GIT_REFERENCE_BACKEND", format!("{format}://missing"))
            .checks(Check::OUTPUT)
            .run();
        fx = fx.write("refdir/.keep", "");
        let uri = format!("{format}://../../refdir");
        // Without the work tree: reftable names its tables at random.
        let checks: Vec<Check> = Check::ALL
            .into_iter()
            .filter(|c| format == "files" || *c != Check::Worktree)
            .collect();
        fx.compare(["init", "-q", "repo"])
            .env("GIT_REFERENCE_BACKEND", &uri)
            .checks(checks.clone())
            .run();
        fx.compare(["-C", "repo", "config", "get", "extensions.refstorage"])
            .checks(Check::OUTPUT)
            .run();
        for f in ["repo/.git/HEAD", "repo/.git/refs/heads"] {
            let text = std::fs::read_to_string(fx.work_dir().join(f)).unwrap();
            assert!(
                text == "ref: refs/heads/.invalid\n"
                    || text == "repository uses alternate refs storage\n",
                "{format} {f}: {text}"
            );
        }
        fx = fx
            .write("repo/a", "a\n")
            .git(["-C", "repo", "add", "a"])
            .tick()
            .git(["-C", "repo", "commit", "-q", "-m", "one"]);
        fx.compare(["-C", "repo", "tag", "t1"])
            .checks(checks.clone())
            .run();
        fx.compare(["-C", "repo", "for-each-ref"])
            .checks(Check::OUTPUT)
            .run();
        fx.compare(["-C", "repo", "rev-parse", "HEAD", "t1"])
            .checks(Check::OUTPUT)
            .run();
    }
}

#[test]
fn unknown_backend_in_the_environment() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    fx = fx.git(["init", "-q", "repo"]);
    for value in ["reftable@/home/reftable", "reftable:", "db://.git"] {
        fx.compare(["-C", "repo", "refs", "list"])
            .env("GIT_REFERENCE_BACKEND", value)
            .checks(Check::OUTPUT)
            .run();
    }
}
