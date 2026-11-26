// SPDX-License-Identifier: MIT
//! `git submodule add`.

use std::path::Path;
use std::process::Stdio;

use crate::messages::submodule_cmd as msg;
use crate::prelude::*;

use super::common::{self, Super, get_string};
use super::update::{self, CloneArgs};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let branch = RefCell::new(None::<String>);
    let force = Cell::new(false);
    let quiet = Cell::new(false);
    let progress = Cell::new(-1);
    let reference = RefCell::new(None::<String>);
    let ref_format = RefCell::new(None::<String>);
    let dissociate = Cell::new(false);
    let name = RefCell::new(None::<String>);
    let depth = RefCell::new(None::<String>);
    let mut parser = Parser::new(
        msg::USAGE_ADD,
        vec![
            Opt::string('b', "branch", &branch, "branch", msg::HELP_ADD_BRANCH),
            Opt::flag('f', "force", &force, msg::HELP_ADD_FORCE),
            Opt::flag('q', "quiet", &quiet, msg::HELP_ADD_QUIET),
            Opt::bool(None, "progress", &progress, msg::HELP_PROGRESS),
            Opt::string(None, "reference", &reference, "repository", msg::HELP_REFERENCE),
            Opt::string(None, "ref-format", &ref_format, "format", msg::HELP_REF_FORMAT),
            Opt::flag(None, "dissociate", &dissociate, msg::HELP_ADD_DISSOCIATE),
            Opt::string(None, "name", &name, "name", msg::HELP_NAME),
            Opt::string(None, "depth", &depth, "n", msg::HELP_ADD_DEPTH),
        ],
    );
    let args = parser.parse(argv)?.args;
    if args.is_empty() || args.len() > 2 {
        return Err(super::usage_error());
    }
    drop(parser);
    let mut sup = Super::open(ctx, None)?;
    let repo_arg = args[0].clone();
    let relative = common::is_relative_url(&repo_arg);
    if relative && !sup.prefix.is_empty() {
        return Err(Die::Fatal(msg::RELATIVE_FROM_TOPLEVEL.into()));
    }
    let real_repo = if relative {
        sup.resolve_url(&repo_arg, false)?
    } else {
        repo_arg.clone()
    };
    let path_arg = match args.get(1) {
        Some(p) => p.clone(),
        None => guess_path(&repo_arg),
    };
    let path = normalize(&format!("{}{path_arg}", sup.prefix));
    let force = force.get();
    let quiet = quiet.get();

    // The path must not be tracked as anything but a submodule.
    let (index, _) = sup.repo.read_index()?;
    let entries = index.entries_for_path(path.as_bytes());
    let mut dir_prefix = path.clone().into_bytes();
    dir_prefix.push(b'/');
    let below = index
        .entries()
        .iter()
        .any(|e| e.path().starts_with(&dir_prefix));
    if below || entries.iter().any(|e| !e.mode.is_gitlink()) {
        return Err(Die::Fatal(if force {
            msg::in_index_not_submodule(&path)
        } else {
            msg::in_index(&path)
        }));
    }
    if !entries.is_empty() && !force {
        return Err(Die::Fatal(msg::in_index(&path)));
    }
    drop(index);

    // An existing repository at the path is added as it is.
    let dir = sup.work_tree.join(&path);
    let display = sup.display_path(&path);
    let existing = match std::fs::symlink_metadata(&dir) {
        Ok(meta) if meta.is_dir() && dir.join(".git").exists() => {
            let Some(head) = common::sub_head(&dir, sup.hash()) else {
                return Err(Die::Fatal(msg::no_commit_checked_out(&path)));
            };
            if head.kind() != sup.hash() {
                return Err(Die::Fatal(msg::DIFFERENT_HASH.into()));
            }
            true
        }
        Ok(meta) if meta.is_dir() => {
            return Err(Die::Fatal(msg::not_valid_repo(&path)));
        }
        _ => false,
    };

    // The index must be writable (git's dry-run `add` takes its lock).
    match rgit_fs::LockFile::acquire(sup.repo.index_path(), &sup.repo.index_lock_options()) {
        Ok(lock) => lock.rollback(),
        Err(e) => return Err(Die::Fatal(e.to_string())),
    }

    // Ignored paths: ask `add`.
    let mut dry = vec![
        "add",
        "--dry-run",
        "--ignore-missing",
        "--no-warn-embedded-repo",
    ];
    if force {
        dry.push("--force");
    }
    dry.push("--");
    dry.push(&path);
    let out = sup
        .git(&dry)?
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Die::Fatal(e.to_string()))?;
    if !out.status.success() {
        use std::io::Write as _;
        let _ = std::io::stderr().write_all(&out.stderr);
        return Err(Die::Exit(out.status.code().unwrap_or(1)));
    }

    let gm = sup.gitmodules();
    let mut sm_name = name.borrow().clone().unwrap_or_else(|| path.clone());
    if !common::check_name(&sm_name) {
        return Err(Die::Fatal(msg::invalid_name(&sm_name)));
    }
    if let Some(other) = gm.path_for_name(&sm_name).filter(|p| *p != path) {
        if !force {
            return Err(Die::Fatal(msg::name_used(&sm_name, &other)));
        }
        let base = sm_name.clone();
        let mut n = 1;
        while gm.path_for_name(&sm_name).is_some_and(|p| p != path) {
            sm_name = format!("{base}{n}");
            n += 1;
        }
    }

    if existing {
        if !quiet {
            println!("{}", msg::adding_existing(&display));
        }
    } else {
        clone_new(
            &sup,
            &path,
            &sm_name,
            &real_repo,
            &CloneOpts {
                force,
                quiet,
                progress: (progress.get() >= 0).then(|| progress.get() > 0),
                depth: depth.borrow().clone(),
                branch: branch.borrow().clone(),
                reference: reference.borrow().iter().cloned().collect(),
                dissociate: dissociate.get(),
                ref_format: ref_format.borrow().clone(),
            },
        )?;
    }

    // Register it: the URL, and active unless `submodule.active` (or an
    // explicit setting) already makes it so.
    let explicit = common::get_bool(&sup.repo.config, &format!("submodule.{sm_name}.active"));
    let by_spec = sup.repo.config.get(b"submodule.active").is_some()
        && common::is_active(&sup.repo.config, &sm_name, &path);
    sup.set_config(&format!("submodule.{sm_name}.url"), Some(&real_repo))?;
    if explicit != Some(true) && !by_spec {
        sup.set_config(&format!("submodule.{sm_name}.active"), Some("true"))?;
    }
    let mut add = vec!["add", "--no-warn-embedded-repo"];
    if force {
        add.push("--force");
    }
    add.push("--");
    add.push(&path);
    if common::run_status(&mut sup.git(&add)?)? != 0 {
        return Err(Die::Fatal(msg::failed_to_add(&path)));
    }
    let register = || -> Result<(), Die> {
        sup.set_gitmodules(&format!("submodule.{sm_name}.path"), Some(&path))?;
        sup.set_gitmodules(&format!("submodule.{sm_name}.url"), Some(&repo_arg))?;
        if let Some(b) = branch.borrow().as_deref() {
            sup.set_gitmodules(&format!("submodule.{sm_name}.branch"), Some(b))?;
        }
        Ok(())
    };
    if register().is_err()
        || common::run_status(&mut sup.git(&["add", "--force", "--", ".gitmodules"])?)? != 0
    {
        return Err(Die::Fatal(msg::failed_to_register(&path)));
    }
    Ok(0)
}

