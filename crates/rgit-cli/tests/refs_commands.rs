// SPDX-License-Identifier: MIT
//! `update-ref`, `show-ref`, `for-each-ref` and `pack-refs` against the
//! reference git: output, exit code, references and reflogs afterwards.

use rgit_difftest::{Check, Fixture};

const CHECKS: [Check; 5] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Refs,
    Check::Reflogs,
];

/// A repository with branches, lightweight and annotated (also nested)
/// tags, a remote-tracking symref, packed and loose references.
fn fixture() -> Option<Fixture> {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .git(["commit", "-q", "--allow-empty", "-m", "a"])
        .git(["tag", "v1"])
        .git(["tag", "-a", "-m", "t", "v2"])
        .git(["branch", "b/x"])
        .git(["update-ref", "refs/remotes/o/main", "HEAD"])
        .git(["symbolic-ref", "refs/remotes/o/HEAD", "refs/remotes/o/main"])
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "b"])
        .git(["tag", "-a", "-m", "t2", "v3"])
        .git([
            "-c",
            "advice.nestedTag=false",
            "tag",
            "-a",
            "-m",
            "n",
            "v4",
            "v3",
        ])
        .git(["pack-refs", "--all"])
        .git(["branch", "c"]);
    (!fx.is_skipped()).then_some(fx)
}

fn run_cases(fx: &mut Fixture, cases: &[&[&str]]) {
    for args in cases {
        fx.compare(*args).checks(CHECKS).run();
    }
}

fn run_stdin(fx: &mut Fixture, args: &[&str], input: &str) {
    fx.compare(args)
        .stdin(input.as_bytes().to_vec())
        .checks(CHECKS)
        .run();
}

#[test]
fn show_ref() {
    let Some(mut fx) = fixture() else { return };
    run_cases(
        &mut fx,
        &[
            &["show-ref"],
            &["show-ref", "--head", "-d"],
            &["show-ref", "master", "x"],
            &["show-ref", "--head", "master"],
            &["show-ref", "nope"],
            &["show-ref", "-q", "master"],
            &["show-ref", "--hash=4", "-d"],
            &["show-ref", "-s", "--abbrev", "v4"],
            &["show-ref", "--abbrev=2", "v1"],
            &["show-ref", "--hash=0", "v1"],
            &["show-ref", "--tags", "--branches"],
            &["show-ref", "--heads"],
            &["show-ref", "--verify", "HEAD", "refs/heads/master"],
            &["show-ref", "--verify", "-d", "refs/tags/v2", "HEAD"],
            &["show-ref", "--verify", "master"],
            &["show-ref", "--verify", "-q", "master"],
            &["show-ref", "--verify"],
            &["show-ref", "--exists", "HEAD"],
            &["show-ref", "--exists", "refs/heads/nope"],
            &["show-ref", "--exists", "refs/heads"],
            &["show-ref", "--exists"],
            &["show-ref", "--exists", "a", "b"],
            &["show-ref", "--exists", "--verify", "HEAD"],
            &["show-ref", "-s=4"],
            &["show-ref", "--bogus"],
            &["show-ref", "-h"],
        ],
    );
    run_stdin(
        &mut fx,
        &["show-ref", "--exclude-existing"],
        "aaa refs/heads/master\nbbb refs/heads/new\nrefs/tags/v9\nccc refs/tags/v9^{}\n",
    );
    run_stdin(
        &mut fx,
        &["show-ref", "--exclude-existing=refs/tags"],
        "aaa refs/heads/master\nrefs/tags/v9\n",
    );
}

#[test]
fn show_ref_bad_refs() {
    let Some(fx) = fixture() else { return };
    let mut fx = fx
        .write(
            ".git/refs/heads/missing",
            "1111111111111111111111111111111111111111\n",
        )
        .write(".git/refs/heads/zbroken", "garbage\n");
    run_cases(
        &mut fx,
        &[
            &["show-ref", "--verify", "refs/heads/missing"],
            &["show-ref", "--verify", "refs/heads/zbroken"],
            &["show-ref", "--exists", "refs/heads/zbroken"],
        ],
    );
    fx.compare(["show-ref", "refs/heads/zbroken"])
        .checks(CHECKS)
        .run();
}

