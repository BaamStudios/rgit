// SPDX-License-Identifier: MIT
//! `for-each-ref` and its format language against the reference git:
//! every atom over branches, tags of every kind and remote-tracking refs,
//! opening atoms, quoting, colours, sorting, the reachability filters
//! and the error messages. Dates are fixed by the fixture.

use rgit_difftest::{Check, Fixture};

const CHECKS: [Check; 3] = [Check::ExitCode, Check::Stdout, Check::Stderr];

/// History with trailers and multi-line messages, tags of every kind,
/// branches with upstreams (one gone), a push remote, a mailmap and a
/// linked work tree.
fn fixture() -> Option<Fixture> {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write(
            ".mailmap",
            "A Thor <athor@example.com> A U Thor <author@example.com>\n",
        )
        .write("a", "1\n")
        .git(["add", "a"])
        .git(["commit", "-q", "-m", "first line\nsecond line\n\nbody one\n\tbody two\n\nAcked-by: X <x@y>\n  folded\nSigned-off-by: A U Thor <author@example.com>"])
        .tick()
        .git(["tag", "v1.2"])
        .git(["tag", "-a", "-m", "annotated\n\nwith body\n\nKey: value", "v1.10"])
        .git(["-c", "advice.nestedTag=false", "tag", "-a", "-m", "nested", "nest", "v1.10"])
        .git(["tag", "-a", "-m", "of a tree", "tree-tag", "HEAD^{tree}"])
        .git(["tag", "blob-tag", "HEAD:a"])
        .write("sig", "signed-looking\n-----BEGIN PGP SIGNATURE-----\nabc\n-----END PGP SIGNATURE-----\n")
        .git(["tag", "-F", "sig", "--cleanup=verbatim", "fake-sig"])
        .git(["branch", "side"])
        .write("a", "2\n")
        .git(["commit", "-q", "-a", "-m", "two"])
        .tick()
        .git(["tag", "v1.9"])
        .write("a", "3\n")
        .git(["commit", "-q", "-a", "-m", "three: a subject. with -- odd chars"])
        .tick()
        .git(["remote", "add", "origin", "/nowhere"])
        .git(["remote", "add", "fork", "/elsewhere"])
        .git(["config", "remote.fork.push", "refs/heads/*:refs/heads/pushed/*"])
        .git(["update-ref", "refs/remotes/origin/main", "HEAD~1"])
        .git(["update-ref", "refs/remotes/origin/side", "HEAD"])
        .git(["update-ref", "refs/remotes/fork/pushed/main", "HEAD~2"])
        .git(["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main"])
        .git(["branch", "--set-upstream-to=origin/main", "main"])
        .git(["config", "branch.main.pushRemote", "fork"])
        .git(["config", "branch.side.remote", "origin"])
        .git(["config", "branch.side.merge", "refs/heads/side"])
        .git(["branch", "gone"])
        .git(["config", "branch.gone.remote", "origin"])
        .git(["config", "branch.gone.merge", "refs/heads/gone"])
        .git(["branch", "local", "HEAD~2"])
        .git(["config", "branch.local.remote", "."])
        .git(["config", "branch.local.merge", "refs/heads/main"])
        .git(["worktree", "add", "-q", "-b", "wt", "../wt", "HEAD~1"]);
    (!fx.is_skipped()).then_some(fx)
}

