// SPDX-License-Identifier: MIT
//! `rev-parse --parseopt`, `--sq-quote` and the date options against the
//! reference git.

use rgit_difftest::{Check, Fixture};

const SPEC: &str = "\
some-command [options] <args>...

some-command does foo and bar!
--
h,help!   show the help

foo       some nifty option --foo
bar=      some cool option --bar with an argument
b,baz     a short and long option

 An option group Header
C?        option C with an optional argument
d,data?   short and long option with an optional argument

 Argument hints
B=arg     short option required argument
e,fuz=with-space  short and long option required argument
s?some    short option optional argument
long?data long option optional argument
g,fluf?path     short and long option optional argument
hidden*   not shown with -h
pair=key=value  with an equals sign in the hint
cswitch\t switch has trailing tab character
no-neg    can be positivated
pos!      cannot be negated

Extras
extra1    after a header without help
";

#[test]
fn parseopt() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"]);
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["--", "--foo", "--bar=ham", "--baz", "arg"],
        &[
            "--", "-C", "Z", "-CZ", "-dA", "-d", "A", "-B", "x", "-Bx", "--no-foo",
        ],
        &[
            "--stuck-long",
            "--",
            "-CZ",
            "-dA",
            "-d",
            "-Bx",
            "--bar",
            "x",
            "-sx",
            "--long=y",
            "-gq",
        ],
        &["--", "-s", "-sx", "--long", "--long=y", "--data=", "-B", ""],
        &["--", "-h"],
        &["--", "--help"],
        &["--", "--help-all"],
        &["--", "--nope"],
        &["--", "-x"],
        &["--", "--bar"],
        &["--", "--fo"],
        &["--", "--foo=x"],
        &["--", "--no-pos"],
        &["--", "--neg", "--no-no-neg"],
        &["--", "a", "b c", "d'e"],
        &["--", "--foo", "--", "arg", "--bar=ham"],
        &["--stop-at-non-option", "--", "--foo", "arg", "--bar=ham"],
        &["--keep-dashdash", "--", "--foo", "--", "arg"],
        &[
            "--keep-dashdash",
            "--stop-at-non-option",
            "--",
            "--foo",
            "arg",
            "--x",
        ],
        &[],
        &["--foo"],
        &["--keep-dashdash"],
    ];
    for args in cases {
        let mut argv = vec!["rev-parse", "--parseopt"];
        argv.extend_from_slice(args);
        fx.compare(&argv)
            .stdin(SPEC)
            .env("GIT_TEST_DISALLOW_ABBREVIATED_OPTIONS", "false")
            .checks(Check::OUTPUT)
            .run();
    }
    for spec in [
        "",
        "cmd\n",
        "--\nq x\n",
        "x\n--\n=, x\n",
        "cmd\n--\nnoble x\n",
    ] {
        fx.compare(["rev-parse", "--parseopt", "--", "--no"])
            .stdin(spec)
            .env("GIT_TEST_DISALLOW_ABBREVIATED_OPTIONS", "false")
            .checks(Check::OUTPUT)
            .run();
    }
}

#[test]
fn sq_quote_and_dates() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .git(["commit", "-q", "--allow-empty", "-m", "one"]);
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["rev-parse", "--sq-quote", "a", "b c", "d'e", ""],
        &["rev-parse", "--sq-quote"],
        &[
            "rev-parse",
            "--since=1700000000",
            "--until=1700000100",
            "HEAD",
        ],
        &[
            "rev-parse",
            "--after=2023-11-14 22:13:20 +0000",
            "--before=1700000000",
        ],
        &[
            "rev-parse",
            "--revs-only",
            "--since=1700000000",
            "--no-revs",
        ],
        &["rev-parse", "--since"],
    ];
    for args in cases {
        fx.compare(*args).checks(Check::OUTPUT).run();
    }
}

/// Reflog selectors at the edges of the log, `@@{u}`, and `@{-1}` in
/// commands that use the object-name subset (`merge`, `cat-file`).
#[test]
fn reflog_edges_and_previous_branch() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .git(["commit", "-q", "--allow-empty", "-m", "one"])
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "two"])
        .git(["config", "branch.main.remote", "."])
        .git(["config", "branch.main.merge", "refs/heads/main"])
        .git(["checkout", "-q", "-b", "other", "HEAD~1"])
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "three"])
        .git(["checkout", "-q", "main"])
        .git(["branch", "empty"])
        .git(["reflog", "expire", "--expire=now", "refs/heads/empty"])
        .git(["branch", "once", "HEAD~1"])
        .git(["reflog", "expire", "--expire=now", "refs/heads/once"])
        .git(["update-ref", "-m", "moved", "refs/heads/once", "HEAD"]);
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["rev-parse", "empty@{0}"],
        &["rev-parse", "empty@{1}"],
        &["rev-parse", "once@{1}"],
        &["rev-parse", "once@{2}"],
        &["rev-parse", "@@{u}"],
        &["rev-parse", "@@{upstream}"],
        &["cat-file", "-t", "@{-1}"],
        &["cat-file", "-p", "@{-1}~0"],
    ];
    for args in cases {
        fx.compare(*args).checks(Check::OUTPUT).run();
    }
    fx.compare(["merge", "--no-edit", "@{-1}"])
        .checks([Check::ExitCode, Check::Stdout, Check::Refs])
        .run();
}

/// `--local-env-vars`: alone also outside a repository, else in order
/// with the other arguments.
#[test]
fn local_env_vars() {
    let mut fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"));
    if fx.is_skipped() {
        return;
    }
    fx.compare(["rev-parse", "--local-env-vars"])
        .checks(Check::OUTPUT)
        .run();
    let mut fx =
        fx.git(["init", "-q", "-b", "main"])
            .git(["commit", "-q", "--allow-empty", "-m", "a"]);
    for args in [
        &["rev-parse", "--local-env-vars"][..],
        &["rev-parse", "HEAD", "--local-env-vars"],
        &["rev-parse", "--local-env-vars", "HEAD", "--git-dir"],
        &["rev-parse", "--local-env-vars", "--local-env-vars"],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
}
