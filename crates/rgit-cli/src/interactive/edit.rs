// SPDX-License-Identifier: MIT
//! `git add -e`: the diff between the index and the work tree goes to the
//! editor as `ADD_EDIT.patch` in the git directory; what comes back is
//! applied to the index with the line counts recomputed
//! (Documentation/git-add.adoc, `--edit` and "EDITING PATCHES").

use crate::commands::stash::child::Git;
use crate::messages::interactive as msg;
use crate::prelude::*;
use crate::repository::Repo;

pub(crate) fn run(ctx: &Context, repo: &Repo, pathspec: &[String]) -> CmdResult {
    let git = Git::new(ctx, repo)?;
    let cwd = super::patch::user_dir(repo)?;
    let mut args: Vec<String> = ["diff-files", "--no-color", "-p", "--"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    args.extend(pathspec.iter().cloned());
    let (code, diff) = git.cmd(&args).in_dir(&cwd).output()?;
    if code != 0 {
        return Err(Die::Error(msg::COULD_NOT_PARSE_DIFF.to_owned(), 1));
    }
    let path = repo
        .setup
        .cwd
        .join(&repo.setup.git_dir)
        .join("ADD_EDIT.patch");
    std::fs::write(&path, &diff).map_err(|e| {
        Die::Fatal(msg::could_not_write(
            &path.to_string_lossy(),
            &crate::global::strerror(&e),
        ))
    })?;
    let env = rgit_config::SystemEnv::default();
    let editor = rgit_ident::editor::editor(&repo.config, &env).map_err(|p| {
        p.print();
        Die::Exit(rgit_options::EXIT_FATAL)
    })?;
    let launch = rgit_ident::editor::LaunchOptions {
        hint: false,
        env: ctx.child_env(),
        cwd: None,
    };
    if let Err(e) = rgit_ident::editor::launch(editor.as_deref(), &path, &launch) {
        e.print();
        let _ = std::fs::remove_file(&path);
        return Err(Die::Fatal(msg::EDITING_PATCH_FAILED.to_owned()));
    }
    let edited = std::fs::read(&path).unwrap_or_default();
    if edited.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Err(Die::Fatal(msg::EMPTY_PATCH.to_owned()));
    }
    let code = git
        .cmd(&["apply", "--recount", "--cached"])
        .stdin(edited)
        .status()?;
    let _ = std::fs::remove_file(&path);
    if code != 0 {
        return Err(Die::Exit(1));
    }
    Ok(0)
}
