// SPDX-License-Identifier: MIT
//! Differential tests of `merge-recursive` (the strategy program) against
//! the reference git: clean and conflicted merges into the index and work
//! tree, `--<option>`s, the labels and the usage errors.

use rgit_difftest::Fixture;

/// `main` (checked out): A - M (changes line 1) - X (changes line 3);
/// `s`: A - S (changes line 3).
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("f", "1\n2\n3\n")
        .git(["add", "f"])
        .tick()
        .git(["commit", "-q", "-m", "A"])
        .git(["checkout", "-q", "-b", "s"])
        .write("f", "1\n2\n3s\n")
        .tick()
        .git(["commit", "-q", "-am", "S"])
        .git(["checkout", "-q", "main"])
        .write("f", "1m\n2\n3\n")
        .tick()
        .git(["commit", "-q", "-am", "M"])
        .tick()
}

#[test]
fn merges() {
    // The index is refreshed first: the fixture's files have fixed times.
    let cases = [
        "main~1 -- HEAD s",
        "--theirs main~1 -- HEAD s",
        "main~1 -- HEAD s s",
        "-h",
        "",
        "main~1 -- HEAD nope",
    ];
    for args in cases {
        let mut fx = fixture();
        if fx.is_skipped() {
            return;
        }
        let alias = format!("alias.t=!git update-index -q --refresh; git merge-recursive {args}");
        fx.compare(["-c", alias.as_str(), "t"]).run();
    }
    // A conflict (`GITHEAD_<id>` does not change the labels).
    let script = "alias.t=!git update-index -q --refresh; printf '1\\n2\\n3x\\n' >f && \
                  git commit -q -am X && env GITHEAD_$(git rev-parse s)=theirs \
                  git merge-recursive main~2 -- HEAD s";
    let mut fx = fixture();
    fx.compare(["-c", script, "t"]).run();
}
