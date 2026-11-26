// SPDX-License-Identifier: MIT
//! References whose names (or symbolic targets) `check-ref-format` rejects
//! (#214, t1430): warnings while listing, dangling symbolic references,
//! deletion, and the `update-ref` name checks, against the reference git.

use rgit_difftest::{Check, Fixture};

const CHECKS: [Check; 5] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Refs,
    Check::Reflogs,
];

/// A repository with a reference named `broken...ref`, a symbolic
/// reference to it, a symbolic reference with a bad name and a loose
/// file with broken content.
fn fixture() -> Option<Fixture> {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .git(["commit", "-q", "--allow-empty", "-m", "a"]);
    if fx.is_skipped() {
        return None;
    }
    let head = String::from_utf8(fx.git_stdout(["rev-parse", "HEAD"])).unwrap();
    let fx = fx
        .write(".git/refs/heads/broken...ref", head.as_bytes())
        .write(".git/refs/heads/badname", "ref: refs/heads/broken...ref\n")
        .write(".git/refs/heads/broken...symref", "ref: refs/heads/main\n")
        .write(".git/refs/tags/shadow", "ref: refs/heads/broken...ref\n");
    Some(fx)
}

fn run_cases(fx: &mut Fixture, cases: &[&[&str]]) {
    for case in cases {
        fx.compare(*case).checks(CHECKS).run();
    }
}

#[test]
fn listing_warns() {
    let Some(fx) = fixture() else { return };
    let mut fx = fx.write(".git/refs/heads/garbage", "not a ref\n");
    run_cases(&mut fx, &[&["for-each-ref"], &["branch"]]);
}

#[test]
fn deleting() {
    let Some(mut fx) = fixture() else { return };
    run_cases(
        &mut fx,
        &[
            &["update-ref", "--no-deref", "-d", "refs/heads/badname"],
            &["branch", "-d", "broken...symref"],
            &["branch", "-d", "broken...ref"],
            &["update-ref", "-d", "refs/tags/shadow"],
        ],
    );
}

#[test]
fn update_ref_names() {
    let Some(fx) = fixture() else { return };
    let mut fx = fx.write(".git/my-private-file", "precious\n");
    run_cases(
        &mut fx,
        &[
            &["update-ref", "-d", "my-private-file"],
            &["update-ref", "-d", "refs"],
            &["update-ref", "refs/heads/~a", "HEAD"],
        ],
    );
    for input in [
        "create ~a refs/heads/main\n",
        "update ~a refs/heads/main\n",
        "delete ~a\n",
        "verify ~a\n",
        "symref-create refs/heads/x ~a\n",
    ] {
        fx.compare(["update-ref", "--stdin"])
            .stdin(input)
            .checks(CHECKS)
            .run();
    }
    fx.compare(["update-ref", "-z", "--stdin"])
        .stdin("create ~a \0refs/heads/main\0")
        .checks(CHECKS)
        .run();
}
