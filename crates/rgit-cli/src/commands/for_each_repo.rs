// SPDX-License-Identifier: MIT
//! `git for-each-repo --config=<config> [--keep-going] [--] <arguments>`
//! (Documentation/git-for-each-repo.adoc): run `git -C <repo> <arguments>`
//! for every value of a multi-valued configuration variable. A leading
//! `~/` is the home directory. Without `--keep-going` the first failure
//! ends the run with its exit code; with it the result is 1.

use bstr::ByteSlice;

use crate::messages::maintenance as msg;
use crate::prelude::*;

/// Repository variables a child in another repository must not inherit.
const LOCAL_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_PREFIX",
    "GIT_IMPLICIT_WORK_TREE",
];

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let key: RefCell<Option<String>> = RefCell::new(None);
    let keep_going = Cell::new(false);
    let mut parser = Parser::new(
        msg::FOR_EACH_REPO_USAGE,
        vec![
            Opt::string(None, "config", &key, msg::ARGH_CONFIG, msg::H_FER_CONFIG),
            Opt::flag(None, "keep-going", &keep_going, msg::H_FER_KEEP_GOING),
        ],
    )
    .flags(rgit_options::ParseFlags {
        stop_at_non_option: true,
        ..Default::default()
    });
    let rest = parser.parse(argv)?.args;
    let Some(key) = key.take() else {
        return Err(Die::Fatal(msg::MISSING_CONFIG.into()));
    };
    if let Err(e) = rgit_config::Key::parse(key.as_bytes()) {
        eprintln!("error: {e}");
        return Err(Die::Output(parser.usage_msg(&msg::bad_config(&key))));
    }
    let config = match crate::repo::setup_gently(ctx)? {
        Some(setup) => crate::repo::config(ctx, &setup),
        None => crate::repo::config_outside_repository(ctx),
    };
    let mut paths = Vec::new();
    for e in config.get_all(key.as_str()) {
        match &e.value {
            Some(v) => paths.push(v.to_str_lossy().into_owned()),
            None => {
                eprintln!("error: {}", msg::missing_value(&key));
                return Err(Die::Output(parser.usage_msg(&msg::bad_config(&key))));
            }
        }
    }
    drop(parser);
    let exe = std::env::current_exe().map_err(|e| Die::Fatal(e.to_string()))?;
    let mut env = ctx.child_env();
    env.retain(|(k, _)| !LOCAL_ENV.iter().any(|l| k == l));
    let mut result = 0;
    for path in paths {
        let path = match path.strip_prefix("~/") {
            Some(rest) => match std::env::var_os("HOME") {
                Some(h) => std::path::Path::new(&h)
                    .join(rest)
                    .to_string_lossy()
                    .into_owned(),
                None => path,
            },
            None => path,
        };
        let mut args = vec!["-C".to_string(), path];
        args.extend(rest.iter().cloned());
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(&args);
        for l in LOCAL_ENV {
            cmd.env_remove(l);
        }
        for (k, v) in &env {
            cmd.env(k, v);
        }
        let status = rgit_trace::trace2::status(
            &mut cmd,
            Default::default(),
            &rgit_trace::trace2::git_argv(&args),
        )
        .map_err(|e| Die::Fatal(e.to_string()))?;
        if !status.success() {
            if !keep_going.get() {
                return Ok(rgit_trace::trace2::status_code(status));
            }
            result = 1;
        }
    }
    Ok(result)
}
