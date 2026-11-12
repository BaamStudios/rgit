// SPDX-License-Identifier: MIT
//! `git credential (fill|approve|reject|capability)`
//! (Documentation/git-credential.adoc). The logic lives in
//! `rgit_transport::credential`.

use std::io::{self, Write};

use rgit_config::sources::{RepoConfig, ScopeFilter, load_standard};
use rgit_config::{ConfigSet, LoadOptions, StaticRepoFacts, SystemEnv};
use rgit_repo::{SetupOptions, StandardProtectedConfig};
use rgit_transport::credential::{
    Audience, CredentialContext, SystemPrompter, read_caller_credential, write_capabilities,
};
use rgit_transport::messages::CREDENTIAL_USAGE;

use crate::prelude::*;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let action = match argv.get(1).map(String::as_str) {
        // Like every builtin in git 2.56: help on stdout, exit 0.
        Some("-h" | "--help-all") if argv.len() == 2 => {
            println!("usage: {CREDENTIAL_USAGE}");
            return Ok(0);
        }
        Some(a @ ("fill" | "approve" | "reject" | "capability")) if argv.len() == 2 => a,
        _ => {
            eprintln!("usage: {CREDENTIAL_USAGE}");
            return Ok(129);
        }
    };
    if action == "capability" {
        let mut out = io::stdout().lock();
        write_capabilities(&mut out).and_then(|()| out.flush()).map_err(io_die)?;
        return Ok(0);
    }

    let env = SystemEnv::default();
    let config = load_config(ctx, &env)?;
    let mut cred = read_caller_credential(&mut io::stdin().lock()).map_err(fatal)?;
    let mut prompter = SystemPrompter;
    let mut cctx = CredentialContext {
        env: &env,
        config: &config,
        base_env: ctx.child_env(),
        prompter: &mut prompter,
    };
    match action {
        "fill" => {
            cred.fill(&mut cctx).map_err(fatal)?;
            let mut out = io::stdout().lock();
            let written = cred.write_to(&mut out, Audience::Caller, cred.settings().protect_protocol);
            let _ = out.flush();
            written.map_err(fatal)?;
        }
        "approve" => cred.approve(&mut cctx).map_err(fatal)?,
        _ => cred.reject(&mut cctx).map_err(fatal)?,
    }
    Ok(0)
}

fn fatal(e: rgit_transport::Error) -> Die {
    Die::Fatal(e.to_string())
}

fn io_die(e: io::Error) -> Die {
    Die::Fatal(e.to_string())
}

/// The full configuration (system, global, repository if there is one,
/// command line); `git credential` works inside and outside a repository.
fn load_config(ctx: &Context, env: &SystemEnv) -> Result<ConfigSet, Die> {
    let command_line = crate::repo::command_line(ctx);
    let mut repo = None;
    if let Ok(mut opts) = SetupOptions::from_process() {
        if let Some(d) = &ctx.git_dir {
            opts.git_dir = Some(d.into());
        }
        let mut protected = StandardProtectedConfig::new(env, command_line.clone());
        if let Ok(Ok(setup)) = rgit_repo::discover(&opts, &mut protected) {
            repo = Some((
                RepoConfig {
                    common_dir: setup.absolute_common_dir(),
                    git_dir: setup.absolute_git_dir(),
                    worktree_config: None,
                },
                StaticRepoFacts {
                    git_dir: Some(setup.absolute_git_dir()),
                    worktree: setup.work_tree.clone(),
                    branch: None,
                },
            ));
        }
    }
    let mut set = ConfigSet::new();
    let result = load_standard(
        LoadOptions {
            env,
            repo: repo.as_ref().map(|(_, f)| f as &dyn rgit_config::RepoFacts),
            includes: true,
        },
        repo.as_ref().map(|(r, _)| r),
        &command_line,
        ScopeFilter::default(),
        &mut set,
    );
    if let Err(e) = result {
        let mut err = io::stderr();
        for (level, msg) in e.report() {
            let _ = writeln!(err, "{} {msg}", level.prefix());
        }
        return Err(Die::Exit(rgit_options::EXIT_FATAL));
    }
    Ok(set)
}
