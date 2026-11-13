// SPDX-License-Identifier: MIT
//! The pager against the reference git, with stdout and stderr on a
//! pseudo terminal (the pager only runs when stdout is a terminal).
//!
//! Each case runs with git and with rgit in the same repository; compared
//! are the exit status, the terminal output (without carriage returns)
//! and the files the pager commands write.

#![cfg(unix)]

use std::io::Read;
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A pseudo terminal: (master, slave).
fn openpty() -> (OwnedFd, OwnedFd) {
    let (mut master, mut slave) = (0, 0);
    // SAFETY: openpty writes two new descriptors, which we then own.
    let r = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    assert_eq!(r, 0, "openpty failed");
    // SAFETY: both descriptors were just created and are not owned elsewhere.
    unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) }
}

/// Run `cmd` with stdin, stdout and stderr on a terminal; returns the exit
/// status (128 + signal when killed) and what it wrote to the terminal.
fn run_tty(mut cmd: Command) -> (i32, String) {
    let (master, slave) = openpty();
    cmd.stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave));
    let mut child = cmd.spawn().expect("spawn");
    // Drop our copies of the slave (moved into `cmd`) so that reading
    // ends when the child and its pager have exited.
    drop(cmd);
    let reader = std::thread::spawn(move || {
        let mut f = std::fs::File::from(master);
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        // Reading fails with EIO once no process has the terminal open.
        while let Ok(n) = f.read(&mut buf) {
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        out
    });
    let status = child.wait().expect("wait");
    let out = reader.join().expect("reader");
    let code = status.code().unwrap_or_else(|| {
        use std::os::unix::process::ExitStatusExt;
        128 + status.signal().unwrap_or(0)
    });
    (code, String::from_utf8_lossy(&out).replace('\r', ""))
}

struct Lab {
    _dir: tempfile::TempDir,
    root: PathBuf,
    git: PathBuf,
}

impl Lab {
    fn new() -> Option<Lab> {
        let git = rgit_difftest::env::reference_git()?;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("repo/sub")).unwrap();
        let lab = Lab {
            _dir: dir,
            root,
            git,
        };
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["commit", "-q", "--allow-empty", "-m", "initial"],
            &["branch", "side"],
        ] {
            let st = lab.cmd(&lab.git, &[], args).status().unwrap();
            assert!(st.success(), "{args:?}");
        }
        Some(lab)
    }

    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    fn cmd(&self, bin: &Path, env: &[(&str, &str)], args: &[&str]) -> Command {
        let mut c = Command::new(bin);
        c.env_clear();
        if let Some(p) = std::env::var_os("PATH") {
            c.env("PATH", p);
        }
        c.env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", &self.root)
            .env("TERM", "xterm")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1112911993 -0700")
            .env("GIT_COMMITTER_DATE", "1112911993 -0700");
        for (k, v) in env {
            c.env(k, v);
        }
        c.args(args).current_dir(self.repo());
        c
    }

    /// Run with `bin` on a terminal; the outcome as text. `files` (relative
    /// to the repository) are reported and removed.
    fn outcome(&self, bin: &Path, env: &[(&str, &str)], args: &[&str], files: &[&str]) -> String {
        let (code, tty) = run_tty(self.cmd(bin, env, args));
        let mut s = format!("exit {code}\n--- terminal\n{tty}");
        for f in files {
            let p = self.repo().join(f);
            match std::fs::read(&p) {
                Ok(data) => {
                    s.push_str(&format!("--- {f}\n{}", String::from_utf8_lossy(&data)));
                    std::fs::remove_file(&p).unwrap();
                }
                Err(_) => s.push_str(&format!("--- {f} (missing)\n")),
            }
        }
        s
    }

    /// Run with git and rgit and compare.
    fn same(&self, env: &[(&str, &str)], args: &[&str], files: &[&str]) -> Result<(), String> {
        let want = self.outcome(&self.git, env, args, files);
        let got = self.outcome(Path::new(env!("CARGO_BIN_EXE_rgit")), env, args, files);
        if got == want {
            Ok(())
        } else {
            Err(format!(
                "{args:?} with {env:?}\n=== git\n{want}=== rgit\n{got}"
            ))
        }
    }
}

type Case<'a> = (&'a [(&'a str, &'a str)], &'a [&'a str], &'a [&'a str]);

