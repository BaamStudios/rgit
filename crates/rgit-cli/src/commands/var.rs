// SPDX-License-Identifier: MIT
//! `git var` (Documentation/git-var.adoc): print a logical variable, or
//! with `-l` the configuration followed by all logical variables.
//!
//! Behaviour checked against the `git` binary (t0007): asking for one
//! identity is strict, `-l` lists lenient identities; variables without a
//! value exit with 1 (single) or are left out (`-l`); path variables may
//! have several values, which `-l` prints as one line each.

use std::io::Write;
use std::path::{Path, PathBuf};

use bstr::{BString, ByteSlice};
use rgit_config::sources::{self, RepoConfig, ScopeFilter};
use rgit_config::{ConfigSet, Environment, LoadOptions, StaticRepoFacts, SystemEnv};
use rgit_ident::editor;
use rgit_ident::ident::{IdentConfig, Resolver, Role, Strictness};
use rgit_ident::{Problem, config as icfg};
use rgit_refs::name::{RefNameFlags, check_refname_format};

use crate::messages::var as msg;
use crate::prelude::*;

/// Compiled-in system attributes file.
const SYSTEM_ATTRIBUTES: &str = "/etc/gitattributes";
/// Initial branch name when nothing is configured.
const DEFAULT_BRANCH: &str = "master";

/// The logical variables in `-l` order.
const VARIABLES: &[&str] = &[
    "GIT_COMMITTER_IDENT",
    "GIT_AUTHOR_IDENT",
    "GIT_EDITOR",
    "GIT_SEQUENCE_EDITOR",
    "GIT_PAGER",
    "GIT_DEFAULT_BRANCH",
    "GIT_SHELL_PATH",
    "GIT_ATTR_SYSTEM",
    "GIT_ATTR_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_GLOBAL",
];

/// Variables whose value is a list of paths, one per line.
fn is_multi(name: &str) -> bool {
    name == "GIT_CONFIG_GLOBAL"
}

fn usage_error() -> Die {
    eprintln!("{}", msg::USAGE);
    Die::Exit(rgit_options::EXIT_USAGE)
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let args = &argv[1..];
    if args.len() == 1 && args[0] == "-h" {
        println!("{}", msg::USAGE);
        return Ok(0);
    }
    if args.len() != 1 {
        return Err(usage_error());
    }
    let list = args[0] == "-l";
    if !list && !VARIABLES.contains(&args[0].as_str()) {
        return Err(usage_error());
    }

    let env = SystemEnv::default();
    let set = load_config(ctx, &env)?;
    // Everything goes out at the end, after any error message, like git's
    // buffered stdout.
    let mut buf = Vec::new();
    let result = if list {
        list_all(&set, &env, &mut buf)
    } else {
        match Vars::new(&set, &env)
            .map_err(problem)
            .and_then(|v| v.get(&args[0], Strictness::Strict))
        {
            Ok(Some(value)) => {
                buf.extend_from_slice(&value);
                buf.push(b'\n');
                Ok(0)
            }
            Ok(None) => Ok(1),
            Err(e) => Err(e),
        }
    };
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(&buf);
    let _ = out.flush();
    result
}

/// `-l`: the configuration, then every variable that has a value.
fn list_all(set: &ConfigSet, env: &dyn Environment, buf: &mut Vec<u8>) -> CmdResult {
    for e in set.entries() {
        buf.extend_from_slice(&e.name);
        if let Some(v) = &e.value {
            buf.push(b'=');
            buf.extend_from_slice(v);
        }
        buf.push(b'\n');
    }
    let vars = Vars::new(set, env).map_err(problem)?;
    for name in VARIABLES {
        let Some(value) = vars.get(name, Strictness::Lenient)? else {
            continue;
        };
        let lines: Vec<&[u8]> = if is_multi(name) {
            value.lines().collect()
        } else {
            vec![&value[..]]
        };
        for line in lines {
            buf.extend_from_slice(name.as_bytes());
            buf.push(b'=');
            buf.extend_from_slice(line);
            buf.push(b'\n');
        }
    }
    Ok(0)
}

fn problem(p: Problem) -> Die {
    p.print();
    Die::Exit(rgit_options::EXIT_FATAL)
}

/// Everything the variables are computed from.
struct Vars<'a> {
    set: &'a ConfigSet,
    env: &'a dyn Environment,
    ident: IdentConfig,
    attributes_file: Option<PathBuf>,
}

