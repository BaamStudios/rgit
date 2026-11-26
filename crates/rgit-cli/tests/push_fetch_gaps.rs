// SPDX-License-Identifier: MIT
//! Client details of `push` and `fetch` against the reference git (#746):
//! a `receive-pack` that exits with an error, a remote helper that can
//! neither push nor export, URL-valued push remotes and the remote they
//! update, `branch..<key>` entries, and the packet trace of a protocol v2
//! fetch over HTTP.

use std::process::Command;

use rgit_difftest::{Check, Fixture};

mod common;
use common::servers::{start_http, start_http_with};

/// The output and the client's refs (the server's loose objects are
/// written by different `unpack-objects` programs).
const CHECKS: [Check; 4] = [Check::ExitCode, Check::Stdout, Check::Stderr, Check::Refs];

/// A client `c` (one commit on `main`) with a bare `srv.git` as `up`;
/// `rp-fail` runs the reference `git receive-pack` and exits with 1.
fn fixture() -> Fixture {
    let fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"));
    if fx.is_skipped() {
        return fx;
    }
    let rp = format!(
        "#!/bin/sh\nenv -u GIT_EXEC_PATH '{}' receive-pack \"$@\"\nexit 1\n",
        fx.reference_git().display()
    );
    fx.write_executable("rp-fail", rp)
        .git(["init", "-q", "--bare", "srv.git"])
        .git(["init", "-q", "-b", "main", "c"])
        .write("c/f", "one\n")
        .git(["-C", "c", "add", "f"])
        .git(["-C", "c", "commit", "-q", "-m", "one"])
        .tick()
        .git(["-C", "c", "remote", "add", "up", "../srv.git"])
}

fn compare(fx: &mut Fixture, args: &[&str]) {
    fx.compare(args).in_dir("c").repo("c").checks(CHECKS).run();
}

/// A `receive-pack` exiting with an error after its report fails the
/// push (t5543): the report stands, `Done` is left out with
/// `--porcelain`, also when nothing was to be sent.
#[test]
fn receive_pack_exit_code_fails_the_push() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let rp = format!("--receive-pack={}", fx.work_dir().join("rp-fail").display());
    for args in [
        &["push", "--atomic", &rp, "up", "HEAD:refs/heads/a"][..],
        &[
            "push",
            "--atomic",
            "--porcelain",
            &rp,
            "up",
            "HEAD:refs/heads/b",
        ],
        &["push", &rp, "up", "HEAD:refs/heads/c"],
        &["push", "--porcelain", &rp, "up", "HEAD:refs/heads/c"],
        &["push", &rp, "up", "HEAD:refs/heads/c"],
        &["push", "--dry-run", &rp, "up", "HEAD:refs/heads/d"],
        &["send-pack", &rp, "../srv.git", "HEAD:refs/heads/e"],
    ] {
        compare(&mut fx, args);
    }
}

/// A helper that logs what it is sent to stderr; `$SC_CAPS` are its
/// capabilities.
const HELPER: &str = "#!/bin/sh
while read -r line
do
	echo \"< $line\" >&2
	case \"$line\" in
	capabilities) printf '%s\\n' $SC_CAPS '' ;;
	list*) printf '%s\\n' '? refs/heads/main' '@refs/heads/main HEAD' '' ;;
	option*) echo unsupported ;;
	'') exit 0 ;;
	*) exit 1 ;;
	esac
done
";

fn path_with(dir: &std::path::Path) -> std::ffi::OsString {
    let mut dirs = vec![dir.to_path_buf()];
    if let Some(p) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    std::env::join_paths(dirs).unwrap()
}

