// SPDX-License-Identifier: MIT
//! `push` over smart HTTP against the reference `git http-backend`: the
//! same pushes go from one client repository to two identical servers,
//! one with git and one with rgit; output (with the URL replaced) and the
//! server refs must match.

#[path = "../../rgit-http/tests/support/mod.rs"]
mod support;

use std::path::Path;
use std::process::Command;

use support::Server;

/// A command with an isolated configuration and fixed identity and dates.
fn cmd(program: &str, dir: &Path, home: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(program);
    c.current_dir(dir)
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE");
    c
}

fn output(c: &mut Command) -> (i32, String, String) {
    let o = c.output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn have_git() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn push_over_smart_http_same_as_git() {
    if !have_git() {
        return;
    }
    let server = Server::start(false);
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    for name in ["g.git", "r.git"] {
        support::run(&mut support::git(
            server.root(),
            &["init", "-q", "--bare", name],
        ));
        support::run(&mut support::git(
            &server.root().join(name),
            &["config", "http.receivepack", "true"],
        ));
    }
    let work = home.join("w");
    std::fs::create_dir_all(&work).unwrap();
    let git = |args: &[&str]| {
        let (code, _, err) = output(&mut cmd("git", &work, home, args));
        assert_eq!(code, 0, "git {args:?}: {err}");
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(work.join("f"), "one\n").unwrap();
    git(&["add", "f"]);
    git(&["commit", "-q", "-m", "one"]);
    git(&["remote", "add", "g", &server.url("/smart/g.git")]);
    git(&["remote", "add", "r", &server.url("/smart/r.git")]);

    let rgit = env!("CARGO_BIN_EXE_rgit");
    let compare = |args: &[&str]| {
        let run = |program: &str, remote: &str| {
            let mut argv = vec!["push", remote];
            argv.extend_from_slice(args);
            let (code, out, err) = output(&mut cmd(program, &work, home, &argv));
            let fix = |s: String| {
                s.replace("/smart/r.git", "/smart/X")
                    .replace("/smart/g.git", "/smart/X")
                    .replace("remotes/r/", "remotes/X/")
                    .replace("remotes/g/", "remotes/X/")
            };
            (code, fix(out), fix(err))
        };
        assert_eq!(run("git", "g"), run(rgit, "r"), "push {args:?}");
        let refs = |name: &str| {
            support::run(&mut support::git(
                &server.root().join(name),
                &["for-each-ref"],
            ))
        };
        assert_eq!(
            refs("g.git"),
            refs("r.git"),
            "server refs after push {args:?}"
        );
    };
    compare(&["main"]);
    compare(&["main:side", "main:refs/tags/t"]);
    std::fs::write(work.join("f"), "two\n").unwrap();
    git(&["commit", "-q", "-a", "-m", "two"]);
    compare(&["main"]);
    compare(&["--porcelain", "main"]);
    compare(&[":side"]);
    git(&["reset", "-q", "--hard", "HEAD~1"]);
    compare(&["main"]);
    compare(&["-f", "main"]);
    compare(&["-v", "main"]);
    let tracking = |r: &str| {
        output(&mut cmd(
            "git",
            &work,
            home,
            &["rev-parse", &format!("refs/remotes/{r}/main")],
        ))
        .1
    };
    assert_eq!(tracking("g"), tracking("r"));
}