impl<'a> Vars<'a> {
    /// Read the configuration git validates up front (identity, editor,
    /// attributes file), whatever variable is asked for.
    fn new(set: &'a ConfigSet, env: &'a dyn Environment) -> Result<Self, Problem> {
        let ident = IdentConfig::from_config(set)?;
        icfg::string_all(set, "core.editor")?;
        let attributes_file = icfg::path(set, "core.attributesFile", env)?;
        Ok(Vars {
            set,
            env,
            ident,
            attributes_file,
        })
    }

    fn get(&self, name: &str, strictness: Strictness) -> Result<Option<BString>, Die> {
        let p = |r: Result<Option<String>, Problem>| r.map(|o| o.map(BString::from)).map_err(problem);
        match name {
            "GIT_AUTHOR_IDENT" => self.ident(Role::Author, strictness).map(Some),
            "GIT_COMMITTER_IDENT" => self.ident(Role::Committer, strictness).map(Some),
            "GIT_EDITOR" => p(editor::editor(self.set, self.env)),
            "GIT_SEQUENCE_EDITOR" => p(editor::sequence_editor(self.set, self.env)),
            "GIT_PAGER" => p(editor::pager(self.set, self.env).map(Some)),
            "GIT_DEFAULT_BRANCH" => self.default_branch().map(Some),
            "GIT_SHELL_PATH" => Ok(Some(editor::SHELL_PATH.into())),
            "GIT_ATTR_SYSTEM" => {
                let off = icfg::env_bool(self.env, "GIT_ATTR_NOSYSTEM", false).map_err(problem)?;
                Ok((!off).then(|| SYSTEM_ATTRIBUTES.into()))
            }
            "GIT_ATTR_GLOBAL" => Ok(self.attr_global().map(|p| path_bytes(&p))),
            "GIT_CONFIG_SYSTEM" => {
                let off =
                    icfg::env_bool(self.env, "GIT_CONFIG_NOSYSTEM", false).map_err(problem)?;
                if off {
                    return Ok(None);
                }
                Ok(Some(match self.env.var_bytes("GIT_CONFIG_SYSTEM") {
                    Some(v) => v,
                    None => sources::DEFAULT_SYSTEM_CONFIG.into(),
                }))
            }
            "GIT_CONFIG_GLOBAL" => {
                let paths = self.config_global();
                if paths.is_empty() {
                    return Ok(None);
                }
                let mut out = BString::default();
                for (i, p) in paths.iter().enumerate() {
                    if i > 0 {
                        out.push(b'\n');
                    }
                    out.extend_from_slice(&path_bytes(p));
                }
                Ok(Some(out))
            }
            _ => Err(usage_error()),
        }
    }

    fn ident(&self, role: Role, strictness: Strictness) -> Result<BString, Die> {
        match Resolver::new(&self.ident, self.env).ident(role, strictness) {
            Ok(id) => Ok(id.to_bstring()),
            Err(e) => {
                eprint!("{}", e.stderr_text());
                Err(Die::Exit(rgit_options::EXIT_FATAL))
            }
        }
    }

    /// `GIT_TEST_DEFAULT_INITIAL_BRANCH_NAME`, `init.defaultBranch`, or
    /// `master`; the name must be valid for `refs/heads/<name>`.
    fn default_branch(&self) -> Result<BString, Die> {
        let name = match self
            .env
            .var_bytes("GIT_TEST_DEFAULT_INITIAL_BRANCH_NAME")
            .filter(|v| !v.is_empty())
        {
            Some(v) => v,
            None => match icfg::string(self.set, "init.defaultBranch").map_err(problem)? {
                Some(v) => v,
                None => return Ok(DEFAULT_BRANCH.into()),
            },
        };
        let mut full = b"refs/heads/".to_vec();
        full.extend_from_slice(&name);
        if check_refname_format(&full, RefNameFlags::STRICT).is_err() {
            return Err(Die::Fatal(msg::invalid_branch_name(&name.to_str_lossy())));
        }
        Ok(name)
    }

    /// `core.attributesFile`, else `$XDG_CONFIG_HOME/git/attributes` or
    /// `$HOME/.config/git/attributes`.
    fn attr_global(&self) -> Option<PathBuf> {
        if let Some(p) = &self.attributes_file {
            return Some(p.clone());
        }
        self.xdg_file("attributes")
    }

