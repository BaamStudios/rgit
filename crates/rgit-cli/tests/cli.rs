// SPDX-License-Identifier: MIT
//! End-to-end tests of the `rgit` binary's front end.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const RGIT: &str = env!("CARGO_BIN_EXE_rgit");

fn rgit(args: &[&str]) -> Output {
    cmd(Path::new(RGIT), args)
}

fn cmd(prog: &Path, args: &[&str]) -> Output {
    Command::new(prog)
        .args(args)
        .env_remove("GIT_EXEC_PATH")
        .env_remove("GIT_DIR")
        .env_remove("GIT_TEST_DISALLOW_ABBREVIATED_OPTIONS")
        .output()
        .expect("run rgit")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn version_line() -> String {
    format!("git version 2.56.1.rgit.{}\n", env!("CARGO_PKG_VERSION"))
}

/// A scratch directory with `git` and `git-version` links to the binary.
#[cfg(unix)]
fn install_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for link in ["git", "git-version"] {
        std::os::unix::fs::symlink(RGIT, dir.join(link)).unwrap();
    }
    dir
}

#[test]
fn version_as_rgit_says_not_official() {
    for args in [&["version"][..], &["--version"], &["-v"]] {
        let o = rgit(args);
        assert_eq!(o.status.code(), Some(0));
        assert_eq!(
            text(&o.stdout),
            format!(
                "{}rgit is an independent reimplementation, not the official git.\n",
                version_line()
            )
        );
    }
}

#[cfg(unix)]
#[test]
fn version_as_git_is_one_line() {
    let dir = install_dir("as-git");
    let o = cmd(&dir.join("git"), &["version"]);
    assert_eq!(text(&o.stdout), version_line());
    // dashed invocation runs the command directly
    let o = cmd(&dir.join("git-version"), &[]);
    assert_eq!(text(&o.stdout), version_line());
    let o = cmd(&dir.join("git"), &["--version", "--build-options"]);
    let out = text(&o.stdout);
    assert!(out.starts_with(&version_line()));
    assert!(out.contains("\nsizeof-size_t: "));
}

#[cfg(unix)]
#[test]
fn exec_path() {
    let dir = install_dir("exec-path");
    let o = cmd(&dir.join("git"), &["--exec-path"]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(text(&o.stdout), format!("{}\n", dir.display()));

    let o = cmd(
        &dir.join("git"),
        &["--exec-path=/some/where", "--exec-path"],
    );
    assert_eq!(text(&o.stdout), "/some/where\n");

    let o = Command::new(dir.join("git"))
        .arg("--exec-path")
        .env("GIT_EXEC_PATH", "/from/env")
        .output()
        .unwrap();
    assert_eq!(text(&o.stdout), "/from/env\n");
}

#[test]
fn version_usage() {
    let o = rgit(&["version", "-h"]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(
        text(&o.stdout),
        "usage: git version [--build-options]\n\n    --[no-]build-options  also print build options\n\n"
    );
    let o = rgit(&["version", "--foo"]);
    assert_eq!(o.status.code(), Some(129));
    assert!(text(&o.stderr).starts_with("error: unknown option `foo'\nusage: git version"));
    let o = rgit(&["version", "--bui=x"]);
    assert_eq!(o.status.code(), Some(129));
    assert_eq!(
        text(&o.stderr),
        "error: option `build-options' takes no value\n"
    );
}

#[test]
fn global_option_errors() {
    let cases: &[(&[&str], &str)] = &[
        (&["--bogus"], "unknown option: --bogus\n"),
        (&["-C"], "no directory given for '-C' option\n"),
        (&["-c"], "-c expects a configuration string\n"),
        (
            &["--git-dir"],
            "no directory given for '--git-dir' option\n",
        ),
        (
            &["--work-tree"],
            "no directory given for '--work-tree' option\n",
        ),
        (&["--namespace"], "no namespace given for --namespace\n"),
        (&["--config-env"], "no config key given for --config-env\n"),
        (&["--gi"], "unknown option: --gi\n"),
    ];
    for (args, first) in cases {
        let o = rgit(args);
        assert_eq!(o.status.code(), Some(129), "{args:?}");
        let err = text(&o.stderr);
        assert!(err.starts_with(first), "{args:?}: {err}");
        assert!(err.contains("\nusage: git [-v | --version]"), "{args:?}");
    }
    let o = rgit(&["-C", "/nonexistent/dir", "version"]);
    assert_eq!(o.status.code(), Some(128));
    assert_eq!(
        text(&o.stderr),
        "fatal: cannot change to '/nonexistent/dir': No such file or directory\n"
    );
    let o = rgit(&["--config-env=a.b", "version"]);
    assert_eq!(o.status.code(), Some(128));
    assert_eq!(text(&o.stderr), "fatal: invalid config format: a.b\n");
}

#[test]
fn global_options_accepted() {
    let o = rgit(&[
        "-C",
        ".",
        "-C",
        "",
        "-c",
        "a.b=c",
        "-c",
        "d",
        "-p",
        "-P",
        "--no-pager",
        "--paginate",
        "--git-dir=x",
        "--git-dir",
        "y",
        "--work-tree=z",
        "--bare",
        "--namespace=n",
        "--no-replace-objects",
        "--no-lazy-fetch",
        "--no-optional-locks",
        "--no-advice",
        "--literal-pathspecs",
        "--attr-source",
        "HEAD",
        "--exec-path=/x",
        "version",
    ]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert!(text(&o.stdout).starts_with(&version_line()));
}

#[test]
fn no_command_and_unknown_command() {
    let o = rgit(&[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stdout).starts_with("usage: git [-v | --version]"));
    let o = rgit(&["-h"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(text(&o.stdout).starts_with("usage: git [-v | --version]"));
    let o = rgit(&["nosuchcmd"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        text(&o.stderr),
        "git: 'nosuchcmd' is not a git command. See 'git --help'.\n"
    );
}

#[test]
fn list_cmds() {
    let o = rgit(&["--list-cmds=builtins"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(text(&o.stdout).lines().any(|l| l == "version"));
    let o = rgit(&["--list-cmds=bogus"]);
    assert_eq!(o.status.code(), Some(128));
    assert_eq!(
        text(&o.stderr),
        "fatal: unsupported command listing type 'bogus'\n"
    );
}

/// Writing to a closed pipe kills rgit with SIGPIPE, silently.
#[cfg(unix)]
#[test]
fn dies_quietly_on_sigpipe() {
    use std::os::unix::process::ExitStatusExt;
    // A pipe whose read end belonged to a process that has already exited.
    let mut reader = Command::new("true").stdin(Stdio::piped()).spawn().unwrap();
    let write_end = reader.stdin.take().unwrap();
    reader.wait().unwrap();
    let o = Command::new(RGIT)
        .args(["version", "--build-options"])
        .stdout(Stdio::from(write_end))
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert_eq!(o.status.signal(), Some(libc::SIGPIPE), "{:?}", o.status);
    assert_eq!(text(&o.stderr), "");
}
