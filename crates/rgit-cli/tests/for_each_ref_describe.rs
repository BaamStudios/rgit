// SPDX-License-Identifier: MIT
//! `%(describe[:...])` in `for-each-ref` formats against the reference git.

use rgit_difftest::{Check, Fixture};

#[test]
fn describe_atom() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .git(["commit", "-q", "--allow-empty", "-m", "one"])
        .git(["tag", "-a", "-m", "t1", "tagone"])
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "two"])
        .git(["tag", "lw-foo"])
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "three"])
        .git(["tag", "-a", "-m", "x2", "x-bar"])
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "four"]);
    if fx.is_skipped() {
        return;
    }
    for format in [
        "%(objectname) %(describe)",
        "%(describe:tags)",
        "%(describe:abbrev=14)",
        "%(describe:match=*-foo,tags)",
        "%(describe:exclude=*-bar)",
        "%(*describe)",
        "%(describe:baz)",
        "%(describe:tags,qux=1,abbrev=14)",
        "%(describe:tags=no)",
        "%(describe:abbrev=x)",
    ] {
        let arg = format!("--format={format}");
        fx.compare(["for-each-ref", arg.as_str()])
            .checks(Check::OUTPUT)
            .run();
    }
}
