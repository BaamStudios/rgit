// SPDX-License-Identifier: MIT
//! `git submodule` foreach, deinit, sync, set-url, set-branch and
//! absorbgitdirs.

use std::path::Path;

use crate::messages::submodule_cmd as msg;
use crate::prelude::*;

use super::common::{self, Super, get_string};

pub fn foreach(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let quiet = Cell::new(false);
    let recursive = Cell::new(false);
    let super_prefix = RefCell::new(None::<String>);
    let mut parser = Parser::new(
        msg::USAGE_FOREACH,
        vec![
            Opt::flag('q', "quiet", &quiet, msg::HELP_FOREACH_QUIET),
            Opt::flag(None, "recursive", &recursive, msg::HELP_RECURSIVE),
            Opt::string(None, "super-prefix", &super_prefix, "path", "").hidden(),
        ],
    )
    .flags(ParseFlags {
        stop_at_non_option: true,
        ..Default::default()
    });
    let args = parser.parse(argv)?.args;
    drop(parser);
    if args.first().is_some_and(|a| a.starts_with('-')) {
        return Err(super::usage_error());
    }
    let mut sup = Super::open(ctx, super_prefix.borrow().as_deref())?;
    let list = sup.module_list(&[])?;
    let gm = sup.gitmodules();
    let toplevel = sup.work_tree.to_string_lossy().into_owned();
    for m in &list {
        let dir = sup.work_tree.join(&m.path);
        if m.unmerged || !common::is_populated(&dir) {
            continue;
        }
        let name = gm.name_for_path(&m.path).unwrap_or_else(|| m.path.clone());
        let display = sup.display_path(&m.path);
        if !quiet.get() {
            println!("{}", msg::entering(&display));
        }
        if !args.is_empty() {
            let mut cmd = if args.len() == 1 {
                // `$path` is a shell variable only, not exported.
                let mut c = std::process::Command::new(rgit_transport::command::SHELL_PATH);
                c.arg("-c").arg(format!("path=\"$sm_path\"; {}", args[0]));
                c
            } else {
                let mut c = std::process::Command::new(rgit_transport::command::SHELL_PATH);
                c.arg("-c").arg(format!("{} \"$@\"", args[0])).args(&args);
                c
            };
            sup.sub_env(&mut cmd);
            cmd.current_dir(&dir)
                .env("GIT_DIR", ".git")
                .env("name", &name)
                .env("sm_path", &m.path)
                .env("displaypath", &display)
                .env("sha1", m.oid.to_hex())
                .env("toplevel", &toplevel);
            if common::run_status(&mut cmd)? != 0 {
                return Err(Die::Fatal(msg::foreach_failed(&display)));
            }
        }
        if recursive.get() {
            let mut a = vec![
                "submodule".to_owned(),
                "foreach".to_owned(),
                "--recursive".to_owned(),
                format!("--super-prefix={display}/"),
            ];
            if quiet.get() {
                a.push("--quiet".into());
            }
            a.push("--".into());
            a.extend(args.iter().cloned());
            let refs: Vec<&str> = a.iter().map(String::as_str).collect();
            let mut cmd = sup.sub_git(Path::new(&m.path), &refs)?;
            if common::run_status(&mut cmd)? != 0 {
                return Err(Die::Fatal(msg::foreach_recursion_failed(&display)));
            }
        }
    }
    Ok(0)
}