/// A helper without `push` and `export` is asked for a plain `list`; the
/// push then fails without a message of its own. Options a pushing
/// helper does not take end the push.
#[test]
fn helper_that_cannot_push() {
    let fx = fixture().write_executable("bin/git-remote-sc", HELPER);
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx;
    let path = path_with(&fx.work_dir().join("bin"));
    let cases: &[(&str, &[&str])] = &[
        ("fetch", &["push", "sc::x", "HEAD:refs/heads/main"]),
        (
            "fetch option",
            &["push", "-q", "sc::x", "HEAD:refs/heads/main"],
        ),
        (
            "fetch",
            &["push", "--porcelain", "sc::x", "HEAD:refs/heads/main"],
        ),
        (
            "fetch",
            &["push", "--dry-run", "sc::x", "HEAD:refs/heads/main"],
        ),
        (
            "fetch",
            &["push", "--atomic", "sc::x", "HEAD:refs/heads/main"],
        ),
        ("import", &["push", "sc::x", "HEAD:refs/heads/main"]),
        ("export", &["push", "sc::x", "HEAD:refs/heads/main"]),
        (
            "push option",
            &["push", "--atomic", "sc::x", "HEAD:refs/heads/main"],
        ),
        (
            "push option",
            &["push", "-o", "a", "sc::x", "HEAD:refs/heads/main"],
        ),
        (
            "push",
            &["push", "--atomic", "sc::x", "HEAD:refs/heads/main"],
        ),
    ];
    for (caps, args) in cases {
        fx.compare(*args)
            .in_dir("c")
            .repo("c")
            .env("PATH", &path)
            .env("SC_CAPS", caps)
            .checks(Check::OUTPUT)
            .run();
    }
}

/// `branch.<name>.pushRemote`, `remote.pushDefault` and
/// `branch.<name>.remote` may name a URL (t5516, t5505); a push to a URL
/// updates the remote-tracking refs of the one remote pushing there.
#[test]
fn url_valued_push_remotes() {
    let fx = fixture().git(["init", "-q", "--bare", "two.git"]).git([
        "-C",
        "c",
        "config",
        "push.default",
        "current",
    ]);
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx;
    for key in [
        "branch.main.pushRemote",
        "remote.pushDefault",
        "branch.main.remote",
    ] {
        let mut f = fixture()
            .git(["init", "-q", "--bare", "two.git"])
            .git(["-C", "c", "config", "push.default", "current"])
            .git(["-C", "c", "config", key, "../two.git"]);
        compare(&mut f, &["push"]);
        let mut f = fixture().git(["-C", "c", "config", key, "nosuch"]);
        compare(&mut f, &["push"]);
    }
    let fork = format!("file://{}", fx.work_dir().join("two.git").display());
    fx = fx.git(["-C", "c", "remote", "add", "fork", &fork]).git([
        "-C",
        "c",
        "config",
        "branch.main.pushRemote",
        &fork,
    ]);
    compare(&mut fx, &["push", "-v"]);
    // Up to date: the stale remote-tracking ref is refreshed.
    fx = fx.git(["-C", "c", "update-ref", "-d", "refs/remotes/fork/main"]);
    compare(&mut fx, &["push", "-v"]);
    fx = fx.git(["-C", "c", "update-ref", "-d", "refs/remotes/fork/main"]);
    compare(&mut fx, &["push", "-v", &fork, "main"]);
    // A push URL of its own: the remote no longer pushes to that URL.
    fx = fx
        .git(["-C", "c", "update-ref", "-d", "refs/remotes/fork/main"])
        .git(["-C", "c", "config", "remote.fork.pushurl", "../srv.git"]);
    compare(&mut fx, &["push", "-v"]);
    // Two remotes with the URL: neither is updated.
    fx = fx
        .git(["-C", "c", "config", "--unset", "remote.fork.pushurl"])
        .git(["-C", "c", "remote", "add", "dup", &fork]);
    compare(&mut fx, &["push", "-v"]);
}

