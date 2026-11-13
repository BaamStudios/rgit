// SPDX-License-Identifier: MIT
//! `git init` (Documentation/git-init.adoc): option parsing, the
//! `<directory>` argument and `GIT_DIR`/`GIT_WORK_TREE`; the repository is
//! created by [`rgit_repo::init`].

use std::io::Write;
use std::path::{Path, PathBuf};

use rgit_config::{Environment, SystemEnv};
use rgit_fs::SharedRepository;
use rgit_messages::{Level, prefixed};
use rgit_repo::init::{self as repo_init, InitError, InitOptions, Notice, Template};
use rgit_repo::path;

use crate::messages::init as msg;
use crate::prelude::*;
use crate::repo::command_line;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let template = RefCell::new(None);
    let bare = Cell::new(false);
    let shared: Cell<Option<SharedRepository>> = Cell::new(None);
    let quiet = Cell::new(false);
    let separate = RefCell::new(None);
    let branch = RefCell::new(None);
    let object_format = RefCell::new(None);
    let ref_format = RefCell::new(None);
    let mut parser = Parser::new(
        &[msg::USAGE],
        vec![
            Opt::string(
                None,
                "template",
                &template,
                "template-directory",
                msg::HELP_TEMPLATE,
            ),
            Opt::flag(None, "bare", &bare, msg::HELP_BARE),
            Opt::callback(
                None,
                "shared",
                Some("permissions"),
                msg::HELP_SHARED,
                |v, _| {
                    let s = SharedRepository::parse_named(v.unwrap_or("group"), "arg")
                        .map_err(|e| CallbackError::Fatal(e.to_string()))?;
                    shared.set(Some(s));
                    Ok(())
                },
            )
            .optional(None)
            .noneg(),
            Opt::flag('q', "quiet", &quiet, msg::HELP_QUIET),
            Opt::string(
                None,
                "separate-git-dir",
                &separate,
                "gitdir",
                msg::HELP_SEPARATE_GIT_DIR,
            ),
            Opt::string(
                'b',
                "initial-branch",
                &branch,
                "name",
                msg::HELP_INITIAL_BRANCH,
            ),
            Opt::string(
                None,
                "object-format",
                &object_format,
                "hash",
                msg::HELP_OBJECT_FORMAT,
            ),
            Opt::string(
                None,
                "ref-format",
                &ref_format,
                "format",
                msg::HELP_REF_FORMAT,
            ),
        ],
    );
    let args = parser.parse(argv)?.args;
    drop(parser);
    if args.len() > 1 {
        return Err(Die::Output(rgit_options::Exit {
            code: rgit_options::EXIT_USAGE,
            stdout: String::new(),
            stderr: format!("usage: {}\n", msg::USAGE),
        }));
    }
    let separate = separate.into_inner();
    if separate.is_some() && bare.get() {
        return Err(Die::Fatal(msg::SEPARATE_AND_BARE.into()));
    }

    let env = SystemEnv::default();
    let cl = command_line(ctx);
    let cwd = current_dir()?;
    let separate_git_dir = match &separate {
        Some(s) => Some(
            path::real_path(Path::new(s), &cwd).map_err(|e| fail(&InitError::Repo(e.into())))?,
        ),
        None => None,
    };
    let template = match template.into_inner() {
        Some(t) => Some(template_dir(&t, &cwd)),
        None => env
            .var("GIT_TEMPLATE_DIR")
            .map(|t| template_dir(&t.to_string_lossy(), &cwd)),
    };

    let early = repo_init::load_config(&env, &cl, None).map_err(|e| fail(&e.into()))?;
    if shared.get().is_none() {
        if let Some(v) = early.string("core.sharedrepository").map_err(|e| fail(&e.into()))? {
            SharedRepository::parse(&String::from_utf8_lossy(v))
                .map_err(|e| fail(&e.into()))?;
        }
    }

    // `init <directory>` creates the directory and runs inside it.
    let directory = args.first();
    let cwd = match directory {
        Some(dir) => {
            repo_init::create_init_directory(&cwd.join(dir)).map_err(|e| {
                Die::Fatal(msg::cannot_mkdir(dir, &repo_init::strerror(&e)))
            })?;
            std::env::set_current_dir(dir).map_err(|e| {
                Die::Fatal(msg::cannot_chdir(dir, &repo_init::strerror(&e)))
            })?;
            current_dir()?
        }
        None => cwd,
    };

    let object_format = match object_format.into_inner() {
        Some(f) => Some(repo_init::parse_hash(&f).map_err(|e| fail(&e))?),
        None => None,
    };
    let ref_format = match ref_format.into_inner() {
        Some(f) => Some(repo_init::parse_ref_format(&f).map_err(|e| fail(&e))?),
        None => None,
    };

    // Where the repository goes.
    let explicit_bare = bare.get() || ctx.bare;
    let given_git_dir: Option<PathBuf> = ctx
        .git_dir
        .clone()
        .map(PathBuf::from)
        .or_else(|| env.var("GIT_DIR").map(PathBuf::from));
    let work_tree: Option<PathBuf> = ctx
        .work_tree
        .clone()
        .map(PathBuf::from)
        .or_else(|| env.var("GIT_WORK_TREE").map(PathBuf::from));
    if work_tree.is_some() && (given_git_dir.is_none() || explicit_bare) {
        return Err(Die::Fatal(msg::WORK_TREE_WITHOUT_GIT_DIR.into()));
    }
    let (git_dir, is_bare) = if explicit_bare && directory.is_some() {
        (cwd.clone(), true)
    } else if let Some(g) = &given_git_dir {
        let abs = path::normalize(&path::absolute(g, &cwd));
        let guess = abs.file_name().is_none_or(|n| n != ".git");
        (abs, explicit_bare || (work_tree.is_none() && guess))
    } else if explicit_bare {
        (cwd.clone(), true)
    } else {
        (cwd.join(".git"), false)
    };
    if !is_bare && work_tree.is_none() && given_git_dir.is_some() {
        // The work tree is the directory containing GIT_DIR; git enters it.
        let top = git_dir.parent().unwrap_or(Path::new("/"));
        if let Err(e) = std::fs::read_dir(top) {
            return Err(Die::Fatal(msg::cannot_access_work_tree(
                &top.display().to_string(),
                &repo_init::strerror(&e),
            )));
        }
    }
    let work_tree = work_tree.map(|w| path::real_path_lossy(&w, &cwd));
    let object_dir = env
        .var("GIT_OBJECT_DIRECTORY")
        .filter(|v| !v.is_empty())
        .map(|o| path::absolute(Path::new(&o), &cwd));

    let opts = InitOptions {
        git_dir,
        separate_git_dir,
        bare: is_bare,
        work_tree,
        object_dir,
        template,
        shared: shared.get(),
        object_format,
        ref_format,
        initial_branch: branch.into_inner(),
    };
    // `--quiet` also silences advice.
    let advisor = ctx
        .advisor_with_env(&early, &env)?
        .without_color_ui()
        .silenced(quiet.get());
    let mut advice_error = None;
    let outcome = repo_init::init(&opts, &env, &cl, early, &mut |n| match n {
        Notice::Warning(w) => eprintln!("{}", prefixed(Level::Warning, &w)),
        Notice::DefaultBranchName => {
            if let Err(e) = advisor.advise(Advice::DefaultBranchName, msg::DEFAULT_BRANCH_ADVICE) {
                advice_error.get_or_insert(e);
            }
        }
    })
    .map_err(|e| fail(&e))?;
    if let Some(e) = advice_error {
        return Err(Die::Fatal(e));
    }
    if !quiet.get() {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(
            out,
            "{}",
            msg::done(
                outcome.reinitialized,
                outcome.shared,
                &outcome.git_dir.display().to_string()
            )
        );
    }
    Ok(0)
}

fn current_dir() -> Result<PathBuf, Die> {
    std::env::current_dir().map_err(|e| {
        Die::Fatal(crate::messages::cannot_get_cwd(&crate::global::strerror(
            &e,
        )))
    })
}

/// `--template=` (empty) means no template; anything else is a directory
/// relative to the directory `init` was started in.
fn template_dir(value: &str, cwd: &Path) -> Template {
    if value.is_empty() {
        Template::Empty
    } else {
        Template::Dir(path::absolute(Path::new(value), cwd))
    }
}

/// Print the error like git and return its exit code.
fn fail(e: &InitError) -> Die {
    let mut err = std::io::stderr().lock();
    for (level, line) in e.report() {
        let _ = match level {
            Some(l) => writeln!(err, "{} {line}", l.prefix()),
            None => writeln!(err, "{line}"),
        };
    }
    Die::Exit(e.exit_code())
}