#[track_caller]
fn all_same(fx: &mut Fixture, cases: &[&[&str]]) {
    let mut failures = Vec::new();
    for args in cases {
        if let Err(m) = fx.compare(*args).checks(CHECKS).try_run() {
            failures.push(m.to_string());
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn object_and_name_atoms() {
    let Some(mut fx) = fixture() else { return };
    all_same(
        &mut fx,
        &[
            &["for-each-ref"],
            &[
                "for-each-ref",
                "--format=%(refname)|%(refname:short)|%(refname:lstrip=-1)|%(refname:rstrip=-2)|%(HEAD)|%(symref)|%(symref:short)|%(*refname)",
            ],
            &[
                "for-each-ref",
                "--format=%(objectname)|%(objectname:short)|%(objectname:short=12)|%(objecttype)|%(objectsize)|%(objectsize:disk)|%(deltabase)",
            ],
            &[
                "for-each-ref",
                "--format=%(tree)|%(tree:short)|%(parent:short=5)|%(numparent)|%(object)|%(type)|%(tag)|%(raw:size)",
            ],
            &[
                "for-each-ref",
                "--format=%(*objectname)|%(*objecttype)|%(*objectsize)|%(*subject)|%(*authorname)|%(*tree:short)",
                "refs/tags",
            ],
            &[
                "for-each-ref",
                "--format=%(raw)",
                "refs/tags/v1.10",
                "refs/heads/main",
            ],
        ],
    );
}

#[test]
fn person_and_message_atoms() {
    let Some(mut fx) = fixture() else { return };
    all_same(
        &mut fx,
        &[
            &[
                "for-each-ref",
                "--format=%(author)|%(authorname)|%(authoremail)|%(authoremail:trim)|%(authoremail:localpart)|%(authordate)|%(authordate:iso8601)|%(authordate:unix)",
            ],
            &[
                "for-each-ref",
                "--format=%(committer)|%(committername:mailmap)|%(committeremail:mailmap,trim)|%(tagger)|%(taggerdate:short)|%(creator)|%(creatordate:rfc2822)|%(authorname:mailmap)|%(authoremail:mailmap)",
            ],
            &[
                "for-each-ref",
                "--format=[%(subject)][%(subject:sanitize)][%(body)][%(contents:subject)][%(contents:body)][%(contents:signature)][%(contents:size)]",
            ],
            &[
                "for-each-ref",
                "--format=[%(contents)][%(contents:lines=1)][%(contents:lines=3)][%(contents:lines=0)]",
            ],
            &[
                "for-each-ref",
                "--format=%(author:mailmap)|%(creator:x)|%(object:short)",
            ],
        ],
    );
}

#[test]
fn trailers() {
    let Some(mut fx) = fixture() else { return };
    all_same(
        &mut fx,
        &[
            &[
                "for-each-ref",
                "--format=%(trailers)|",
                "refs/heads/side",
                "refs/tags/v1.10",
            ],
            &[
                "for-each-ref",
                "--format=%(trailers:only)|%(trailers:unfold)|%(contents:trailers:key=acked-by:)",
                "refs/heads/side",
            ],
            &[
                "for-each-ref",
                "--format=%(trailers:key=Signed-off-by,valueonly,separator=%x2C)|%(trailers:keyonly,separator=%x3B)|%(trailers:key_value_separator=%x3D,unfold)",
                "refs/heads/side",
            ],
            &["for-each-ref", "--format=%(trailers:bogus)"],
            &["for-each-ref", "--format=%(trailers:key)"],
            &["for-each-ref", "--format=%(contents:trailersonly)"],
        ],
    );
}

#[test]
fn tracking_and_worktrees() {
    let Some(mut fx) = fixture() else { return };
    all_same(
        &mut fx,
        &[
            &[
                "for-each-ref",
                "--format=%(refname:short)|%(upstream)|%(upstream:short)|%(upstream:track)|%(upstream:trackshort)|%(upstream:track,nobracket)|%(upstream:remotename)|%(upstream:remoteref)",
                "refs/heads",
            ],
            &[
                "for-each-ref",
                "--format=%(refname:short)|%(push)|%(push:short)|%(push:track)|%(push:trackshort)|%(push:remotename)|%(push:remoteref)|%(upstream:lstrip=-1)",
                "refs/heads",
            ],
            &[
                "for-each-ref",
                "--format=%(refname:short) %(ahead-behind:main) %(ahead-behind:side)",
            ],
            &[
                "for-each-ref",
                "--format=%(refname:short) %(worktreepath)",
                "refs/heads",
            ],
            &["for-each-ref", "--format=%(ahead-behind)"],
            &["for-each-ref", "--format=%(ahead-behind:nope)"],
            &["for-each-ref", "--format=%(upstream:short,track)"],
            &["for-each-ref", "--format=%(refname:lstrip=x)"],
        ],
    );
}

#[test]
fn opening_atoms_quoting_and_color() {
    let Some(mut fx) = fixture() else { return };
    all_same(
        &mut fx,
        &[
            &[
                "for-each-ref",
                "--format=%(if)%(*objectname)%(then)annotated %(*objecttype)%(else)plain%(end)|%(if:equals=main)%(refname:short)%(then)M%(end)|%(if:notequals=main)%(refname:short)%(then)N%(end)",
            ],
            &[
                "for-each-ref",
                "--format=|%(align:20)%(refname:short)%(end)|%(align:middle,20)%(refname:short)%(end)|%(align:width=20,position=right)%(refname:short)%(end)|",
            ],
            &[
                "for-each-ref",
                "--shell",
                "--format=|%(align:30,middle)'%(refname:short)| %(authorname)'%(end)|%(if)%(HEAD)%(then)it's head%(end)",
            ],
            &["for-each-ref", "--perl", "--format=%(subject) %(refname)"],
            &["for-each-ref", "--python", "--format=%(subject) %(body)"],
            &["for-each-ref", "--tcl", "--format=%(subject) [%(refname)]"],
            &["for-each-ref", "--python", "--format=%(raw)"],
            &[
                "for-each-ref",
                "--perl",
                "--format=%(raw:size)",
                "refs/heads",
            ],
            &[
                "for-each-ref",
                "--color",
                "--format=%(color:red)%(refname:short)%(color:reset) %(color:bold blue)x",
            ],
            &[
                "for-each-ref",
                "--color=never",
                "--format=%(color:red)%(refname:short)",
            ],
            &["for-each-ref", "--format=%(color:red)%(refname:short)"],
            &[
                "for-each-ref",
                "--omit-empty",
                "--format=%(if)%(upstream)%(then)%(refname)%(end)",
            ],
            &["for-each-ref", "--format=%(if)"],
            &["for-each-ref", "--format=%(then)"],
            &["for-each-ref", "--format=%(if)%(then)%(then)%(end)"],
            &["for-each-ref", "--format=%(if)%(then)%(else)%(else)%(end)"],
            &["for-each-ref", "--format=%(if)%(else)%(end)"],
            &["for-each-ref", "--format=%(if)%(end)"],
            &["for-each-ref", "--format=%(end)"],
            &["for-each-ref", "--format=%(align)"],
            &["for-each-ref", "--format=%(align:left,right)"],
            &["for-each-ref", "--format=%(align:position=up)"],
            &["for-each-ref", "--format=%(color)"],
            &["for-each-ref", "--format=%(color:nocolor)"],
            &["for-each-ref", "--format=%(rest)"],
            &["for-each-ref", "--format=%(authoremail:trim,bad)"],
            &["for-each-ref", "--format=%(contents:lines=-1)"],
            &["for-each-ref", "--format=%(objectname:short=0)"],
            &["for-each-ref", "--format=%(authordate:bogus)"],
            &["for-each-ref", "--format=%()"],
        ],
    );
}

#[test]
fn sorting_and_filters() {
    let Some(mut fx) = fixture() else { return };
    all_same(
        &mut fx,
        &[
            &[
                "for-each-ref",
                "--format=%(refname)",
                "--sort=version:refname",
                "refs/tags",
            ],
            &[
                "for-each-ref",
                "--format=%(refname)",
                "--sort=-v:refname",
                "--sort=objecttype",
            ],
            &[
                "for-each-ref",
                "--format=%(refname) %(creatordate:unix)",
                "--sort=-refname",
                "--sort=creatordate",
            ],
            &[
                "for-each-ref",
                "--format=%(refname)",
                "--sort=objectsize",
                "--sort=raw",
            ],
            &[
                "for-each-ref",
                "--format=%(refname)",
                "--sort=contents:size",
                "--ignore-case",
            ],
            &[
                "for-each-ref",
                "--format=%(refname)",
                "--sort=refname",
                "--no-sort",
            ],
            &["for-each-ref", "--format=%(refname)", "--merged=HEAD~1"],
            &[
                "for-each-ref",
                "--format=%(refname)",
                "--merged",
                "--no-merged=HEAD~2",
            ],
            &["for-each-ref", "--format=%(refname)", "--contains=HEAD~1"],
            &["for-each-ref", "--format=%(refname)", "--no-contains=HEAD"],
            &["for-each-ref", "--format=%(refname)", "--points-at=v1.10"],
            &[
                "for-each-ref",
                "--format=%(refname)",
                "--count=3",
                "--sort=-refname",
            ],
            &[
                "for-each-ref",
                "--format=%(refname)",
                "--exclude=refs/tags/v*",
                "refs/tags",
            ],
            &[
                "for-each-ref",
                "--format=%(refname)",
                "--start-after=refs/remotes",
            ],
            &[
                "for-each-ref",
                "--format=%(refname)",
                "--start-after=x",
                "refs/tags",
            ],
            &[
                "for-each-ref",
                "--format=%(refname)",
                "--start-after=x",
                "--sort=refname",
            ],
            &["for-each-ref", "--format=%(refname)", "--merged=nope"],
            &[
                "for-each-ref",
                "--format=%(refname)",
                "--contains=HEAD^{tree}",
            ],
            &["for-each-ref", "--count=nope"],
            &["for-each-ref", "--sort=bogus"],
        ],
    );
    fx.compare(["for-each-ref", "--format=%(refname)", "--stdin"])
        .stdin(b"refs/tags/v*\nrefs/heads/s\nrefs/heads/side\n".to_vec())
        .checks(CHECKS)
        .run();
}

#[test]
fn tag_list_formats() {
    let Some(mut fx) = fixture() else { return };
    all_same(
        &mut fx,
        &[
            &[
                "tag",
                "-l",
                "--format=%(refname:short) %(if)%(*objectname)%(then)%(contents:subject)%(end)|%(trailers:only)",
            ],
            &["tag", "-l", "--format=%(refname) %(ahead-behind:HEAD) !"],
            &[
                "tag",
                "-l",
                "--color",
                "--format=%(color:red)%(refname:short)",
            ],
            &[
                "-c",
                "color.ui=always",
                "tag",
                "-l",
                "--format=%(color:red)%(refname:short)",
            ],
            &["tag", "-l", "--sort=-taggerdate", "--sort=version:refname"],
            &[
                "tag",
                "-l",
                "--sort=tag",
                "--format=%(tag) %(refname:short)",
            ],
        ],
    );
}
