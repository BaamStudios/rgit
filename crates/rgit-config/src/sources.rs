// SPDX-License-Identifier: MIT
//! The standard configuration stack: system, global, local, worktree and
//! command line.

use std::path::{Path, PathBuf};

use crate::cmdline::CommandLine;
use crate::env::Environment;
use crate::error::Error;
use crate::origin::Scope;
use crate::set::{ConfigSet, LoadOptions, Loader, SourceSpec};
use crate::value::{bytes_to_path, parse_bool};

/// Compiled-in system configuration file (`$(prefix)/etc/gitconfig` with the
/// `/usr` prefix mapped to `/etc`, as distributions build git).
pub const DEFAULT_SYSTEM_CONFIG: &str = "/etc/gitconfig";

/// The system file to read, honouring `GIT_CONFIG_NOSYSTEM` and
/// `GIT_CONFIG_SYSTEM`. `None` means "do not read".
pub fn system_config_path(env: &dyn Environment) -> Option<PathBuf> {
    if env_bool(env, "GIT_CONFIG_NOSYSTEM") {
        return None;
    }
    match env.var("GIT_CONFIG_SYSTEM") {
        Some(p) => Some(PathBuf::from(p)),
        None => Some(PathBuf::from(DEFAULT_SYSTEM_CONFIG)),
    }
}

/// The global files in reading order: `$XDG_CONFIG_HOME/git/config` (or
/// `~/.config/git/config`) then `~/.gitconfig`; or only `GIT_CONFIG_GLOBAL`.
pub fn global_config_paths(env: &dyn Environment) -> Vec<PathBuf> {
    if let Some(p) = env.var("GIT_CONFIG_GLOBAL") {
        return vec![PathBuf::from(p)];
    }
    let mut out = Vec::new();
    let home = env
        .var_bytes("HOME")
        .filter(|h| !h.is_empty())
        .map(|h| bytes_to_path(&h));
    match env.var_bytes("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        Some(x) => out.push(bytes_to_path(&x).join("git").join("config")),
        None => {
            if let Some(h) = &home {
                out.push(h.join(".config").join("git").join("config"));
            }
        }
    }
    if let Some(h) = home {
        out.push(h.join(".gitconfig"));
    }
    out
}

/// `git_env_bool`: an unset or unparsable variable is false.
fn env_bool(env: &dyn Environment, name: &str) -> bool {
    env.var_bytes(name)
        .and_then(|v| parse_bool(Some(&v)))
        .unwrap_or(false)
}

/// Repository locations for the local and worktree scopes.
#[derive(Debug, Clone)]
pub struct RepoConfig {
    /// The common directory (holds `config`).
    pub common_dir: PathBuf,
    /// The worktree's git directory (holds `config.worktree`); equal to the
    /// common directory for the main worktree.
    pub git_dir: PathBuf,
    /// Read `config.worktree`: `None` decides from `extensions.worktreeConfig`
    /// in the local file.
    pub worktree_config: Option<bool>,
}

impl RepoConfig {
    /// For a repository whose git directory is `git_dir` (main worktree).
    pub fn new(git_dir: impl Into<PathBuf>) -> Self {
        let git_dir = git_dir.into();
        RepoConfig {
            common_dir: git_dir.clone(),
            git_dir,
            worktree_config: None,
        }
    }
}

/// Which scopes to read; all by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopeFilter {
    /// System scope.
    pub system: bool,
    /// Global scope.
    pub global: bool,
    /// Local scope.
    pub local: bool,
    /// Worktree scope.
    pub worktree: bool,
    /// Command scope.
    pub command: bool,
}

impl Default for ScopeFilter {
    fn default() -> Self {
        ScopeFilter {
            system: true,
            global: true,
            local: true,
            worktree: true,
            command: true,
        }
    }
}

impl ScopeFilter {
    /// Only one scope.
    pub fn only(scope: Scope) -> Self {
        ScopeFilter {
            system: scope == Scope::System,
            global: scope == Scope::Global,
            local: scope == Scope::Local,
            worktree: scope == Scope::Worktree,
            command: scope == Scope::Command,
        }
    }
}

/// Load the full stack (system, global, local, worktree, command line) in
/// git's order into `set`. On error `set` keeps the entries read so far.
pub fn load_standard(
    opts: LoadOptions<'_>,
    repo: Option<&RepoConfig>,
    command_line: &CommandLine,
    filter: ScopeFilter,
    set: &mut ConfigSet,
) -> Result<(), Error> {
    let env = opts.env;
    let mut loader = Loader::new(opts);
    if filter.system {
        if let Some(p) = system_config_path(env) {
            loader.add(file(p, Scope::System, false));
        }
    }
    if filter.global {
        for p in global_config_paths(env) {
            loader.add(file(p, Scope::Global, false));
        }
    }
    if let Some(repo) = repo {
        let local = repo.common_dir.join("config");
        let worktree = match repo.worktree_config {
            Some(b) => b,
            None => worktree_config_enabled(&local),
        };
        if filter.local {
            loader.add(file(local, Scope::Local, true));
        }
        if filter.worktree && worktree {
            loader.add(file(
                repo.git_dir.join("config.worktree"),
                Scope::Worktree,
                true,
            ));
        }
    }
    if filter.command {
        loader.add(SourceSpec::CommandLine(command_line.clone()));
    }
    loader.load_into(set)
}

fn file(path: PathBuf, scope: Scope, required: bool) -> SourceSpec {
    SourceSpec::File {
        path,
        scope,
        required,
    }
}

/// Whether the local file enables `extensions.worktreeConfig`.
fn worktree_config_enabled(local: &Path) -> bool {
    let Ok(data) = std::fs::read(local) else {
        return false;
    };
    crate::set::parse_bytes(&data, crate::Origin::File(local.into()), Scope::Local)
        .ok()
        .and_then(|set| set.bool("extensions.worktreeConfig").ok().flatten())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::MapEnv;

    #[test]
    fn paths() {
        let env = MapEnv::new([("HOME", "/h")]);
        assert_eq!(
            global_config_paths(&env),
            vec![
                PathBuf::from("/h/.config/git/config"),
                PathBuf::from("/h/.gitconfig")
            ]
        );
        let env = MapEnv::new([("HOME", "/h"), ("XDG_CONFIG_HOME", "/x")]);
        assert_eq!(global_config_paths(&env)[0], PathBuf::from("/x/git/config"));
        let env = MapEnv::new([("HOME", "/h"), ("GIT_CONFIG_GLOBAL", "/g")]);
        assert_eq!(global_config_paths(&env), vec![PathBuf::from("/g")]);
        let env = MapEnv::new([("GIT_CONFIG_NOSYSTEM", "1"), ("GIT_CONFIG_SYSTEM", "/s")]);
        assert_eq!(system_config_path(&env), None);
        let env = MapEnv::new([("GIT_CONFIG_SYSTEM", "/s")]);
        assert_eq!(system_config_path(&env), Some(PathBuf::from("/s")));
    }
}
