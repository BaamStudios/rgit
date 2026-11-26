// SPDX-License-Identifier: MIT
//! Command forms Gitea runs on the server (issue #71), against the
//! reference git: `update-server-info` after every repository creation and
//! push, and `log -n 100` passed as one argument by the commit graph view.

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"))
}

/// Branches, an annotated tag, a tag of a tag, a lightweight tag, one pack
/// and loose objects.
fn repo() -> Fixture {
    let mut fx = fixture()
        .git(["init", "-q"])
        .git(["config", "maintenance.auto", "false"]);
    for c in 0..3 {
        fx = fx
            .write("a.txt", format!("a {c}\n"))
            .git(["add", "a.txt"])
            .git(["commit", "-q", "-m", &format!("c{c}")])
            .tick();
    }
    fx.git(["tag", "-a", "-m", "one", "v1", "HEAD~1"])
        .git(["tag", "-a", "-m", "nested", "v1-outer", "v1"])
        .git(["tag", "light", "HEAD~2"])
        .git(["branch", "side", "HEAD~1"])
        .git(["repack", "-adq"])
        .write("b.txt", "loose\n")
        .git(["add", "b.txt"])
        .git(["commit", "-q", "-m", "loose"])
}

const INFO: [&str; 2] = ["info/refs", "objects/info/packs"];

fn compare_info(fx: &mut Fixture, args: &[&str]) {
    let mut cmp = fx.compare(args).checks(Check::ALL);
    for f in INFO {
        cmp = cmp.git_file(f);
    }
    cmp.run();
}

#[test]
fn update_server_info_matches_git() {
    let mut fx = repo();
    if fx.is_skipped() {
        return;
    }
    compare_info(&mut fx, &["update-server-info"]);
    compare_info(&mut fx, &["update-server-info", "-f"]);
    compare_info(&mut fx, &["update-server-info", "--force"]);
    // After a ref is deleted and a pack added, the files are rewritten.
    let mut fx = fx.git(["branch", "-D", "side"]).git(["repack", "-dq"]);
    compare_info(&mut fx, &["update-server-info"]);
}

#[test]
fn update_server_info_in_a_bare_repository() {
    let fx = repo();
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx.git(["clone", "-q", "--bare", ".", "bare.git"]);
    let mut cmp = fx
        .compare(["update-server-info"])
        .in_dir("bare.git")
        .repo("bare.git")
        .checks(Check::ALL);
    for f in INFO {
        cmp = cmp.git_file(f);
    }
    cmp.run();
}

#[test]
fn update_server_info_errors_match_git() {
    let mut fx = repo();
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["update-server-info", "extra"][..],
        &["update-server-info", "--bogus"],
        &["update-server-info", "-h"],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
    let mut outside = fixture();
    if outside.is_skipped() {
        return;
    }
    outside
        .compare(["update-server-info"])
        .checks([Check::ExitCode])
        .run();
}

#[test]
fn max_count_accepts_leading_white_space_like_strtol() {
    let mut fx = repo();
    if fx.is_skipped() {
        return;
    }
    for args in [
        &["log", "--oneline", "-n 2"][..],
        &["log", "--oneline", "-n\t1"],
        &["log", "--oneline", "-n+1"],
        &["log", "--oneline", "-n -1"],
        &["log", "--oneline", "--max-count= 1"],
        &["log", "--oneline", "--skip= 2"],
        &["log", "--oneline", "-n 1 "],
        &["log", "--oneline", "--max-count=0x1"],
        &["rev-list", "--max-parents= 1", "HEAD"],
        &["rev-list", "--min-parents= 1", "HEAD"],
        // The form of Gitea's commit graph (routers/web/repo/commit.go).
        &[
            "log",
            "--graph",
            "--date-order",
            "--decorate=full",
            "--tags",
            "--branches",
            "-C",
            "-M",
            "--date=iso-strict",
            "-n 100",
            "--pretty=format:DATA:%D|%H|%ad|%h|%s",
        ],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
}