pub fn deinit(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let quiet = Cell::new(false);
    let force = Cell::new(false);
    let all = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE_DEINIT,
        vec![
            Opt::flag('q', "quiet", &quiet, msg::HELP_STATUS_QUIET),
            Opt::flag('f', "force", &force, msg::HELP_DEINIT_FORCE),
            Opt::flag(None, "all", &all, msg::HELP_DEINIT_ALL),
        ],
    );
    let args = parser.parse(argv)?.args;
    if all.get() && !args.is_empty() {
        return Err(parser.error_with_usage(msg::ALL_WITH_PATHSPEC).into());
    }
    drop(parser);
    if !all.get() && args.is_empty() {
        return Err(Die::Fatal(msg::USE_ALL.into()));
    }
    let mut sup = Super::open(ctx, None)?;
    let list = sup.module_list(&args)?;
    let gm = sup.gitmodules();
    for m in &list {
        let name = sup.name_of(&gm, &m.path)?;
        let display = sup.display_path(&m.path);
        let dir = sup.work_tree.join(&m.path);
        if dir.join(".git").is_dir() {
            // An embedded git directory is moved away first.
            absorb_one(&sup, &name, &m.path, &display, true)?;
        }
        if common::is_populated(&dir) {
            // git's check is the one of `git rm -n <path>`: a changed
            // .gitmodules, another commit checked out, local changes.
            if !force.get() {
                if !sup.gitmodules_staged_ok()? {
                    eprintln!("fatal: {}", msg::STAGE_GITMODULES);
                    return Err(Die::Fatal(msg::local_modifications(&display)));
                }
                if common::sub_head(&dir, sup.hash()) != Some(m.oid) || modified(&sup, &dir)? {
                    eprintln!(
                        "error: {}\n    {display}\n{}",
                        msg::RM_LOCAL_MODIFICATIONS,
                        msg::RM_HINT
                    );
                    return Err(Die::Fatal(msg::local_modifications(&display)));
                }
            }
            if std::fs::remove_dir_all(&dir).is_err() {
                eprintln!("error: {}", msg::could_not_remove_work_tree(&display));
            } else if std::fs::create_dir(&dir).is_err() {
                eprintln!("error: {}", msg::could_not_create_empty_dir(&display));
            } else if !quiet.get() {
                println!("{}", msg::cleared_directory(&display));
            }
        } else if dir.is_dir() {
            let _ = std::fs::remove_dir_all(&dir);
            let _ = std::fs::create_dir(&dir);
            if !quiet.get() {
                println!("{}", msg::cleared_directory(&display));
            }
        } else if std::fs::symlink_metadata(&dir).is_err() {
            // A missing work tree comes back as an empty directory.
            let _ = std::fs::create_dir_all(&dir);
        }
        // The git directory no longer has a work tree.
        let git_dir = sup.module_git_dir(&name);
        if git_dir.join("config").exists() {
            common::set_in_file(&git_dir.join("config"), "core.worktree", None)?;
        } else {
            let cwd = std::env::current_dir().unwrap_or_else(|_| sup.work_tree.clone());
            let shown = common::relative_path(&git_dir.join("config"), &common::absolute(&cwd));
            eprintln!("error: {}", msg::could_not_lock_config(&shown.to_string_lossy()));
            eprintln!("warning: {}", msg::could_not_unset_worktree(&name));
        }
        if let Some(url) = get_string(&sup.repo.config, &format!("submodule.{name}.url")) {
            sup.remove_section(&format!("submodule.{name}"))?;
            if !quiet.get() {
                let shown = gm.get(&name, "url").unwrap_or(url);
                println!("{}", msg::unregistered(&name, &shown, &display));
            }
            if sup.repo.config.get(b"submodule.active").is_some() {
                sup.set_config(&format!("submodule.{name}.active"), Some("false"))?;
            }
            sup.reload_config()?;
        }
    }
    Ok(0)
}

/// Whether the submodule at `dir` has local modifications or untracked
/// files.
fn modified(sup: &Super, dir: &Path) -> Result<bool, Die> {
    let mut cmd = sup.sub_git(
        dir,
&["status", "--porcelain", "-uall"],
    )?;
    Ok(common::capture(&mut cmd).is_none_or(|o| !o.is_empty()))
}

