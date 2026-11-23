// SPDX-License-Identifier: MIT
//! `%(is-base:...)` and `%(ahead-behind:...)` of `for-each-ref`, `branch`
//! and `tag` against the reference git, and `tag --contains` over replaced
//! commits that form a cycle (t7004).

use rgit_difftest::Fixture;

/// `main` with a first-parent line, branches forked at different points
/// and a merge.
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .git(["commit", "-q", "--allow-empty", "-m", "m1"])
        .git(["branch", "early"])
        .git(["commit", "-q", "--allow-empty", "-m", "m2"])
        .git(["branch", "mid"])
        .git(["branch", "tie"])
        .git(["commit", "-q", "--allow-empty", "-m", "m3"])
        .git(["switch", "-q", "-c", "topic", "mid"])
        .git(["commit", "-q", "--allow-empty", "-m", "t1"])
        .git(["merge", "-q", "--no-ff", "-m", "merge", "main"])
        .git(["commit", "-q", "--allow-empty", "-m", "t2"])
        .git(["tag", "-a", "-m", "tag", "v1", "mid"])
        .git(["tag", "light", "early"])
        .git(["switch", "-q", "main"])
}

fn all_same(fx: &mut Fixture, cases: &[&[&str]]) {
    let mut failures = Vec::new();
    for args in cases {
        if let Err(m) = fx.compare(*args).try_run() {
            failures.push(m.to_string());
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn is_base() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    all_same(
        &mut fx,
        &[
            &["for-each-ref", "--format=%(refname):%(is-base:topic)"],
            &[
                "for-each-ref",
                "--format=%(refname):%(is-base:topic)",
                "refs/heads/early",
                "refs/heads/tie",
            ],
            &[
                "for-each-ref",
                "--format=%(refname):%(is-base:topic)",
                "--sort=-refname",
                "refs/heads/mid",
                "refs/heads/tie",
            ],
            &[
                "for-each-ref",
                "--format=%(refname)",
                "--sort=refname",
                "--sort=-is-base:topic",
            ],
            &[
                "for-each-ref",
                "--format=[%(is-base:topic)-%(is-base:main)]%(refname)",
            ],
            &["for-each-ref", "--format=%(is-base:nosuch)"],
            &["for-each-ref", "--format=%(is-base:HEAD^{tree})"],
            &["for-each-ref", "--format=%(is-base)"],
            &["for-each-ref", "--format=%(is-base:)"],
            &["refs", "list", "--format=%(refname) %(is-base:topic)"],
            &["tag", "--format=%(refname) %(is-base:topic)"],
            &["branch", "--format=%(refname) %(is-base:topic)"],
        ],
    );
}

#[test]
fn ahead_behind() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    all_same(
        &mut fx,
        &[
            &["for-each-ref", "--format=%(refname) %(ahead-behind:topic)"],
            &["for-each-ref", "--format=%(ahead-behind:HEAD^{tree})"],
            &["for-each-ref", "--format=%(ahead-behind:)"],
        ],
    );
}

/// A commit object that is gone counts and ends its line of history,
/// reported as unreadable.
#[test]
fn ahead_behind_missing_commit() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let id = String::from_utf8(fx.git_stdout(["rev-parse", "main~1"])).unwrap();
    let id = id.trim();
    let mut fx = fx.remove(format!(".git/objects/{}/{}", &id[..2], &id[2..]));
    all_same(
        &mut fx,
        &[&[
            "for-each-ref",
            "--format=%(refname) %(ahead-behind:early)",
            "refs/heads/main",
        ]],
    );
    // How often git repeats the error depends on its walk; the counts
    // are what matters.
    fx.compare([
        "for-each-ref",
        "--format=%(refname) %(ahead-behind:main) %(ahead-behind:early)",
        "refs/heads",
    ])
    .skip(rgit_difftest::Check::Stderr)
    .run();
}

#[test]
fn tag_contains_replaced_cycle() {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .git(["commit", "-q", "--allow-empty", "-m", "one"])
        .git(["commit", "-q", "--allow-empty", "-m", "two"])
        .git(["commit", "-q", "--allow-empty", "-m", "three"]);
    if fx.is_skipped() {
        return;
    }
    let rev = |r: &str| {
        String::from_utf8(fx.git_stdout(["rev-parse", r]))
            .unwrap()
            .trim()
            .to_owned()
    };
    let (first, second, third) = (rev("HEAD~2"), rev("HEAD~1"), rev("HEAD"));
    let mut fx = fx
        .git(["tag", "cycle-a", &first])
        .git(["tag", "cycle-b", &third])
        .git(["tag", "plain", &second]);
    all_same(&mut fx, &[&["tag", "--contains", &second]]);
    let mut fx = fx
        .git(["replace", "--graft", &first, &third, &second])
        .git(["replace", "--graft", &third, &first]);
    all_same(
        &mut fx,
        &[
            &["tag", "--contains", &second, "--list", "cycle-*"],
            &["tag", "--no-contains", &second],
            &["--no-replace-objects", "tag", "--contains", &second],
        ],
    );
}

/// A bare repository is the main worktree of its `HEAD` branch.
#[test]
fn worktreepath_in_bare_repository() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "--bare", "-b", "main"])
        .git([
            "--work-tree=.",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "one",
        ])
        .git(["branch", "other"]);
    if fx.is_skipped() {
        return;
    }
    all_same(
        &mut fx,
        &[&["for-each-ref", "--format=%(refname) [%(worktreepath)]"]],
    );
}