fn check_all(lab: &Lab, cases: &[Case<'_>]) {
    let failures: Vec<String> = cases
        .iter()
        .filter_map(|(env, args, files)| lab.same(env, args, files).err())
        .collect();
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// Outside the repository, so that commands never see the file.
const OUT: &[&str] = &["../paginated.out"];
const PAGER: (&str, &str) = ("PAGER", "cat >../paginated.out");

#[test]
fn which_commands_page() {
    let Some(lab) = Lab::new() else { return };
    let log = &["log", "--no-decorate", "--oneline"][..];
    check_all(
        &lab,
        &[
            (&[PAGER], log, OUT),
            (&[PAGER], &["rev-list", "HEAD"], OUT),
            (&[PAGER], &["--paginate", "rev-list", "HEAD"], OUT),
            (&[PAGER], &["-p", "rev-list", "HEAD"], OUT),
            (&[PAGER], &["--no-pager", "log", "--no-decorate"], OUT),
            (&[PAGER], &["-P", "log", "--no-decorate"], OUT),
            (
                &[PAGER],
                &["-c", "pager.log=false", "log", "--no-decorate"],
                OUT,
            ),
            (
                &[PAGER],
                &["-c", "pager.log=false", "-p", "log", "--no-decorate"],
                OUT,
            ),
            (&[PAGER], &["-c", "pager.rev-list", "rev-list", "HEAD"], OUT),
            (
                &[PAGER],
                &["-c", "pager.rev-list=true", "rev-list", "HEAD"],
                OUT,
            ),
            (&[PAGER], &["branch"], OUT),
            (&[PAGER], &["branch", "-v"], OUT),
            (&[PAGER], &["-c", "pager.branch=false", "branch"], OUT),
            (&[PAGER], &["--no-pager", "branch"], OUT),
            (&[PAGER], &["branch", "--show-current"], OUT),
            (
                &[PAGER],
                &["-c", "pager.branch", "branch", "--set-upstream-to=side"],
                OUT,
            ),
            (
                &[PAGER],
                &["-c", "pager.config", "config", "foo.bar", "bar"],
                OUT,
            ),
            (
                &[PAGER],
                &["-c", "pager.config", "config", "--get", "foo.bar"],
                OUT,
            ),
            (&[PAGER], &["config", "--get-all", "foo.bar"], OUT),
            (
                &[PAGER],
                &["-c", "pager.config=false", "config", "--get-all", "foo.bar"],
                OUT,
            ),
            (&[PAGER], &["config", "--list"], OUT),
            (&[PAGER], &["config", "list"], OUT),
            (&[PAGER], &["config", "get", "foo.bar"], OUT),
            (&[PAGER], &["reflog", "--no-decorate"], OUT),
            (&[PAGER], &["reflog", "show", "--no-decorate"], OUT),
            (&[PAGER], &["reflog", "exists", "HEAD"], OUT),
            (&[PAGER], &["diff"], OUT),
            (&[PAGER], &["diff", "--exit-code"], OUT),
            (&[PAGER], &["-p", "diff", "--quiet"], OUT),
            (
                &[PAGER],
                &["-c", "pager.status=true", "status", "--porcelain"],
                OUT,
            ),
        ],
    );
}

#[test]
fn pager_choice_and_environment() {
    let Some(lab) = Lab::new() else { return };
    let log = &["log", "--no-decorate", "--oneline"][..];
    let used = &["used"][..];
    check_all(
        &lab,
        &[
            (&[("PAGER", "wc >used")], log, used),
            (&[("PAGER", "wc"), ("GIT_PAGER", "wc >used")], log, used),
            (
                &[("PAGER", "wc")],
                &["-c", "core.pager=wc >used", "log", "--no-decorate"],
                used,
            ),
            (
                &[("GIT_PAGER", "sed s/^/env:/ >used")],
                &[
                    "-c",
                    "pager.log=exit 1",
                    "log",
                    "--no-decorate",
                    "--oneline",
                ],
                used,
            ),
            (
                &[],
                &[
                    "-c",
                    "core.pager=exit 1",
                    "-c",
                    "pager.log=sed s/^/foo:/ >used",
                    "log",
                    "--oneline",
                    "--no-decorate",
                ],
                used,
            ),
            // `--paginate` uses the default pager, not `pager.<cmd>`.
            (
                &[],
                &[
                    "-c",
                    "core.pager=sed s/^/C:/ >used",
                    "-c",
                    "pager.log=sed s/^/L:/ >used",
                    "-p",
                    "log",
                    "--oneline",
                    "--no-decorate",
                ],
                used,
            ),
            // No pager for an empty command or `cat`.
            (&[("GIT_PAGER", ""), ("PAGER", "wc >used")], log, used),
            (&[("GIT_PAGER", "cat"), ("PAGER", "wc >used")], log, used),
            (&[("PAGER", "")], log, used),
            // LESS and LV are set when unset; GIT_PAGER_IN_USE is not.
            (
                &[(
                    "PAGER",
                    "env | grep -E '^(LESS|LV|GIT_PAGER_IN_USE)=' | sort >used; wc",
                )],
                log,
                used,
            ),
            (
                &[
                    ("LESS", "-x"),
                    ("PAGER", "env | grep -E '^(LESS|LV)=' | sort >used; wc"),
                ],
                log,
                used,
            ),
            // stderr goes to the pager too.
            (
                &[("PAGER", "cat >used")],
                &["-p", "rev-parse", "--verify", "nonexistent"],
                used,
            ),
            // `-p` without a command pages the usage.
            (&[("PAGER", "wc -l >/dev/null; touch used")], &["-p"], used),
            // Colours for the pager.
            (
                &[PAGER],
                &["-c", "color.ui=auto", "log", "--no-decorate", "--oneline"],
                OUT,
            ),
            (
                &[PAGER],
                &[
                    "-c",
                    "color.ui=auto",
                    "-c",
                    "color.pager=false",
                    "log",
                    "--no-decorate",
                ],
                OUT,
            ),
            (
                &[PAGER, ("TERM", "dumb")],
                &["log", "--no-decorate", "--oneline"],
                OUT,
            ),
            (&[PAGER], &["-c", "color.ui=never", "branch"], OUT),
            (
                &[],
                &["--no-pager", "log", "--no-decorate", "--oneline"],
                &[],
            ),
        ],
    );
}

#[test]
fn pager_errors_and_exit() {
    let Some(lab) = Lab::new() else { return };
    let msg = lab.root.join("msg");
    std::fs::write(&msg, "this is a long commit message\n".repeat(20000)).unwrap();
    let msg = msg.display().to_string();
    let st = lab
        .cmd(
            &lab.git,
            &[],
            &["commit", "-q", "--allow-empty", "-F", &msg],
        )
        .status()
        .unwrap();
    assert!(st.success());
    let used = &["pager-used"][..];
    check_all(
        &lab,
        &[
            (
                &[],
                &[
                    "-c",
                    "core.pager=does-not-exist",
                    "log",
                    "--no-decorate",
                    "-1",
                ],
                &[],
            ),
            (
                &[],
                &[
                    "-c",
                    "pager.log=invalid-pager",
                    "log",
                    "--no-decorate",
                    "-1",
                ],
                &[],
            ),
            // The pager's exit code does not matter.
            (
                &[],
                &[
                    "-c",
                    "core.pager=wc >pager-used; exit 1",
                    "log",
                    "--no-decorate",
                ],
                used,
            ),
            // A pager that quits early: we die of SIGPIPE after it exits.
            (
                &[],
                &[
                    "-c",
                    "core.pager=>pager-used; head -n 1 >/dev/null; exit 0",
                    "log",
                    "--no-decorate",
                ],
                used,
            ),
            (
                &[],
                &[
                    "-c",
                    "core.pager=>pager-used; head -n 1 >/dev/null; exit 1",
                    "log",
                    "--no-decorate",
                ],
                used,
            ),
        ],
    );
}

#[test]
fn pager_for_external_commands() {
    let Some(lab) = Lab::new() else { return };
    let tools = lab.root.join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    rgit_difftest::write_executable(
        tools.join("git-envdump"),
        "#!/bin/sh\necho \"in-use=$GIT_PAGER_IN_USE pager=$GIT_PAGER\"\n",
    );
    let exec = format!("--exec-path={}", tools.display());
    let e = exec.as_str();
    let out = &["out"][..];
    check_all(
        &lab,
        &[
            (&[("PAGER", "cat >out")], &[e, "-p", "envdump"], out),
            (&[("PAGER", "cat >out")], &[e, "envdump"], out),
            (
                &[],
                &[e, "-c", "pager.envdump=sed s/^/x:/ >out", "envdump"],
                out,
            ),
            (
                &[("PAGER", "cat >out")],
                &[e, "-c", "pager.envdump=false", "envdump"],
                out,
            ),
            (&[("PAGER", "cat >out")], &[e, "--no-pager", "envdump"], out),
        ],
    );
}

/// The pager starts in the directory the command was started in.
#[test]
fn pager_cwd() {
    let Some(lab) = Lab::new() else { return };
    check_all(
        &lab,
        &[(
            &[],
            &[
                "-C",
                "sub",
                "-c",
                "core.pager=cat >cwd-retained",
                "-p",
                "rev-parse",
                "HEAD",
            ],
            &["sub/cwd-retained", "cwd-retained"],
        )],
    );
}
