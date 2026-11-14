// SPDX-License-Identifier: MIT
//! Aliases (Documentation/config/alias.adoc) and the configuration they
//! are read from before a command runs.
//!
//! `alias.<name>` (ASCII alphanumerics and `-`, matched without regard to
//! case; `alias.<a>.<b>` is the alias `<a>.<b>`) and `alias.<name>.command`
//! (any name, matched exactly). The value is split like a shell command
//! line; a leading `!` makes it a shell command run from the top of the
//! working tree with `GIT_PREFIX` set.

use bstr::ByteSlice;
use rgit_config::sources::{self, RepoConfig, ScopeFilter};
use rgit_config::{ConfigSet, LoadOptions, StaticRepoFacts, SystemEnv};
use rgit_repo::{Setup, SetupOptions, StandardProtectedConfig};

use crate::messages::help as msg;
use crate::{Context, Die, repo};

/// The configuration as seen before a command runs (git's "early config"):
/// the repository is discovered from the current directory without moving
/// there, problems finding it only mean there is no repository config.
pub struct EarlyConfig {
    pub config: ConfigSet,
    pub setup: Option<Setup>,
}

impl EarlyConfig {
    pub fn load(ctx: &Context) -> Result<EarlyConfig, Die> {
        repo::check_command_line(ctx)?;
        let env = SystemEnv::default();
        let setup = SetupOptions::from_process().ok().and_then(|mut opts| {
            if let Some(d) = &ctx.git_dir {
                opts.git_dir = Some(d.into());
            }
            if let Some(w) = &ctx.work_tree {
                opts.work_tree = Some(w.into());
            }
            if ctx.bare {
                opts.implicit_work_tree = false;
            }
            let mut protected = StandardProtectedConfig::new(&env, repo::command_line(ctx));
            rgit_repo::discover(&opts, &mut protected).ok()?.ok()
        });
        let mut config = ConfigSet::new();
        let result = match &setup {
            Some(setup) => {
                let git_dir = setup.absolute_git_dir();
                let branch = std::fs::read(git_dir.join("HEAD")).ok().and_then(|h| {
                    h.strip_prefix(b"ref: refs/heads/")
                        .map(|r| r.trim_ascii_end().into())
                });
                let facts = StaticRepoFacts {
                    git_dir: Some(git_dir.clone()),
                    worktree: setup.work_tree.as_ref().map(|w| setup.cwd.join(w)),
                    branch,
                };
                // Paths in messages as git shows them: relative while we
                // are still in the directory they are relative to.
                let here = std::env::current_dir().is_ok_and(|d| d == setup.cwd);
                let repo_cfg = RepoConfig {
                    common_dir: if here {
                        setup.common_dir.clone()
                    } else {
                        setup.absolute_common_dir()
                    },
                    git_dir,
                    worktree_config: None,
                };
                sources::load_standard(
                    LoadOptions {
                        env: &env,
                        repo: Some(&facts),
                        includes: true,
                    },
                    Some(&repo_cfg),
                    &repo::command_line(ctx),
                    ScopeFilter::default(),
                    &mut config,
                )
            }
            None => sources::load_standard(
                LoadOptions {
                    env: &env,
                    repo: None,
                    includes: true,
                },
                None,
                &repo::command_line(ctx),
                ScopeFilter::default(),
                &mut config,
            ),
        };
        if let Err(e) = result {
            for (level, m) in e.report() {
                eprintln!("{} {m}", level.prefix());
            }
            return Err(Die::Exit(rgit_messages::EXIT_FATAL));
        }
        Ok(EarlyConfig { config, setup })
    }
}

/// The alias name an `alias.*` variable defines, if any.
fn alias_name(name: &bstr::BStr) -> Option<(String, bool)> {
    let (section, sub, key) = rgit_config::key::split_canonical(name);
    if section != "alias" {
        return None;
    }
    let key = key.to_str_lossy().into_owned();
    match sub {
        // `alias..x` is `alias.x`.
        None => Some((key, false)),
        Some(s) if s.is_empty() => Some((key, false)),
        Some(s) if key == "command" => Some((s.to_str_lossy().into_owned(), true)),
        Some(s) => Some((format!("{}.{key}", s.to_str_lossy()), false)),
    }
}