#[test]
fn for_each_ref() {
    let Some(mut fx) = fixture() else { return };
    run_cases(
        &mut fx,
        &[
            &["for-each-ref"],
            &["for-each-ref", "refs/heads", "refs/tags/v"],
            &["for-each-ref", "refs/tags/v[23]", "refs/remotes/"],
            &[
                "for-each-ref",
                "--format=%(refname) %(refname:short) %(objectname:short) \
                 %(objectname:short=10) %(objecttype) [%(HEAD)] <%(symref)> <%(symref:short)>",
            ],
            &[
                "for-each-ref",
                "--format=%(refname:strip=2) %(refname:lstrip=-1) %(refname:rstrip=1) \
                 %(*objectname) %(*objecttype)|%%|%41%(refname:lstrip=1)",
            ],
            &["for-each-ref", "--sort=-refname", "--count=3"],
            &[
                "for-each-ref",
                "--sort=objectname",
                "--format=%(objectname) %(refname)",
            ],
            &[
                "for-each-ref",
                "--sort=-objecttype",
                "--sort=refname",
                "--format=%(objecttype) %(refname)",
            ],
            &["for-each-ref", "--format=%(foo)"],
            &["for-each-ref", "--format=%(refname"],
            &[
                "for-each-ref",
                "--shell",
                "--format=%(refname) %(objecttype)",
            ],
            &[
                "for-each-ref",
                "--perl",
                "--format=%(refname)",
                "refs/heads",
            ],
            &[
                "for-each-ref",
                "--python",
                "--format=%(refname)",
                "refs/heads",
            ],
            &["for-each-ref", "--tcl", "--format=%(refname)", "refs/heads"],
            &[
                "for-each-ref",
                "--include-root-refs",
                "--format=%(refname) %(HEAD)",
            ],
            &[
                "for-each-ref",
                "--exclude=refs/tags",
                "--exclude=refs/heads/b*",
                "--format=%(refname)",
            ],
            &["for-each-ref", "--points-at=HEAD", "--format=%(refname)"],
            &["for-each-ref", "--count=-1"],
            &["for-each-ref", "--format="],
            &[
                "for-each-ref",
                "--format=%(objectsize) %(refname:short)",
                "refs/tags",
            ],
            &["for-each-ref", "--sort=bogus"],
            &["for-each-ref", "--format=%(refname:bogus)"],
            &["for-each-ref", "--format=%(HEAD:x)"],
            &[
                "for-each-ref",
                "--start-after=refs/heads/c",
                "--format=%(refname)",
            ],
            &["for-each-ref", "-h"],
        ],
    );
}

#[test]
fn update_ref_command_line() {
    let Some(mut fx) = fixture() else { return };
    run_cases(
        &mut fx,
        &[
            &["update-ref"],
            &["update-ref", "refs/heads/n1"],
            &["update-ref", "refs/heads/n1", "nope"],
            &["update-ref", "refs/heads/n1", "master", "nope"],
            &["update-ref", "refs/heads/n1", "master", ""],
            &["update-ref", "refs/heads/c", "master", ""],
            &["update-ref", "refs/heads/c", "b/x", "b/x"],
            &["update-ref", "-m", "", "refs/heads/n1", "master"],
            &["update-ref", "-m", "a\nb", "refs/heads/n1", "master"],
            &["update-ref", "refs/heads/n1", "master", "b/x", "a"],
            &["update-ref", "-d"],
            &["update-ref", "-d", "refs/heads/c", "nope"],
            &["update-ref", "-d", "refs/heads/c", "b/x"],
            &["update-ref", "-d", "refs/heads/c", "master"],
            &["update-ref", "-d", "refs/heads/b/x"],
            &["update-ref", "-d", "refs/heads/nope"],
            &[
                "update-ref",
                "refs/heads/n2",
                "1111111111111111111111111111111111111111",
            ],
            &["update-ref", "refs/heads/n2", "HEAD^{tree}"],
            &["update-ref", "refs/tags/t1", "HEAD^{tree}"],
            &["update-ref", "refs/heads/bad..name", "HEAD"],
            &["update-ref", "HEAD", "b/x"],
            &["update-ref", "--no-deref", "HEAD", "b/x"],
            &[
                "update-ref",
                "-m",
                "msg",
                "--create-reflog",
                "refs/tags/t2",
                "HEAD~1",
            ],
            &["update-ref", "refs/heads/b", "HEAD"],
            &["update-ref", "refs/heads/c/d", "HEAD"],
            &["update-ref", "refs/remotes/o/HEAD", "v2^0"],
            &["update-ref", "--stdin", "x"],
            &["update-ref", "-z", "x", "y"],
        ],
    );
}