struct CloneOpts {
    force: bool,
    quiet: bool,
    progress: Option<bool>,
    depth: Option<String>,
    branch: Option<String>,
    reference: Vec<String>,
    dissociate: bool,
    ref_format: Option<String>,
}

/// Clone the new submodule (or reuse its git directory from an earlier
/// clone) and check out its branch.
fn clone_new(sup: &Super, path: &str, name: &str, url: &str, o: &CloneOpts) -> Result<(), Die> {
    let git_dir = sup.module_git_dir(name);
    if git_dir.join("HEAD").exists() {
        if !o.force {
            let config = common::sub_config(&git_dir);
            let mut text = msg::git_dir_found_locally(name);
            for remote in config.subsections("remote") {
                let r = String::from_utf8_lossy(&remote).into_owned();
                let u = get_string(&config, &format!("remote.{r}.url")).unwrap_or_default();
                text.push_str(&format!("\n  {r}\t{u}"));
            }
            text.push('\n');
            text.push_str(&msg::reuse_hint(url));
            return Err(Die::Fatal(text));
        }
        if !o.quiet {
            eprintln!("{}", msg::reactivating(name));
        }
    }
    update::clone_into(
        sup,
        CloneArgs {
            path,
            name,
            url,
            depth: o.depth.as_deref(),
            quiet: o.quiet,
            progress: o.progress,
            single_branch: None,
            branch: None,
            require_init: false,
            reference: &o.reference,
            dissociate: o.dissociate,
            ref_format: o.ref_format.as_deref(),
        },
    )?;
    let dir = sup.work_tree.join(path);
    let start;
    let mut a = vec!["checkout", "-f", "-q"];
    if let Some(b) = &o.branch {
        let remote = common::sub_default_remote(&common::sub_git_dir(&dir).unwrap_or_default());
        start = format!("{remote}/{b}");
        a.extend(["-B", b.as_str(), start.as_str()]);
    }
    if common::run_status(&mut sup.sub_git(Path::new(path), &a)?)? != 0 {
        return Err(Die::Fatal(msg::unable_to_checkout_submodule(path)));
    }
    Ok(())
}

/// The directory name `git clone` would use for `url`.
fn guess_path(url: &str) -> String {
    let mut s = url.trim_end_matches('/');
    s = s.strip_suffix("/.git").unwrap_or(s);
    s = s.trim_end_matches('/');
    let start = s.rfind(['/', ':']).map_or(0, |i| i + 1);
    let mut base = &s[start..];
    base = base.strip_suffix(".git").unwrap_or(base);
    base.to_owned()
}

/// Normalise a path relative to the top: drop `.` and empty components,
/// resolve `..`, no trailing slash.
fn normalize(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for c in p.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(guess_path("/a/b/repo.git"), "repo");
        assert_eq!(guess_path("host.xz:foo/.git"), "foo");
        assert_eq!(guess_path("../sub/"), "sub");
        assert_eq!(normalize("dot/dotslashsubmod/./../..////realsubmod2/a/b/c/d/../../../../frotz//.."), "realsubmod2");
        assert_eq!(normalize("././dotsubmod/./frotz/./"), "dotsubmod/frotz");
    }
}