/// Does the `alias.*` variable `entry_name` define the alias `cmd`?
fn defines(entry_name: &bstr::BStr, cmd: &str) -> bool {
    match alias_name(entry_name) {
        Some((name, true)) => name == cmd,
        Some((name, false)) => name.eq_ignore_ascii_case(cmd),
        None => false,
    }
}

/// The value of the alias `cmd`. A variable without a value is an error.
pub fn lookup(config: &ConfigSet, cmd: &str) -> Result<Option<String>, Die> {
    let Some(entry) = config
        .entries()
        .iter()
        .rev()
        .find(|e| defines(e.name.as_bstr(), cmd))
    else {
        return Ok(None);
    };
    match entry.string() {
        Ok(v) => Ok(Some(v.to_str_lossy().into_owned())),
        Err(err) => {
            eprintln!("error: {err}");
            Err(Die::Fatal(crate::messages::config::bad_config_line(
                entry.line,
                &entry.source.origin.describe(),
            )))
        }
    }
}

/// All alias names in configuration order, each once, with their values.
pub fn list(config: &ConfigSet) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for e in config.entries() {
        let Some((name, _)) = alias_name(e.name.as_bstr()) else {
            continue;
        };
        let value = e
            .value
            .as_ref()
            .map(|v| v.to_str_lossy().into_owned())
            .unwrap_or_default();
        match out.iter_mut().find(|(n, _)| *n == name) {
            Some(slot) => slot.1 = value,
            None => out.push((name, value)),
        }
    }
    out
}

/// Split an alias value into words: whitespace separates words, `'...'`
/// quotes literally, `"..."` quotes with backslash escapes, a backslash
/// outside quotes escapes the next character.
pub fn split(value: &str) -> Result<Vec<String>, &'static str> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut chars = value.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(ch) = chars.next() {
        match quote {
            None if ch.is_ascii_whitespace() => {
                words.push(std::mem::take(&mut cur));
                while chars.peek().is_some_and(|c| c.is_ascii_whitespace()) {
                    chars.next();
                }
            }
            None if ch == '\'' || ch == '"' => quote = Some(ch),
            Some(q) if ch == q => quote = None,
            Some('\'') => cur.push(ch),
            _ if ch == '\\' => match chars.next() {
                Some(next) => cur.push(next),
                None => return Err(msg::ENDS_WITH_BACKSLASH),
            },
            _ => cur.push(ch),
        }
    }
    if quote.is_some() {
        return Err(msg::UNCLOSED_QUOTE);
    }
    words.push(cur);
    Ok(words)
}

/// Global options at the start of an alias (`ps = -p status`). Options
/// that would change the environment of the command (`-C`, `--git-dir`,
/// ...) are refused. Returns the remaining words, or the exit code when an
/// option such as `--exec-path` finished the command.
pub fn global_options(
    ctx: &mut Context,
    alias: &str,
    words: Vec<String>,
) -> Result<Result<Vec<String>, i32>, Die> {
    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_str();
        if !w.starts_with('-') || matches!(w, "-h" | "--help" | "-v" | "--version") {
            break;
        }
        let changes_env = matches!(
            w,
            "-C" | "--bare"
                | "--literal-pathspecs"
                | "--glob-pathspecs"
                | "--noglob-pathspecs"
                | "--icase-pathspecs"
                | "--no-replace-objects"
                | "--no-lazy-fetch"
                | "--no-optional-locks"
                | "--no-advice"
                | "-P"
                | "--no-pager"
        ) || [
            "--git-dir",
            "--work-tree",
            "--namespace",
            "--attr-source",
            "--super-prefix",
        ]
        .iter()
        .any(|o| w == *o || w.strip_prefix(o).is_some_and(|r| r.starts_with('=')));
        if changes_env {
            return Err(Die::Fatal(msg::alias_changes_environment(alias)));
        }
        if w == "-c" || w == "--config-env" {
            i += 1;
        }
        i += 1;
    }
    let (opts, rest) = words.split_at(i.min(words.len()));
    if !opts.is_empty() {
        match crate::global::parse(ctx, opts)? {
            crate::global::Action::Exit(code) => return Ok(Err(code)),
            crate::global::Action::Run(_) | crate::global::Action::NoCommand => {}
        }
    }
    if rest.is_empty() {
        return Err(Die::Fatal(msg::empty_alias(alias)));
    }
    Ok(Ok(rest.to_vec()))
}

/// Characters that make git run a command through the shell.
const SHELL_SPECIALS: &[char] = &[
    '|', '&', ';', '<', '>', '(', ')', '$', '`', '\\', '"', '\'', ' ', '\t', '\n', '*', '?', '[',
    '#', '~', '=', '%',
];