    /// `$XDG_CONFIG_HOME/git/<name>` (when set and not empty), else
    /// `$HOME/.config/git/<name>` (when `HOME` is set, even empty).
    fn xdg_file(&self, name: &str) -> Option<PathBuf> {
        if let Some(x) = self.env.var("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
            return Some(PathBuf::from(x).join("git").join(name));
        }
        let home = self.env.var("HOME")?;
        Some(join_home(&home, ".config/git").join(name))
    }

    /// The global configuration files, highest priority first:
    /// `GIT_CONFIG_GLOBAL` (when not empty; an empty value means none),
    /// else the XDG file and `~/.gitconfig`.
    fn config_global(&self) -> Vec<PathBuf> {
        if let Some(g) = self.env.var("GIT_CONFIG_GLOBAL") {
            return if g.is_empty() {
                Vec::new()
            } else {
                vec![PathBuf::from(g)]
            };
        }
        let mut out = Vec::new();
        if let Some(x) = self.xdg_file("config") {
            out.push(x);
        }
        if let Some(home) = self.env.var("HOME") {
            out.push(join_home(&home, ".gitconfig"));
        }
        out
    }
}

/// `$HOME/<rest>` the way git builds it: an empty `HOME` gives `/<rest>`.
fn join_home(home: &std::ffi::OsStr, rest: &str) -> PathBuf {
    if home.is_empty() {
        PathBuf::from("/").join(rest)
    } else {
        Path::new(home).join(rest)
    }
}

fn path_bytes(p: &Path) -> BString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        BString::from(p.as_os_str().as_bytes())
    }
    #[cfg(not(unix))]
    {
        BString::from(p.to_string_lossy().as_bytes())
    }
}

/// Discover the repository like git's gentle setup (no repository, or an
/// unusable `GIT_DIR`, is fine) and load the configuration.
fn load_config(ctx: &Context, env: &SystemEnv) -> Result<ConfigSet, Die> {
    let repo = discover(ctx, env);
    let facts = repo.as_ref().map(|(git_dir, common_dir, work_tree)| {
        (
            StaticRepoFacts {
                git_dir: std::path::absolute(git_dir).ok(),
                worktree: work_tree.clone(),
                branch: current_branch(common_dir),
            },
            RepoConfig {
                common_dir: common_dir.clone(),
                git_dir: git_dir.clone(),
                worktree_config: None,
            },
        )
    });
    let opts = LoadOptions {
        env,
        repo: facts
            .as_ref()
            .map(|(f, _)| f as &dyn rgit_config::RepoFacts),
        includes: true,
    };
    let mut set = ConfigSet::new();
    let result = sources::load_standard(
        opts,
        facts.as_ref().map(|(_, r)| r),
        &crate::repo::command_line(ctx),
        ScopeFilter::default(),
        &mut set,
    );
    if let Err(e) = result {
        for (level, line) in e.report() {
            eprintln!("{} {line}", level.prefix());
        }
        return Err(Die::Exit(rgit_options::EXIT_FATAL));
    }
    Ok(set)
}

/// (git dir, common dir, work tree) of the repository, if any.
fn discover(ctx: &Context, env: &SystemEnv) -> Option<(PathBuf, PathBuf, Option<PathBuf>)> {
    let mut opts = rgit_repo::SetupOptions::from_process().ok()?;
    if let Some(d) = &ctx.git_dir {
        opts.git_dir = Some(d.into());
    }
    if let Some(w) = &ctx.work_tree {
        opts.work_tree = Some(w.into());
    }
    if ctx.bare {
        opts.implicit_work_tree = false;
    }
    let mut protected =
        rgit_repo::StandardProtectedConfig::new(env, crate::repo::command_line(ctx));
    let setup = match rgit_repo::discover(&opts, &mut protected) {
        Ok(Ok(setup)) => setup,
        _ => return None,
    };
    for w in &setup.warnings {
        eprintln!("warning: {w}");
    }
    if setup.cwd != opts.cwd {
        let _ = std::env::set_current_dir(&setup.cwd);
    }
    Some((setup.git_dir, setup.common_dir, setup.work_tree))
}

/// The current branch from `HEAD`, for `includeIf.onbranch`.
fn current_branch(git_dir: &Path) -> Option<BString> {
    let head = std::fs::read(git_dir.join("HEAD")).ok()?;
    let r = head.strip_prefix(b"ref: refs/heads/")?;
    Some(r.trim_ascii_end().into())
}
