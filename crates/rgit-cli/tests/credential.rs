// SPDX-License-Identifier: MIT
//! Differential tests of `git credential` against the reference git.
//! Helpers are shell snippets or absolute paths, so the tests do not depend
//! on running `git credential-<name>` (external commands).

use rgit_difftest::{Check, Fixture};

const DUMP: &str = r#"#!/bin/sh
echo "dump: $*" >&2
while IFS= read -r l; do echo "dump< $l" >&2; done
test -n "$REPLY" && printf '%b' "$REPLY"
exit 0
"#;

const ASKPASS: &str = r#"#!/bin/sh
echo "askpass: [$*]" >&2
case "$1" in Username*) echo askpass-username;; *) echo askpass-password;; esac
"#;

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .write_executable("dump", DUMP)
        .write_executable("askpass", ASKPASS)
}

fn run(fx: &mut Fixture, args: &[&str], stdin: &str, env: &[(&str, &str)]) {
    let work = fx.work_dir();
    let askpass = work.join("askpass");
    let mut argv: Vec<String> = Vec::new();
    for a in args {
        argv.push(a.replace("$W", &work.display().to_string()));
    }
    let mut c = fx
        .compare(&argv)
        .stdin(stdin.as_bytes().to_vec())
        .env("GIT_ASKPASS", &askpass)
        .checks(Check::OUTPUT);
    for (k, v) in env {
        c = c.env(k, v.replace("$W", &work.display().to_string()));
    }
    c.run();
}

#[test]
fn fill_prompts() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    run(
        &mut fx,
        &["credential", "fill"],
        "protocol=https\nhost=example.com\n",
        &[],
    );
    run(
        &mut fx,
        &["-c", "credential.useHttpPath=true", "credential", "fill"],
        "protocol=https\nhost=ex ample.com\npath=a/b c\nusername=a\x1bb\n",
        &[],
    );
    run(
        &mut fx,
        &["credential", "fill"],
        "protocol=https\nhost=example.com\n",
        &[("GIT_ASKPASS", ""), ("GIT_TERMINAL_PROMPT", "0")],
    );
    run(
        &mut fx,
        &["-c", "credential.interactive=false", "credential", "fill"],
        "protocol=https\nhost=example.com\n",
        &[],
    );
}

/// A helper that exits without reading its input (#262). The request is
/// larger than a pipe buffer, so the write always fails with EPIPE after the
/// helper is gone; git ignores SIGPIPE there and goes on with the next helper.
#[test]
fn helper_exits_without_reading() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let path = "p".repeat(256 * 1024);
    let input = format!("protocol=https\nhost=example.com\npath={path}\n");
    for first in ["!echo username=one", "!exit 0"] {
        run(
            &mut fx,
            &[
                "-c",
                "credential.useHttpPath=true",
                "-c",
                &format!("credential.helper={first}"),
                "-c",
                "credential.helper=!f() { cat >/dev/null; echo username=u; echo password=two; }; f",
                "credential",
                "fill",
            ],
            &input,
            &[],
        );
    }
}

#[test]
fn fill_with_helpers() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let helper = "credential.helper=$W/dump";
    run(
        &mut fx,
        &["-c", helper, "credential", "fill"],
        "capability[]=authtype\nprotocol=https\nhost=example.com\nwwwauth[]=Basic realm=x\n",
        &[(
            "REPLY",
            "capability[]=authtype\\nauthtype=Basic\\ncredential=xyz\\n",
        )],
    );
    run(
        &mut fx,
        &[
            "-c",
            "credential.helper=!echo username=one",
            "-c",
            helper,
            "credential",
            "fill",
        ],
        "url=https://example.com/foo.git\n",
        &[("REPLY", "password=two\\npassword_expiry_utc=5\\n")],
    );
    run(
        &mut fx,
        &[
            "-c",
            helper,
            "-c",
            "credential.https://example.com.username=alice",
            "-c",
            "credential.https://*.org.helper=",
            "credential",
            "fill",
        ],
        "url=https://example.com/path.git\n",
        &[("REPLY", "password=pw\\n")],
    );
    run(
        &mut fx,
        &["-c", helper, "credential", "fill"],
        "protocol=https\nhost=example.com\n",
        &[("REPLY", "quit=1\\n")],
    );
}

#[test]
fn approve_reject_and_errors() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let helper = "credential.helper=$W/dump";
    let full = "protocol=https\nhost=h\npath=p\nusername=u\npassword=p\noauth_refresh_token=t\n";
    run(&mut fx, &["-c", helper, "credential", "approve"], full, &[]);
    run(&mut fx, &["-c", helper, "credential", "reject"], full, &[]);
    run(
        &mut fx,
        &["-c", helper, "credential", "approve"],
        "protocol=https\nhost=h\npassword=p\n",
        &[],
    );
    run(&mut fx, &["credential", "capability"], "", &[]);
    run(&mut fx, &["credential"], "", &[]);
    run(&mut fx, &["credential", "fill"], "protocol=http\n", &[]);
    run(&mut fx, &["credential", "fill"], "host=h\n", &[]);
    run(&mut fx, &["credential", "fill"], "noequals\n", &[]);
    run(
        &mut fx,
        &["credential", "fill"],
        "url=https://one.example.com?%0ahost=two.example.com/\n",
        &[],
    );
    run(
        &mut fx,
        &["-c", "credential.helper=!true", "credential", "fill"],
        "url=https://example%0d.com/\n",
        &[],
    );
}