/// Run the shell alias `alias` (`!<cmd>`) with `args`: from the top of the
/// working tree, with `GIT_PREFIX` set to the original directory.
pub fn run_shell(
    ctx: &Context,
    early: &EarlyConfig,
    alias: &str,
    cmd: &str,
    args: &[String],
) -> crate::CmdResult {
    use std::ffi::OsString;
    use std::process::Command;

    let mut shown: Vec<OsString> = vec![cmd.into()];
    shown.extend(args.iter().map(Into::into));
    rgit_trace::trace_run_command(&[], &shown);
    let (mut command, started): (Command, Vec<OsString>) = if cmd.contains(SHELL_SPECIALS) {
        let script = if args.is_empty() {
            cmd.to_string()
        } else {
            format!("{cmd} \"$@\"")
        };
        let mut c = Command::new("/bin/sh");
        c.arg("-c").arg(&script).arg(cmd).args(args);
        let mut v: Vec<OsString> = vec!["/bin/sh".into(), "-c".into(), script.into(), cmd.into()];
        v.extend(args.iter().map(Into::into));
        (c, v)
    } else {
        let mut c = Command::new(cmd);
        c.args(args);
        let resolved = crate::cmdlist::path_dirs()
            .into_iter()
            .map(|d| d.join(cmd))
            .find(|p| crate::external::is_executable(p))
            .map(|p| p.into_os_string())
            .unwrap_or_else(|| cmd.into());
        let mut v: Vec<OsString> = vec![resolved];
        v.extend(args.iter().map(Into::into));
        (c, v)
    };
    rgit_trace::trace_start_command(&started);
    for (k, v) in ctx.child_env() {
        command.env(k, v);
    }
    let mut prefix = OsString::new();
    if let Some(setup) = &early.setup {
        command.current_dir(&setup.cwd);
        if let Some(p) = &setup.prefix {
            prefix = p.clone();
        }
        if setup.work_tree.is_none() && ctx.git_dir.is_none() {
            command.env("GIT_DIR", &setup.git_dir);
            command.env("GIT_IMPLICIT_WORK_TREE", "0");
        }
    }
    command.env("GIT_PREFIX", prefix);
    let status = match command.status() {
        Ok(s) => s,
        Err(e) => {
            let err = crate::global::strerror(&e);
            eprintln!("error: {}", msg::cannot_run(cmd, &err));
            return Err(Die::Fatal(msg::while_expanding_alias(alias, cmd, &err)));
        }
    };
    if let Some(code) = status.code() {
        return Ok(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            if sig != libc::SIGINT && sig != libc::SIGPIPE {
                let m = crate::messages::died_of_signal(cmd, sig);
                return Err(Die::Error(m, 128 + sig));
            }
            return Ok(128 + sig);
        }
    }
    Ok(128)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitting() {
        assert_eq!(split("log -1").unwrap(), ["log", "-1"]);
        assert_eq!(split("a  b").unwrap(), ["a", "b"]);
        assert_eq!(split("a  ").unwrap(), ["a", ""]);
        assert_eq!(split(" a").unwrap(), ["", "a"]);
        assert_eq!(
            split(r#"a "b\"c" 'd\e' f\ g"#).unwrap(),
            ["a", "b\"c", "d\\e", "f g"]
        );
        assert_eq!(split("a''b \"x\"y").unwrap(), ["ab", "xy"]);
        assert_eq!(split("a\\").unwrap_err(), msg::ENDS_WITH_BACKSLASH);
        assert_eq!(split("a 'b").unwrap_err(), msg::UNCLOSED_QUOTE);
    }

    #[test]
    fn names() {
        let n = |s: &str| alias_name(bstr::BStr::new(s));
        assert_eq!(n("alias.co"), Some(("co".into(), false)));
        assert_eq!(n("alias.Sub.command"), Some(("Sub".into(), true)));
        assert_eq!(
            n("alias.simple.dotted"),
            Some(("simple.dotted".into(), false))
        );
        assert_eq!(n("alias..x"), Some(("x".into(), false)));
        assert_eq!(n("core.x"), None);
        assert!(defines(bstr::BStr::new("alias.legacycase"), "LegacyCase"));
        assert!(!defines(bstr::BStr::new("alias.Sub.command"), "sub"));
    }
}