pub fn sync(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let quiet = Cell::new(false);
    let recursive = Cell::new(false);
    let super_prefix = RefCell::new(None::<String>);
    let mut parser = Parser::new(
        msg::USAGE_SYNC,
        vec![
            Opt::flag('q', "quiet", &quiet, msg::HELP_SYNC_QUIET),
            Opt::flag(None, "recursive", &recursive, msg::HELP_RECURSIVE),
            Opt::string(None, "super-prefix", &super_prefix, "path", "").hidden(),
        ],
    );
    let args = parser.parse(argv)?.args;
    drop(parser);
    let mut sup = Super::open(ctx, super_prefix.borrow().as_deref())?;
    let list = sup.module_list(&args)?;
    for m in &list {
        sync_one(&mut sup, m, quiet.get(), recursive.get())?;
    }
    Ok(0)
}

/// Copy the `.gitmodules` URL of one submodule into the configuration of
/// the superproject and into the submodule's remote.
fn sync_one(sup: &mut Super, m: &common::Module, quiet: bool, recursive: bool) -> Result<(), Die> {
    let gm = sup.gitmodules();
    let name = sup.name_of(&gm, &m.path)?;
    let display = sup.display_path(&m.path);
    let (super_url, sub_url) = match gm.get(&name, "url") {
        Some(url) if common::is_relative_url(&url) => {
            let (_, remote_url) = sup.default_remote_url();
            let base = remote_url.unwrap_or_else(|| sup.work_tree.to_string_lossy().into_owned());
            let super_url = common::resolve_relative_url(&url, &base, None)?;
            // A relative remote: the submodule sees it from its own path.
            let sub_url =
                common::resolve_relative_url(&url, &base, Some(&common::up_path(&m.path)))?;
            (Some(super_url), Some(sub_url))
        }
        Some(url) => (Some(url.clone()), Some(url)),
        None => (None, None),
    };
    let key = format!("submodule.{name}.url");
    if get_string(&sup.repo.config, &key).is_some() {
        if !quiet {
            println!("{}", msg::synchronizing(&display));
        }
        if let Some(u) = &super_url {
            sup.set_config(&key, Some(u))?;
            sup.reload_config()?;
        }
    }
    let dir = sup.work_tree.join(&m.path);
    let Some(git_dir) = common::sub_git_dir(&dir).filter(|_| common::is_populated(&dir)) else {
        return Ok(());
    };
    if let Some(u) = &sub_url {
        let remote = common::sub_default_remote(&git_dir);
        common::set_in_file(&git_dir.join("config"), &format!("remote.{remote}.url"), Some(u))?;
    }
    if recursive {
        let mut a = vec![
            "submodule".to_owned(),
            "sync".to_owned(),
            "--recursive".to_owned(),
            format!("--super-prefix={display}/"),
        ];
        if quiet {
            a.push("--quiet".into());
        }
        let refs: Vec<&str> = a.iter().map(String::as_str).collect();
        let mut cmd = sup.sub_git(Path::new(&m.path), &refs)?;
        if common::run_status(&mut cmd)? != 0 {
            return Err(Die::Fatal(msg::failed_recurse(&display)));
        }
    }
    Ok(())
}

pub fn set_url(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let quiet = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE_SET_URL,
        vec![Opt::flag('q', "quiet", &quiet, msg::HELP_SET_URL_QUIET)],
    );
    let args = parser.parse(argv)?.args;
    if args.len() != 2 {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let mut sup = Super::open(ctx, None)?;
    let path = common::prefixed_path(&sup, &args[0])?;
    let gm = sup.gitmodules();
    let name = sup.name_of(&gm, &path)?;
    sup.set_gitmodules(&format!("submodule.{name}.url"), Some(&args[1]))?;
    let list = sup.module_list(std::slice::from_ref(&args[0]))?;
    for m in list.iter().filter(|m| m.path == path) {
        sync_one(&mut sup, m, quiet.get(), false)?;
    }
    Ok(0)
}

