// SPDX-License-Identifier: MIT
//! Differential tests of `log -g` / `reflog show` with log options (#47),
//! the `%g*` placeholders, the `email`/`mboxrd` formats, `--source` and
//! `--show-linear-break` (#405).

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "repo"])
        .git(["-C", "repo", "commit", "-q", "--allow-empty", "-m", "one"])
        .git(["-C", "repo", "commit", "-q", "--allow-empty", "-m", "two"])
        .git(["-C", "repo", "checkout", "-q", "-b", "side", "HEAD~1"])
        .write("repo/f", "ä\nFrom here\n".as_bytes())
        .git(["-C", "repo", "add", "f"])
        .git([
            "-C",
            "repo",
            "commit",
            "-q",
            "-m",
            "thrée\n\nbody\nFrom x\n>From y",
        ])
        .git(["-C", "repo", "checkout", "-q", "-"])
}

fn same(fx: &mut Fixture, args: &[&str]) {
    fx.compare(args).in_dir("repo").checks(Check::OUTPUT).run();
}

#[test]
fn walk_reflogs() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    same(&mut fx, &["log", "-g"]);
    same(&mut fx, &["log", "-g", "--oneline", "side", "HEAD"]);
    same(
        &mut fx,
        &["log", "-g", "--format=%gd|%gD|%gs|%gn|%ge|%h", "HEAD@{1}"],
    );
    same(&mut fx, &["log", "-g", "-1", "--date=raw", "--format=%gd"]);
    same(&mut fx, &["log", "-g", "--format=%gd %s", "HEAD@{now}"]);
    same(&mut fx, &["log", "-g", "--stat", "-1", "side"]);
    same(&mut fx, &["log", "-g", "--reverse"]);
    same(&mut fx, &["log", "-g", "main..side"]);
    same(&mut fx, &["reflog", "--format=short", "-2"]);
    same(&mut fx, &["reflog", "show", "--date=iso", "side"]);
    same(&mut fx, &["rev-list", "-g", "--parents", "-1", "HEAD"]);
}

#[test]
fn mail_formats_source_and_linear_break() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    for f in ["--pretty=email", "--pretty=mboxrd", "--format=email"] {
        same(&mut fx, &["log", f, "--all"]);
    }
    same(&mut fx, &["log", "--pretty=email", "-z", "--stat", "side"]);
    same(&mut fx, &["rev-list", "--pretty=email", "side"]);
    same(&mut fx, &["log", "--oneline", "--source", "--all"]);
    same(
        &mut fx,
        &["log", "--show-linear-break", "--oneline", "--all"],
    );
    same(&mut fx, &["log", "--show-linear-break=***", "--all"]);
    same(&mut fx, &["log", "--show-linear-break", "--graph"]);
    same(&mut fx, &["-c", "log.abbrevCommit=true", "log"]);
    same(&mut fx, &["log", "--oneline", "--reflog"]);
}
