// SPDX-License-Identifier: MIT

//! `advise` (t0018) against the reference `test-tool`.

mod common;

use common::fixture;

#[test]
fn advise_matches_reference() {
    let Some(mut fx) = fixture() else { return };
    // Outside a repository, and the argument count check before that.
    fx.compare(["advise"]).output_only().run();
    fx.compare(["advise", "a", "b"]).output_only().run();
    // The fixture's "git" is the reference test-tool: write a minimal
    // repository by hand.
    let mut fx = fx
        .write(".git/HEAD", "ref: refs/heads/main\n")
        .write(".git/config", "[core]\n\trepositoryformatversion = 0\n")
        .write(".git/objects/info/.keep", "")
        .write(".git/refs/heads/.keep", "");
    for args in [
        &["advise"][..],
        &["advise", "a", "b"],
        &["advise", "This is a piece of advice"],
        &["advise", "a\n\n\tb\n"],
        &["advise", "-h"],
        &["advise", ""],
    ] {
        fx.compare(args).output_only().run();
    }
    for (k, v) in [
        ("GIT_ADVICE", "0"),
        ("GIT_ADVICE", "bogus"),
        ("GIT_CONFIG_PARAMETERS", "'advice.nestedtag'='true'"),
        ("GIT_CONFIG_PARAMETERS", "'advice.nestedtag'='false'"),
        ("GIT_CONFIG_PARAMETERS", "'advice.nestedtag'='bogus'"),
        ("GIT_CONFIG_PARAMETERS", "'color.advice'='always'"),
    ] {
        fx.compare(["advise", "piece"])
            .env(k, v)
            .output_only()
            .run();
    }
}
