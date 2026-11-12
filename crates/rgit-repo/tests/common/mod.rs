// SPDX-License-Identifier: MIT
//! Shared helpers: run the reference git and rgit-repo on the same
//! directory and environment and render both results the same way.

#![allow(dead_code)]

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use rgit_config::{CommandLine, MapEnv};
use rgit_difftest::Fixture;
use rgit_repo::{SetupOptions, StandardProtectedConfig};

pub struct World {
    pub fx: Fixture,
    pub root: PathBuf,
}

impl World {
    pub fn new() -> Option<World> {
        let fx = Fixture::new();
        if fx.is_skipped() {
            return None;
        }
        let root = fx.work_dir();
        Some(World { fx, root })
    }

    pub fn base_env(&self) -> Vec<(String, OsString)> {
        vec![
            ("HOME".into(), self.fx.home_dir().into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_CEILING_DIRECTORIES".into(), self.root.clone().into()),
            ("GIT_AUTHOR_NAME".into(), "A U Thor".into()),
            ("GIT_AUTHOR_EMAIL".into(), "author@example.com".into()),
            ("GIT_COMMITTER_NAME".into(), "C O Mitter".into()),
            ("GIT_COMMITTER_EMAIL".into(), "committer@example.com".into()),
        ]
    }

    pub fn git(&self, cwd: &Path, args: &[&str]) {
        let out = Command::new(self.fx.reference_git())
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .envs(self.base_env())
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    /// git's result: the five setup lines, or the stderr.
    pub fn reference(&self, cwd: &Path, extra: &[(&str, OsString)]) -> String {
        let trace = self.root.join("trace.out");
        let _ = std::fs::remove_file(&trace);
        let out = Command::new(self.fx.reference_git())
            .args(["symbolic-ref", "HEAD"])
            .current_dir(cwd)
            .env_clear()
            .envs(self.base_env())
            .envs(extra.iter().map(|(k, v)| (*k, v)))
            .env("GIT_TRACE_SETUP", &trace)
            .env("GIT_TRACE_BARE", "1")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let mut s: String = std::fs::read_to_string(&trace)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.starts_with("setup: "))
            .map(|l| format!("{l}\n"))
            .collect();
        if s.is_empty() && !out.status.success() {
            return format!("FAIL {stderr}");
        }
        for l in stderr.lines() {
            // symbolic-ref's own complaint about a detached HEAD.
            if l.starts_with("fatal: ref HEAD is not a symbolic ref")
                || l == "fatal: No such ref: HEAD"
            {
                continue;
            }
            s.push_str(&format!("stderr: {l}\n"));
        }
        s
    }

    pub fn ours(&self, cwd: &Path, extra: &[(&str, OsString)]) -> String {
        let mut vars = self.base_env();
        vars.extend(extra.iter().map(|(k, v)| (k.to_string(), v.clone())));
        let env = MapEnv::new(vars);
        let opts = SetupOptions::from_env(&env, cwd.to_path_buf());
        let mut protected = StandardProtectedConfig::new(&env, CommandLine::default());
        match rgit_repo::setup(&opts, &mut protected) {
            Err(e) => {
                let mut s = "FAIL ".to_string();
                for (level, msg) in e.report() {
                    s.push_str(&format!("{} {msg}\n", level.prefix()));
                }
                s
            }
            Ok(s) => {
                let show = |p: Option<&std::ffi::OsStr>| {
                    p.map_or("(null)".to_string(), |p| p.to_string_lossy().into_owned())
                };
                let mut out = format!(
                    "setup: git_dir: {}\nsetup: git_common_dir: {}\nsetup: worktree: {}\nsetup: cwd: {}\nsetup: prefix: {}\n",
                    s.git_dir.display(),
                    s.common_dir.display(),
                    show(s.work_tree.as_deref().map(Path::as_os_str)),
                    s.cwd.display(),
                    show(s.prefix.as_deref()),
                );
                for w in &s.warnings {
                    out.push_str(&format!("stderr: warning: {w}\n"));
                }
                out
            }
        }
    }
}

/// `to` relative to `from` (both absolute, normalised).
pub fn relative(to: &Path, from: &Path) -> PathBuf {
    let t: Vec<Component> = to.components().collect();
    let f: Vec<Component> = from.components().collect();
    let common = t.iter().zip(&f).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..f.len() {
        out.push("..");
    }
    for c in &t[common..] {
        out.push(c.as_os_str());
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}