#[test]
fn update_ref_stdin() {
    let Some(mut fx) = fixture() else { return };
    let text: &[&str] = &[
        "update refs/heads/s1 HEAD\n",
        "update refs/heads/s1\n",
        "update refs/heads/c \n",
        "update refs/heads/s1 HEAD b/x\n",
        "update refs/heads/s1 HEAD HEAD extra\n",
        "create refs/heads/s1 HEAD\n",
        "create refs/heads/c HEAD\n",
        "create refs/heads/s2\n",
        "create refs/heads/s2 0000000000000000000000000000000000000000\n",
        "create refs/heads/s2 nope\n",
        "create \n",
        "delete refs/heads/c\n",
        "delete refs/heads/c 0000000000000000000000000000000000000000\n",
        "verify refs/heads/c HEAD\n",
        "verify refs/heads/c b/x\n",
        "verify refs/heads/zz\n",
        "bogus x\n",
        " update x\n",
        "\n",
        "update\n",
        "option no-deref\nupdate HEAD b/x\n",
        "option bogus\n",
        "update \"refs/heads/q\" \"master\"\n",
        "update \"refs/heads/q\"x master\n",
        "update \"refs/heads/q master\n",
        "create refs/heads/q \"ma\\zn\"\n",
        "update refs/heads/s3 HEAD\nupdate refs/heads/s3 HEAD\n",
        "start\nupdate refs/heads/s4 HEAD\nprepare\ncommit\n",
        "start\nupdate refs/heads/s5 HEAD\nprepare\nupdate refs/heads/s6 HEAD\n",
        "start\nupdate refs/heads/s5 HEAD\nabort\n",
        "start\nupdate refs/heads/s5 HEAD\n",
        "update refs/heads/s5 HEAD\ncommit\nupdate refs/heads/s6 HEAD\n",
        "update refs/heads/s5 HEAD\ncommit\nstart\nupdate refs/heads/s6 HEAD\ncommit\n",
        "commit\ncommit\n",
        "update refs/heads/s7 HEAD",
        "update refs/heads/c HEAD\nupdate refs/heads/b HEAD\n",
        "symref-update refs/heads/sy refs/heads/master\n",
        "symref-update refs/remotes/o/HEAD refs/heads/b/x ref refs/heads/master\n",
        "symref-update refs/remotes/o/HEAD refs/heads/b/x ref refs/remotes/o/main\n",
        "symref-update refs/heads/c refs/heads/b/x foo bar\n",
        "symref-update refs/heads/c refs/heads/b/x oid nope\n",
        "symref-verify refs/remotes/o/HEAD refs/remotes/o/main\n",
        "option no-deref\nsymref-verify refs/remotes/o/HEAD refs/heads/b/x\n",
        "option no-deref\nsymref-verify refs/remotes/o/HEAD refs/remotes/o/main\n",
        "option no-deref\nsymref-delete refs/remotes/o/HEAD refs/remotes/o/main\n",
        "symref-create refs/heads/sy2 refs/heads/master\n",
        "symref-create refs/remotes/o/HEAD refs/heads/master\n",
    ];
    for input in text {
        run_stdin(&mut fx, &["update-ref", "--stdin"], input);
    }
    run_stdin(
        &mut fx,
        &["update-ref", "-m", "why", "--create-reflog", "--stdin"],
        "update refs/tags/new HEAD\ndelete refs/heads/b/x\n",
    );
    let nul: &[&str] = &[
        "update refs/heads/z1\0HEAD\0\0",
        "update refs/heads/c\0\0\0",
        "update refs/heads/z2\0HEAD\0",
        "update refs/heads/z2\0HEAD",
        "update refs/heads/z2",
        "create refs/heads/z3\0HEAD\0",
        "create refs/heads/z3",
        "delete refs/heads/c\0\0",
        "delete refs/heads/c",
        "verify refs/heads/zz\0\0",
        "update refs/heads/z1\0HEAD\0\0extra\0",
        "update \0",
        "symref-update refs/heads/zz2\0refs/heads/master\0",
        "option no-deref\0symref-delete refs/heads/z1\0",
    ];
    for input in [
        "start\ncreate refs/heads/restart HEAD\nstart\ncommit\n",
        "create refs/heads/restart HEAD\nstart\ncommit\n",
        "option no-deref\nsymref-update refs/heads/restart refs/heads/master\n",
    ] {
        run_stdin(&mut fx, &["update-ref", "--stdin"], input);
    }
    for input in nul {
        run_stdin(&mut fx, &["update-ref", "-z", "--stdin"], input);
    }
}

/// An update whose new value equals the old one (#257): compare the ref and
/// the reflogs git writes.
#[test]
fn update_ref_same_value_reflog() {
    let Some(fx) = fixture() else { return };
    let mut fx = fx.tick();
    let cases: &[&[&str]] = &[
        &["update-ref", "-m", "same", "refs/heads/main", "HEAD"],
        &["update-ref", "-m", "same", "HEAD", "HEAD"],
        &["update-ref", "-m", "same", "refs/heads/b/x", "b/x", "b/x"],
    ];
    for args in cases {
        fx.compare(*args)
            .checks(CHECKS.into_iter().chain([Check::GitFiles]))
            .git_file("logs/HEAD")
            .git_file("logs/refs/heads/main")
            .git_file("logs/refs/heads/b/x")
            .git_file("refs/heads/main")
            .run();
    }
}

#[test]
fn pack_refs() {
    let Some(fx) = fixture() else { return };
    let mut fx = fx
        .git(["branch", "a/b/c"])
        .git(["update-ref", "refs/notes/n", "HEAD"])
        .git(["tag", "v5"])
        .write(".git/refs/heads/zbroken", "garbage\n");
    let cases: &[&[&str]] = &[
        &["pack-refs"],
        &["pack-refs", "--all"],
        &["pack-refs", "--all", "--no-prune"],
        &["pack-refs", "--all", "--exclude", "refs/heads/*"],
        &["pack-refs", "--include", "refs/notes/*"],
        &["pack-refs", "--bogus"],
        &["pack-refs", "x"],
    ];
    for args in cases {
        fx.compare(*args)
            .checks(CHECKS.into_iter().chain([Check::Worktree, Check::GitFiles]))
            .git_file("packed-refs")
            .run();
    }
}
