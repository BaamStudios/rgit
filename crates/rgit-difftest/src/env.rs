// SPDX-License-Identifier: MIT
//! Locating the binaries and building the deterministic environment.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Environment variable naming the reference git binary.
pub const REFERENCE_GIT_VAR: &str = "RGIT_REFERENCE_GIT";
/// Environment variable naming the rgit binary under test.
pub const RGIT_BIN_VAR: &str = "RGIT_BIN";
/// When set to `1`, a missing reference git fails the test instead of skipping it.
pub const REQUIRE_REFERENCE_VAR: &str = "RGIT_REQUIRE_REFERENCE";
/// When set to `1`, the temporary directory of a failing comparison is kept.
pub const KEEP_VAR: &str = "RGIT_DIFFTEST_KEEP";

/// Identity used for all commits made in fixtures.
pub const AUTHOR_NAME: &str = "Ada Author";
/// Author e-mail used in fixtures.
pub const AUTHOR_EMAIL: &str = "ada@example.com";
/// Committer name used in fixtures.
pub const COMMITTER_NAME: &str = "Cy Committer";
/// Committer e-mail used in fixtures.
pub const COMMITTER_EMAIL: &str = "cy@example.com";
/// Start of the fixture clock (seconds since the epoch, UTC): 2023-11-14 22:13:20.
pub const START_TIME: i64 = 1_700_000_000;
/// Default branch configured in the fixture's global config.
pub const DEFAULT_BRANCH: &str = "main";

fn env_flag(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| v == "1")
}

pub(crate) fn keep_requested() -> bool {
    env_flag(KEEP_VAR)
}

/// Finds `name` in `PATH`.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// Resolves the reference git binary.
///
/// Order: `$RGIT_REFERENCE_GIT`, then `upstream/git/git` in the workspace
/// (built by `scripts/fetch-upstream.sh`), then `git` from `PATH`.
/// Returns `None` when none is found.
///
/// # Panics
/// If `$RGIT_REFERENCE_GIT` is set but does not name an executable file.
pub fn reference_git() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(REFERENCE_GIT_VAR).filter(|v| !v.is_empty()) {
        let p = PathBuf::from(p);
        assert!(
            is_executable(&p),
            "{REFERENCE_GIT_VAR}={} is not an executable file",
            p.display()
        );
        return Some(p);
    }
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let upstream = workspace.join("upstream/git/git");
    if is_executable(&upstream) {
        return Some(upstream.canonicalize().unwrap_or(upstream));
    }
    which(if cfg!(windows) { "git.exe" } else { "git" })
}

/// Resolves the reference git, or decides to skip.
///
/// # Panics
/// If no reference git is found and `RGIT_REQUIRE_REFERENCE=1`.
pub(crate) fn reference_git_or_skip() -> Option<PathBuf> {
    let found = reference_git();
    if found.is_none() {
        assert!(
            !env_flag(REQUIRE_REFERENCE_VAR),
            "no reference git found ({REFERENCE_GIT_VAR}, upstream/git/git, PATH) \
             and {REQUIRE_REFERENCE_VAR}=1"
        );
        eprintln!("rgit-difftest: skipping, no reference git found (set {REFERENCE_GIT_VAR})");
    }
    found
}

/// The rgit binary named by `$RGIT_BIN`, if set.
pub fn rgit_from_env() -> Option<PathBuf> {
    std::env::var_os(RGIT_BIN_VAR)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// The deterministic environment shared by all commands in a fixture.
#[derive(Clone, Debug)]
pub(crate) struct BaseEnv {
    pub home: PathBuf,
    pub ceiling: PathBuf,
    pub time: i64,
    pub extra: Vec<(OsString, OsString)>,
}

impl BaseEnv {
    /// Applies the environment to `cmd`, clearing everything inherited except
    /// `PATH`. `bin_dir`, if given, is put first in `PATH` (it holds a `git`
    /// link to the binary under test, for commands that re-invoke `git`).
    pub fn apply(&self, cmd: &mut Command, bin_dir: Option<&Path>) {
        cmd.env_clear();
        let mut path: Vec<PathBuf> = bin_dir.into_iter().map(Path::to_path_buf).collect();
        if let Some(p) = std::env::var_os("PATH") {
            path.extend(std::env::split_paths(&p));
        }
        if let Ok(p) = std::env::join_paths(path) {
            cmd.env("PATH", p);
        }
        #[cfg(windows)]
        for key in ["SYSTEMROOT", "TEMP", "TMP"] {
            if let Some(v) = std::env::var_os(key) {
                cmd.env(key, v);
            }
        }
        let date = format!("{} +0000", self.time);
        let fixed: [(&str, &OsStr); 21] = [
            ("HOME", self.home.as_os_str()),
            ("GIT_CONFIG_NOSYSTEM", OsStr::new("1")),
            ("GIT_ATTR_NOSYSTEM", OsStr::new("1")),
            ("GIT_CEILING_DIRECTORIES", self.ceiling.as_os_str()),
            ("GIT_AUTHOR_NAME", OsStr::new(AUTHOR_NAME)),
            ("GIT_AUTHOR_EMAIL", OsStr::new(AUTHOR_EMAIL)),
            ("GIT_AUTHOR_DATE", OsStr::new(&date)),
            ("GIT_COMMITTER_NAME", OsStr::new(COMMITTER_NAME)),
            ("GIT_COMMITTER_EMAIL", OsStr::new(COMMITTER_EMAIL)),
            ("GIT_COMMITTER_DATE", OsStr::new(&date)),
            ("TZ", OsStr::new("UTC")),
            ("LC_ALL", OsStr::new("C")),
            ("LANG", OsStr::new("C")),
            ("LANGUAGE", OsStr::new("C")),
            ("TERM", OsStr::new("dumb")),
            ("PAGER", OsStr::new("cat")),
            ("GIT_PAGER", OsStr::new("cat")),
            ("EDITOR", OsStr::new("true")),
            ("GIT_EDITOR", OsStr::new("true")),
            ("GIT_MERGE_AUTOEDIT", OsStr::new("no")),
            ("GIT_TERMINAL_PROMPT", OsStr::new("0")),
        ];
        for (k, v) in fixed {
            cmd.env(k, v);
        }
        for (k, v) in &self.extra {
            cmd.env(k, v);
        }
    }
}

/// Content of `$HOME/.gitconfig` in every fixture.
pub(crate) fn global_config() -> String {
    format!("[init]\n\tdefaultBranch = {DEFAULT_BRANCH}\n")
}
