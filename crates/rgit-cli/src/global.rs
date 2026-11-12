// SPDX-License-Identifier: MIT
//! Global options: `git [<global options>] <command> [<args>]`
//! (Documentation/git.adoc, OPTIONS). These are not parse-options options:
//! no abbreviation, and most take their value as the next word.

use std::path::Path;

use crate::context::Context;
use crate::messages as msg;
use crate::{Die, commands};

/// What to do after the global options.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Run a command; `argv[0]` is the command name.
    Run(Vec<String>),
    /// No command given.
    NoCommand,
    /// Done (e.g. `--exec-path` printed it).
    Exit(i32),
}

fn usage_error(text: &str) -> Die {
    Die::Output(rgit_options::Exit {
        code: rgit_options::EXIT_USAGE,
        stdout: String::new(),
        stderr: format!("{text}\n{}", msg::GIT_USAGE),
    })
}

/// `strerror`-style text of an I/O error (without Rust's "(os error N)").
pub fn strerror(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error ") {
        Some(i) => s[..i].to_string(),
        None => s,
    }
}

/// Take the value of `--name=<v>` or `--name <v>`.
fn value_of(args: &[String], i: &mut usize, name: &str) -> Option<Option<String>> {
    let a = &args[*i];
    if a == name {
        *i += 1;
        return Some(args.get(*i).cloned());
    }
    a.strip_prefix(name)
        .and_then(|r| r.strip_prefix('='))
        .map(|v| Some(v.to_string()))
}

/// Parse the global options in `args` (everything after argv[0]).
pub fn parse(ctx: &mut Context, args: &[String]) -> Result<Action, Die> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if !a.starts_with('-') {
            break;
        }
        match a {
            "-h" | "--help" => {
                let mut argv = vec!["help".to_string()];
                argv.extend(args[i + 1..].iter().cloned());
                return Ok(Action::Run(argv));
            }
            "-v" | "--version" => {
                let mut argv = vec!["version".to_string()];
                argv.extend(args[i + 1..].iter().cloned());
                return Ok(Action::Run(argv));
            }
            "-C" => {
                i += 1;
                let Some(dir) = args.get(i) else {
                    return Err(usage_error(&msg::no_directory_given("-C")));
                };
                if !dir.is_empty() {
                    std::env::set_current_dir(dir)
                        .map_err(|e| Die::Fatal(msg::cannot_change_to(dir, &strerror(&e))))?;
                }
            }
            "-c" => {
                i += 1;
                let Some(spec) = args.get(i) else {
                    return Err(usage_error(msg::C_EXPECTS_CONFIG));
                };
                let entry = match spec.split_once('=') {
                    Some((k, v)) => (k.to_string(), Some(v.to_string())),
                    None => (spec.clone(), None),
                };
                ctx.config_parameters.push(entry);
            }
            "-p" | "--paginate" => ctx.paginate = Some(true),
            "-P" | "--no-pager" => ctx.paginate = Some(false),
            "--bare" => {
                ctx.bare = true;
                if ctx.git_dir.is_none() && std::env::var_os("GIT_DIR").is_none() {
                    let cwd = std::env::current_dir()
                        .map_err(|e| Die::Fatal(crate::messages::cannot_get_cwd(&strerror(&e))))?;
                    ctx.git_dir = Some(cwd.to_string_lossy().into_owned());
                }
            }
            "--no-replace-objects" => ctx.no_replace_objects = true,
            "--no-lazy-fetch" => ctx.no_lazy_fetch = true,
            "--no-optional-locks" => ctx.no_optional_locks = true,
            "--no-advice" => ctx.no_advice = true,
            "--literal-pathspecs" => ctx.literal_pathspecs = true,
            "--glob-pathspecs" => ctx.glob_pathspecs = true,
            "--noglob-pathspecs" => ctx.noglob_pathspecs = true,
            "--icase-pathspecs" => ctx.icase_pathspecs = true,
            "--exec-path" => {
                println!("{}", ctx.exec_path.display());
                return Ok(Action::Exit(0));
            }
            "--html-path" | "--man-path" | "--info-path" => {
                println!("{}", doc_path(&ctx.exec_path, a).display());
                return Ok(Action::Exit(0));
            }
            _ => {
                if let Some(p) = a.strip_prefix("--exec-path=") {
                    ctx.exec_path = p.into();
                    ctx.exec_path_overridden = true;
                } else if let Some(groups) = a.strip_prefix("--list-cmds=") {
                    list_cmds(groups)?;
                    return Ok(Action::Exit(0));
                } else if let Some(v) = value_of(args, &mut i, "--git-dir") {
                    let v = v.ok_or_else(|| usage_error(&msg::no_directory_given("--git-dir")))?;
                    ctx.git_dir = Some(v);
                } else if let Some(v) = value_of(args, &mut i, "--work-tree") {
                    let v =
                        v.ok_or_else(|| usage_error(&msg::no_directory_given("--work-tree")))?;
                    ctx.work_tree = Some(v);
                } else if let Some(v) = value_of(args, &mut i, "--namespace") {
                    ctx.namespace = Some(v.ok_or_else(|| usage_error(msg::NO_NAMESPACE))?);
                } else if let Some(v) = value_of(args, &mut i, "--attr-source") {
                    ctx.attr_source = Some(v.ok_or_else(|| usage_error(msg::NO_ATTR_SOURCE))?);
                } else if let Some(v) = value_of(args, &mut i, "--config-env") {
                    let spec = v.ok_or_else(|| usage_error(msg::NO_CONFIG_KEY))?;
                    let Some((key, var)) = spec.rsplit_once('=') else {
                        return Err(Die::Fatal(msg::invalid_config_format(&spec)));
                    };
                    let value = std::env::var(var)
                        .map_err(|_| Die::Fatal(msg::missing_env_for_config(var, key)))?;
                    ctx.config_parameters.push((key.to_string(), Some(value)));
                } else {
                    return Err(usage_error(&msg::unknown_option(a)));
                }
            }
        }
        i += 1;
    }
    if i >= args.len() {
        return Ok(Action::NoCommand);
    }
    Ok(Action::Run(args[i..].to_vec()))
}

/// rgit installs no HTML/man/info pages; report where they would live
/// relative to the installation prefix.
fn doc_path(exec_path: &Path, which: &str) -> std::path::PathBuf {
    let prefix = exec_path.parent().unwrap_or(exec_path);
    match which {
        "--html-path" => prefix.join("share/doc/rgit"),
        "--man-path" => prefix.join("share/man"),
        _ => prefix.join("share/info"),
    }
}

/// `--list-cmds=<group>[,<group>...]`.
fn list_cmds(groups: &str) -> Result<(), Die> {
    let mut names: Vec<&str> = Vec::new();
    for g in groups.split(',').filter(|g| !g.is_empty()) {
        match g {
            "builtins" | "parseopt" | "main" => {
                names.extend(commands::COMMANDS.iter().map(|c| c.name));
            }
            // We have no helpers, deprecated commands, external commands,
            // aliases or category lists yet.
            "nohelpers" | "deprecated" | "others" | "alias" | "config" => {}
            _ if g.starts_with("list-") => {}
            _ => return Err(Die::Fatal(msg::unsupported_listing(g))),
        }
    }
    names.sort_unstable();
    names.dedup();
    for n in names {
        println!("{n}");
    }
    Ok(())
}
