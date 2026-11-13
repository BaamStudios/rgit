// SPDX-License-Identifier: MIT
//! State set up by the global options and handed to every command.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Everything the global options (`git [<global options>] <command>`)
/// determine. Nothing is written to the process environment (that needs
/// `unsafe` in edition 2024); child processes get [`Context::child_env`].
#[derive(Debug, Clone, Default)]
pub struct Context {
    /// argv[0] was `git` or `git-<cmd>`, not `rgit`.
    pub invoked_as_git: bool,
    /// Directory with the dashed programs (`git --exec-path`).
    pub exec_path: PathBuf,
    /// `--exec-path=<path>` was given.
    pub exec_path_overridden: bool,
    /// `-c` / `--config-env` entries in order, as (key, value); a value of
    /// `None` means the `-c key` form (boolean true).
    pub config_parameters: Vec<(String, Option<String>)>,
    /// `--git-dir` (or the directory chosen by `--bare`), unresolved.
    pub git_dir: Option<String>,
    /// `--work-tree`, unresolved (relative to the final working directory).
    pub work_tree: Option<String>,
    /// `--namespace`.
    pub namespace: Option<String>,
    /// `--bare` was given.
    pub bare: bool,
    /// `-p` (`Some(true)`) or `-P` (`Some(false)`), see [`crate::pager`].
    pub paginate: Option<bool>,
    pub no_replace_objects: bool,
    pub no_lazy_fetch: bool,
    pub no_optional_locks: bool,
    pub no_advice: bool,
    pub literal_pathspecs: bool,
    pub glob_pathspecs: bool,
    pub noglob_pathspecs: bool,
    pub icase_pathspecs: bool,
    /// `--attr-source`.
    pub attr_source: Option<String>,
    /// The command being run (for `pager.<cmd>`).
    pub command: String,
}

/// Quote `s` for the shell like git's `sq_quote`: `'...'`, with `'` and `!`
/// escaped outside the quotes.
pub fn sq_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\'' | '!' => {
                out.push('\'');
                out.push('\\');
                out.push(c);
                out.push('\'');
            }
            _ => out.push(c),
        }
    }
    out.push('\'');
    out
}

impl Context {
    /// The advice settings (`advice.*`, `GIT_ADVICE`, `--no-advice`,
    /// `color.advice`). An `advice.*` value that is not a boolean is fatal.
    pub fn advisor(
        &self,
        config: &rgit_config::ConfigSet,
    ) -> Result<rgit_messages::Advisor, crate::Die> {
        self.advisor_with_env(config, &rgit_config::SystemEnv::default())
    }

    /// [`Context::advisor`] with the given environment.
    pub fn advisor_with_env(
        &self,
        config: &rgit_config::ConfigSet,
        env: &dyn rgit_config::Environment,
    ) -> Result<rgit_messages::Advisor, crate::Die> {
        rgit_messages::Advisor::new(config, env, self.no_advice).map_err(crate::Die::Fatal)
    }

    /// The value of `GIT_CONFIG_PARAMETERS` children should see: the
    /// inherited value followed by our `-c`/`--config-env` entries.
    pub fn config_parameters_env(&self) -> Option<String> {
        let inherited = std::env::var("GIT_CONFIG_PARAMETERS").ok();
        if self.config_parameters.is_empty() {
            return inherited;
        }
        let mut parts: Vec<String> = inherited.into_iter().filter(|s| !s.is_empty()).collect();
        for (k, v) in &self.config_parameters {
            let mut p = sq_quote(k);
            p.push('=');
            if let Some(v) = v {
                p.push_str(&sq_quote(v));
            }
            parts.push(p);
        }
        Some(parts.join(" "))
    }

    /// Environment variables to set for child processes so that they see
    /// the global options, as git exports them.
    pub fn child_env(&self) -> Vec<(OsString, OsString)> {
        let mut env: Vec<(OsString, OsString)> = Vec::new();
        let mut set = |k: &str, v: &str| env.push((k.into(), v.into()));
        if let Some(c) = self.config_parameters_env() {
            set("GIT_CONFIG_PARAMETERS", &c);
        }
        if let Some(d) = &self.git_dir {
            set("GIT_DIR", d);
        }
        if let Some(w) = &self.work_tree {
            set("GIT_WORK_TREE", w);
        }
        if let Some(n) = &self.namespace {
            set("GIT_NAMESPACE", n);
        }
        if self.bare {
            set("GIT_IMPLICIT_WORK_TREE", "0");
        }
        if self.no_replace_objects {
            set("GIT_NO_REPLACE_OBJECTS", "1");
        }
        if self.no_lazy_fetch {
            set("GIT_NO_LAZY_FETCH", "1");
        }
        if self.no_optional_locks {
            set("GIT_OPTIONAL_LOCKS", "0");
        }
        if self.no_advice {
            set("GIT_ADVICE", "0");
        }
        if self.literal_pathspecs {
            set("GIT_LITERAL_PATHSPECS", "1");
        }
        if self.glob_pathspecs {
            set("GIT_GLOB_PATHSPECS", "1");
        }
        if self.noglob_pathspecs {
            set("GIT_NOGLOB_PATHSPECS", "1");
        }
        if self.icase_pathspecs {
            set("GIT_ICASE_PATHSPECS", "1");
        }
        if let Some(a) = &self.attr_source {
            set("GIT_ATTR_SOURCE", a);
        }
        if crate::pager::started() {
            set("GIT_PAGER_IN_USE", "true");
        } else if self.paginate == Some(false)
            || (self.paginate.is_none()
                && !self.command.is_empty()
                && crate::pager::disabled(self, &crate::pager::config(self)))
        {
            set("GIT_PAGER", "cat");
        }
        env.push((
            "GIT_EXEC_PATH".into(),
            self.exec_path.clone().into_os_string(),
        ));
        let mut dirs = vec![self.exec_path.clone()];
        if let Some(old) = std::env::var_os("PATH") {
            dirs.extend(std::env::split_paths(&old));
        }
        if let Ok(path) = std::env::join_paths(dirs) {
            env.push(("PATH".into(), path));
        }
        env
    }
}

/// Default exec path: `$GIT_EXEC_PATH`, else the directory of argv[0] (not
/// resolving symlinks, so an install directory with a `git` symlink is its
/// own exec path), else the directory of the running executable.
pub fn default_exec_path(argv0: &str) -> PathBuf {
    if let Some(p) = std::env::var_os("GIT_EXEC_PATH") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    if argv0.contains(std::path::MAIN_SEPARATOR) || argv0.contains('/') {
        if let Some(parent) = Path::new(argv0).parent() {
            let parent = if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            };
            if let Ok(abs) = std::path::absolute(parent) {
                return normalize(&abs);
            }
        }
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Remove `.` components (`std::path::absolute` keeps `./`).
fn normalize(p: &Path) -> PathBuf {
    p.components()
        .filter(|c| !matches!(c, std::path::Component::CurDir))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(sq_quote("a.b"), "'a.b'");
        assert_eq!(sq_quote("it's"), "'it'\\''s'");
        assert_eq!(sq_quote("!env"), "''\\!'env'");
    }

    #[test]
    fn config_env_format() {
        let ctx = Context {
            config_parameters: vec![
                ("alias.e".into(), Some("!env".into())),
                ("d.e".into(), None),
                ("x.y".into(), Some("it's".into())),
            ],
            ..Default::default()
        };
        let v = ctx.config_parameters_env().unwrap();
        assert!(
            v.ends_with("'alias.e'=''\\!'env' 'd.e'= 'x.y'='it'\\''s'"),
            "{v}"
        );
    }
}