/// A `branch` section without a name is refused by everything that reads
/// the remotes (t5516).
#[test]
fn branch_section_without_name() {
    let fx = fixture()
        .git(["-C", "c", "config", "branch..remote", "up"])
        .git(["-C", "c", "config", "branch.autoSetupMerge", "true"]);
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx;
    compare(&mut fx, &["push", "up", "main"]);
    compare(&mut fx, &["fetch", "up"]);
    compare(&mut fx, &["remote"]);
    let mut fx = fixture();
    compare(&mut fx, &["-c", "branch..merge=x", "push", "up", "main"]);
    compare(&mut fx, &["-c", "branch.merge=x", "push", "up", "main"]);
}

fn rgit(fx: &Fixture, dir: &str, args: &[&str], trace: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(args)
        .current_dir(fx.work_dir().join(dir))
        .env("HOME", fx.home_dir())
        .env("XDG_CONFIG_HOME", fx.home_dir().join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TRACE_PACKET", trace)
        .output()
        .unwrap()
}

/// Over HTTP the v2 capabilities are traced as `git<` (twice the version
/// line) and again under the program's name; a fetch by object name
/// asks for no refs (t5702).
#[test]
fn v2_http_fetch_trace() {
    let fx = fixture()
        .git(["init", "-q", "-b", "main", "srv"])
        .git(["-C", "srv", "commit", "-q", "--allow-empty", "-m", "one"])
        .tick()
        .git(["clone", "-q", "srv", "cl"])
        .git(["-C", "srv", "commit", "-q", "--allow-empty", "-m", "two"]);
    if fx.is_skipped() {
        return;
    }
    let base = start_http(&fx);
    let fx = fx.git([
        "-C",
        "cl",
        "remote",
        "set-url",
        "origin",
        &format!("{base}/srv"),
    ]);
    let two = String::from_utf8(fx.git_stdout(["-C", "srv", "rev-parse", "HEAD"])).unwrap();
    let trace = fx.work_dir().join("trace");
    let out = rgit(
        &fx,
        "cl",
        &[
            "-c",
            "protocol.version=2",
            "fetch",
            "--no-tags",
            "origin",
            two.trim(),
        ],
        &trace,
    );
    assert!(out.status.success(), "{out:?}");
    let log = std::fs::read_to_string(&trace).unwrap();
    assert_eq!(log.matches("git< version 2").count(), 2, "{log}");
    assert!(log.contains("fetch< version 2"), "{log}");
    assert!(log.contains("fetch< object-format=sha1"), "{log}");
    assert!(!log.contains("fetch> command=ls-refs"), "{log}");
    assert!(log.contains("fetch> command=fetch"), "{log}");
}

/// A fetch from a repository served with `GIT_NAMESPACE` sees the refs of
/// the namespace (t5702).
#[test]
fn v2_http_fetch_namespace() {
    let fx = fixture()
        .git(["init", "-q", "-b", "main", "ns"])
        .git(["-C", "ns", "commit", "-q", "--allow-empty", "-m", "one"])
        .tick()
        .git(["-C", "ns", "tag", "one"])
        .git(["-C", "ns", "commit", "-q", "--allow-empty", "-m", "two"])
        .git([
            "-C",
            "ns",
            "update-ref",
            "refs/namespaces/ns/refs/heads/main",
            "one",
        ]);
    if fx.is_skipped() {
        return;
    }
    let base = start_http_with(&fx, vec![("GIT_NAMESPACE".into(), "ns".into())]);
    let trace = fx.work_dir().join("trace");
    let url = format!("{base}/ns");
    let out = rgit(
        &fx,
        "c",
        &[
            "-c",
            "protocol.version=2",
            "fetch",
            &url,
            "refs/heads/main:refs/heads/theirs",
        ],
        &trace,
    );
    assert!(out.status.success(), "{out:?}");
    let log = std::fs::read_to_string(&trace).unwrap();
    assert!(log.contains("fetch< version 2"), "{log}");
    let one = fx.git_stdout(["-C", "ns", "rev-parse", "one"]);
    let theirs = fx.git_stdout(["-C", "c", "rev-parse", "theirs"]);
    assert_eq!(one, theirs);
}