pub fn set_branch(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let quiet = Cell::new(false);
    let default = Cell::new(false);
    let branch = RefCell::new(None::<String>);
    let mut parser = Parser::new(
        msg::USAGE_SET_BRANCH,
        vec![
            Opt::flag('q', "quiet", &quiet, "").hidden(),
            Opt::flag('d', "default", &default, msg::HELP_SET_DEFAULT),
            Opt::string('b', "branch", &branch, "branch", msg::HELP_SET_BRANCH),
        ],
    );
    let args = parser.parse(argv)?.args;
    if branch.borrow().is_none() && !default.get() {
        return Err(Die::Fatal(msg::BRANCH_OR_DEFAULT.into()));
    }
    if branch.borrow().is_some() && default.get() {
        return Err(Die::Fatal(msg::BRANCH_AND_DEFAULT.into()));
    }
    if args.len() != 1 {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let mut sup = Super::open(ctx, None)?;
    let path = common::prefixed_path(&sup, &args[0])?;
    let gm = sup.gitmodules();
    let name = sup.name_of(&gm, &path)?;
    let key = format!("submodule.{name}.branch");
    if branch.borrow().is_none() && gm.get(&name, "branch").is_none() {
        // Nothing to remove (git's unset fails).
        return Ok(1);
    }
    sup.set_gitmodules(&key, branch.borrow().as_deref())?;
    Ok(0)
}

pub fn absorbgitdirs(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let quiet = Cell::new(false);
    let super_prefix = RefCell::new(None::<String>);
    let mut parser = Parser::new(
        msg::USAGE_ABSORB,
        vec![
            Opt::flag('q', "quiet", &quiet, "").hidden(),
            Opt::string(None, "super-prefix", &super_prefix, "path", "").hidden(),
        ],
    );
    let args = parser.parse(argv)?.args;
    drop(parser);
    let mut sup = Super::open(ctx, super_prefix.borrow().as_deref())?;
    let list = sup.module_list(&args)?;
    let gm = sup.gitmodules();
    for m in &list {
        let dir = sup.work_tree.join(&m.path);
        if m.unmerged || !common::is_populated(&dir) {
            continue;
        }
        let display = sup.display_path(&m.path);
        let Some(name) = gm.name_for_path(&m.path) else {
            return Err(Die::Fatal(msg::could_not_lookup_name(&display)));
        };
        absorb_one(&sup, &name, &m.path, &display, false)?;
        // Nested submodules move into the submodule's git directory.
        let mut a = vec![
            "submodule".to_owned(),
            "absorbgitdirs".to_owned(),
            format!("--super-prefix={display}/"),
        ];
        if quiet.get() {
            a.push("--quiet".into());
        }
        let refs: Vec<&str> = a.iter().map(String::as_str).collect();
        let mut cmd = sup.sub_git(Path::new(&m.path), &refs)?;
        if common::run_status(&mut cmd)? != 0 {
            return Err(Die::Exit(128));
        }
    }
    Ok(0)
}

/// Move the git directory of the submodule at `path` below
/// `modules/<name>` unless it is there already.
fn absorb_one(sup: &Super, name: &str, path: &str, display: &str, deinit: bool) -> Result<(), Die> {
    let dir = sup.work_tree.join(path);
    let target = sup.module_git_dir(name);
    let Some(current) = common::sub_git_dir(&dir) else {
        return Ok(());
    };
    let current = common::real(&current);
    if target.exists() && common::real(&target) == current {
        return Ok(());
    }
    if deinit {
        eprintln!("warning: {}", msg::contains_git_dir(display));
    }
    if target.exists() {
        return Err(Die::Fatal(msg::could_not_migrate(&current.to_string_lossy())));
    }
    let shown_to = common::real(target.parent().unwrap_or(&target)).join(name);
    eprintln!(
        "{}",
        msg::migrating(
            display,
            &current.to_string_lossy(),
            &shown_to.to_string_lossy()
        )
    );
    if dir.join(".git").is_dir() {
        common::relocate_git_dir(&dir, &target)
    } else {
        common::move_git_dir(&current, &target)
            .map_err(|_| Die::Fatal(msg::could_not_migrate(&current.to_string_lossy())))?;
        common::connect(&dir, &target)
    }
}
