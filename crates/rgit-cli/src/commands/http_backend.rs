// SPDX-License-Identifier: MIT
//! `git http-backend` (Documentation/git-http-backend.adoc): the CGI
//! program that serves repositories over smart and dumb HTTP.
//!
//! The request is described by the CGI environment and answered by
//! `rgit_server::http_backend`; this file only finds and opens the
//! repository (like `upload-pack`: `<dir>/.git`, `<dir>`, `<dir>.git`)
//! and connects the standard streams. Arguments are ignored, as git
//! does: a CGI program gets none it could trust.

use std::io::Write;

use rgit_refs::files::FilesStore;
use rgit_server::http_backend::{self as service, Host, Opened, Repository};

use crate::prelude::*;
use crate::repo::{self, Mode};

struct CliHost<'a> {
    ctx: &'a mut Context,
}

impl Host for CliHost<'_> {
    fn open(&mut self, dir: &str) -> Result<Option<Repository>, Opened> {
        // A broken `.git` file or a repository owned by someone else ends
        // the request (git answers 500 after its `fatal:` message).
        let Some(found) = super::upload_pack::find_repo(dir, false).map_err(|_| Opened::Died)?
        else {
            return Ok(None);
        };
        super::upload_pack::check_ownership(self.ctx, &found).map_err(|_| Opened::Died)?;
        Ok(self.load(&found.git_dir))
    }
}

impl CliHost<'_> {
    fn load(&mut self, git_dir: &std::path::Path) -> Option<Repository> {
        std::env::set_current_dir(git_dir).ok()?;
        let git_dir = std::env::current_dir().ok()?;
        self.ctx.git_dir = Some(".".into());
        let setup = repo::setup(self.ctx, Mode::RequiredQuiet).ok()?;
        let hash = setup.format.object_format;
        let config = repo::load_config(self.ctx, &setup).ok()?;
        let odb = repo::odb_with_config(&setup, &config).ok()?;
        let refs = FilesStore::new(&setup.common_dir, hash).with_worktree(&setup.git_dir);
        let objects_dir = git_dir.join(rgit_odb::objects_dir(&setup.common_dir));
        Some(Repository {
            git_dir,
            objects_dir,
            hash,
            config,
            refs: Box::new(refs),
            odb,
        })
    }
}

/// The program that runs the services: like git, the `git` of an exec path
/// set explicitly (`GIT_EXEC_PATH`, as web server configurations do, or
/// `--exec-path`), so that the services are that installation's programs;
/// otherwise rgit itself.
fn service_program(ctx: &Context) -> Result<std::path::PathBuf, Die> {
    let explicit = ctx.exec_path_overridden
        || std::env::var_os("GIT_EXEC_PATH").is_some_and(|p| !p.is_empty());
    if explicit {
        let git = ctx
            .exec_path
            .join(if cfg!(windows) { "git.exe" } else { "git" });
        if crate::external::is_executable(&git) {
            return Ok(git);
        }
    }
    std::env::current_exe().map_err(|e| Die::Fatal(e.to_string()))
}

pub fn run(ctx: &mut Context, _argv: &[String]) -> CmdResult {
    let exe = service_program(ctx)?;
    let opts = service::Options {
        git: vec![exe.into_os_string()],
        env: ctx.child_env(),
        namespace: ctx
            .namespace
            .clone()
            .or_else(|| std::env::var("GIT_NAMESPACE").ok())
            .filter(|n| !n.is_empty()),
    };
    let var = |name: &str| std::env::var(name).ok();
    let mut host = CliHost { ctx };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut err = std::io::stderr();
    let code = service::http_backend(&var, &mut host, &opts, std::io::stdin(), &mut out, &mut err);
    let _ = out.flush();
    Ok(code)
}
