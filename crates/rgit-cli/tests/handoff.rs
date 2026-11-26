// SPDX-License-Identifier: MIT
//! `rgit svn`, `rgit p4` and the other foreign-SCM and legacy script
//! commands hand off to an installed git's `git-<cmd>` (#710): arguments,
//! environment and exit code pass through, the program calls rgit as `git`,
//! and a missing program is a clear fatal error with exit code 128.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const RGIT: &str = env!("CARGO_BIN_EXE_rgit");

/// A fake hand-off program: prints its name, arguments, some environment
/// and what `git --version` (the `git` its `PATH` finds) says, then exits 5.
const FAKE: &str = "#!/bin/sh\n\
echo \"$(basename \"$0\"): $*\"\n\
echo \"FOO=$FOO\"\n\
test -n \"$GIT_DIR\" && echo \"GIT_DIR=$GIT_DIR\"\n\
test -n \"$GIT_CONFIG_PARAMETERS\" && echo \"cfg=$GIT_CONFIG_PARAMETERS\"\n\
git version | sed 's/ (.*//'\n\
exit 5\n";

/// A fake installed git: only answers `--exec-path`.
const FAKE_GIT: &str = "#!/bin/sh\n\
test \"$1\" = --exec-path || exit 1\n\
echo \"$(dirname \"$0\")/../core\"\n";

fn write_exe(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct Env {
    tmp: tempfile::TempDir,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("bin")).unwrap();
        // `sh`, `sed`, `basename` for the fake scripts, but no git there.
        for tool in ["sh", "sed", "basename", "dirname"] {
            if let Some(p) = which(tool) {
                std::os::unix::fs::symlink(p, tmp.path().join("bin").join(tool)).unwrap();
            }
        }
        Env { tmp }
    }

    fn dir(&self) -> &Path {
        self.tmp.path()
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(RGIT)
            .args(args)
            .current_dir(self.dir())
            .env_clear()
            .env("HOME", self.dir())
            .env("XDG_CONFIG_HOME", self.dir().join(".config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("PATH", self.dir().join("bin"))
            .env("FOO", "bar")
            .env("LANG", "C")
            .output()
            .unwrap()
    }
}

fn which(tool: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(tool))
        .find(|p| p.is_file())
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn svn_runs_program_on_path_with_args_env_and_exit_code() {
    let env = Env::new();
    write_exe(&env.dir().join("bin/git-svn"), FAKE);
    let o = env.run(&["-c", "x.y=z", "svn", "clone", "a b", "--stdlayout"]);
    assert_eq!(o.status.code(), Some(5), "stderr: {}", stderr(&o));
    let out = stdout(&o);
    assert!(out.contains("git-svn: clone a b --stdlayout"), "{out}");
    assert!(out.contains("FOO=bar"), "{out}");
    assert!(out.contains("cfg='x.y'='z'"), "{out}");
    // The program's `git` is rgit.
    let version = stdout(&Command::new(RGIT).arg("version").output().unwrap());
    let version = version.lines().next().unwrap().trim();
    assert!(out.contains(version), "{out}");
}

#[test]
fn p4_found_in_exec_path_of_installed_git() {
    let env = Env::new();
    write_exe(&env.dir().join("bin/git"), FAKE_GIT);
    write_exe(&env.dir().join("core/git-p4"), FAKE);
    let o = env.run(&["--git-dir=/nowhere", "p4", "sync", "//depot/x@all"]);
    assert_eq!(o.status.code(), Some(5), "stderr: {}", stderr(&o));
    let out = stdout(&o);
    assert!(out.contains("git-p4: sync //depot/x@all"), "{out}");
    assert!(out.contains("GIT_DIR=/nowhere"), "{out}");
    // `git` in the program is rgit, not the installed git (which only
    // answers `--exec-path`).
    assert!(out.contains("git version"), "{out}");
}

#[test]
fn installed_git_exec_path_only_for_handed_off_commands() {
    let env = Env::new();
    write_exe(&env.dir().join("bin/git"), FAKE_GIT);
    write_exe(&env.dir().join("core/git-foo"), FAKE);
    let o = env.run(&["foo"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).contains("'foo' is not a git command"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn missing_svn_and_p4_are_clear_fatal_errors() {
    let env = Env::new();
    let o = env.run(&["svn", "clone", "x"]);
    assert_eq!(o.status.code(), Some(128));
    assert_eq!(
        stderr(&o),
        "fatal: 'svn' is not built into rgit; install git-svn (package \"git-svn\" / \"git\" \
         with Subversion Perl bindings) to use it\n"
    );
    let o = env.run(&["p4", "-h"]);
    assert_eq!(o.status.code(), Some(128));
    assert_eq!(
        stderr(&o),
        "fatal: 'p4' is not built into rgit; install git-p4 (package \"git-p4\" / \"git\" \
         with Python) to use it\n"
    );
}

#[test]
fn missing_legacy_scripts_are_clear_fatal_errors() {
    let env = Env::new();
    for cmd in [
        "cvsimport",
        "cvsexportcommit",
        "cvsserver",
        "archimport",
        "quiltimport",
        "instaweb",
        "gui",
        "citool",
        "web--browse",
    ] {
        let o = env.run(&[cmd]);
        assert_eq!(o.status.code(), Some(128), "{cmd}");
        let err = stderr(&o);
        assert!(
            err.starts_with(&format!(
                "fatal: '{cmd}' is not built into rgit; install git-{cmd} ("
            )),
            "{err}"
        );
    }
}

#[test]
fn alias_wins_over_missing_handoff() {
    let env = Env::new();
    let o = Command::new(RGIT)
        .args(["-c", "alias.svn=version", "svn"])
        .current_dir(env.dir())
        .env_clear()
        .env("HOME", env.dir())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("PATH", env.dir().join("bin"))
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(stdout(&o).starts_with("git version"), "{}", stdout(&o));
}
