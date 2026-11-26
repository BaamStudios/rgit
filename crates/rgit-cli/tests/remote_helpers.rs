// SPDX-License-Identifier: MIT
//! Remote helpers (`git remote-<transport>`, `ext::`, `fd::`) against the
//! reference git: how URLs reach a helper, its arguments and environment,
//! the `capabilities`/`list` exchange, `connect` through `remote-ext`,
//! the protocol allow-list and the built-in helpers' own interface.

use rgit_difftest::{Check, Fixture};

/// A helper that reports its arguments and `GIT_DIR`, then fails.
const SHOW: &str = "#!/bin/sh\necho \"args: $*\" >&2\necho \"dir: ${GIT_DIR:-none}\" >&2\nexit 1\n";

/// A helper with `fetch` that lists refs of every kind of value.
const LIST: &str = "#!/bin/sh
while read line
do
	case \"$line\" in
	capabilities) printf 'fetch\\noption\\n\\n' ;;
	option*) echo unsupported ;;
	list) printf ':object-format sha1\\n? refs/heads/main\\n? refs/tags/v1\\n@refs/heads/main HEAD\\n\\n' ;;
	'') exit 0 ;;
	esac
done
";

/// A helper announcing a mandatory capability nobody knows.
const MANDATORY: &str = "#!/bin/sh\nread x\nprintf '*frobnicate\\n\\n'\nread y\n";

fn path_with(dir: &std::path::Path) -> std::ffi::OsString {
    let mut dirs = vec![dir.to_path_buf()];
    if let Some(p) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    std::env::join_paths(dirs).unwrap()
}

#[test]
fn helper_dispatch_same_as_git() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .write_executable("bin/git-remote-show", SHOW)
        .write_executable("bin/git-remote-list", LIST)
        .write_executable("bin/git-remote-mand", MANDATORY)
        .git(["init", "-q", "-b", "main", "r"])
        .git(["-C", "r", "remote", "add", "r1", "show::abc"])
        .git(["-C", "r", "remote", "add", "r2", "show://abc"])
        .git(["-C", "r", "remote", "add", "r3", "/some/path"])
        .git(["-C", "r", "config", "remote.r3.vcs", "show"])
        .git(["-C", "r", "config", "remote.r4.vcs", "show"])
        .git(["-C", "r", "remote", "add", "l", "list::x"]);
    if fx.is_skipped() {
        return;
    }
    let path = path_with(&fx.work_dir().join("bin"));
    let outside: &[&[&str]] = &[
        &["ls-remote", "show::foo/bar"],
        &["ls-remote", "show://foo/bar"],
        &["ls-remote", "mand::x"],
        &["-c", "protocol.show.allow=never", "ls-remote", "show::x"],
    ];
    for args in outside {
        fx.compare(*args)
            .env("PATH", &path)
            .checks(Check::OUTPUT)
            .run();
    }
    let inside: &[&[&str]] = &[
        &["fetch", "r1"],
        &["fetch", "r2"],
        &["fetch", "r3"],
        &["fetch", "r4"],
        &["fetch", "show::foo"],
        &["ls-remote", "r1"],
        &["ls-remote", "l"],
        &["ls-remote", "r3"],
        &["ls-remote", "--symref", "l"],
    ];
    for args in inside {
        fx.compare(*args)
            .in_dir("r")
            .repo("r")
            .env("PATH", &path)
            .checks(Check::OUTPUT)
            .run();
    }
    // The helper's name is what the allow-list checks, not the URL's.
    fx.compare(["fetch", "r3"])
        .in_dir("r")
        .repo("r")
        .env("PATH", &path)
        .env("GIT_ALLOW_PROTOCOL", "file")
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn ext_connect_same_as_git() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main", "r"])
        .git(["-C", "r", "commit", "-q", "--allow-empty", "-m", "a"])
        .git(["-C", "r", "tag", "-a", "v1", "-m", "t"])
        .git(["-C", "r", "branch", "dev"]);
    if fx.is_skipped() {
        return;
    }
    let url = "ext::sh -c %S% r";
    let cases: &[&[&str]] = &[
        &["-c", "protocol.ext.allow=always", "ls-remote", url],
        &[
            "-c",
            "protocol.ext.allow=always",
            "ls-remote",
            "--symref",
            url,
        ],
        &[
            "-c",
            "protocol.version=0",
            "-c",
            "protocol.ext.allow=always",
            "ls-remote",
            url,
        ],
        &["ls-remote", url],
        &[
            "-c",
            "protocol.ext.allow=always",
            "ls-remote",
            "ext::sh -c %x",
        ],
        &[
            "-c",
            "protocol.ext.allow=always",
            "ls-remote",
            "ext::sh -c %",
        ],
    ];
    for args in cases {
        fx.compare(*args).checks(Check::OUTPUT).run();
    }
    fx.compare(["-c", "protocol.ext.allow=always", "clone", "-q", url, "c"])
        .checks(Check::OUTPUT)
        .run();
    fx.compare(["for-each-ref"])
        .in_dir("c")
        .repo("c")
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn builtin_helpers_same_as_git() {
    let mut fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"));
    if fx.is_skipped() {
        return;
    }
    let cases: &[(&[&str], &str)] = &[
        (&["remote-ext", "-h"], ""),
        (&["remote-fd", "-h"], ""),
        (&["remote-ext"], ""),
        (&["remote-fd", "x"], ""),
        (&["remote-fd", "x", "y", "z"], ""),
        (&["remote-ext", "x", "cmd"], "capabilities\n\n\n"),
        (&["remote-ext", "x", "cmd"], "capabilities\nfoo\n"),
        (&["remote-ext", "x", "cmd"], "option x y\n"),
        (&["remote-fd", "x", "7"], "capabilities\n"),
        (&["remote-fd", "x", "7,8/abc"], "capabilities\n"),
        (&["remote-fd", "x", "7,8x"], "capabilities\n"),
        (&["remote-fd", "x", ""], "capabilities\n"),
        (
            &["remote-ext", "x", "echo %s %S x"],
            "connect git-upload-pack\n",
        ),
    ];
    for (args, input) in cases {
        fx.compare(*args)
            .stdin(input.as_bytes().to_vec())
            .checks(Check::OUTPUT)
            .run();
    }
}
