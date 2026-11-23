// SPDX-License-Identifier: MIT
//! trace2 glue for the binary: settings from the system and global
//! configuration (Documentation/config/trace2.adoc: repository and `-c`
//! settings are not used), the command name and the "important" settings
//! listed as `def_param`.

use rgit_config::sources::{self, ScopeFilter};
use rgit_config::{ConfigSet, LoadOptions, SystemEnv};
use rgit_trace::trace2;

use crate::Context;

/// Start trace2 for this process.
pub fn init(args: &[String]) {
    let env = SystemEnv::default();
    let mut config = ConfigSet::new();
    let filter = ScopeFilter {
        local: false,
        worktree: false,
        command: false,
        ..ScopeFilter::default()
    };
    // Unreadable files only mean there are no settings from them.
    let _ = sources::load_standard(
        LoadOptions {
            env: &env,
            repo: None,
            includes: true,
        },
        None,
        &Default::default(),
        filter,
        &mut config,
    );
    let lookup = |key: &str| -> Option<String> {
        config
            .get(key)
            .map(|e| e.value.as_ref().map_or_else(String::new, |v| v.to_string()))
    };
    trace2::init(args, &rgit_version::version(), &lookup);
}

/// Built-ins that git runs without looking for a repository first (the
/// others report the repository before their name).
const NO_SETUP: &[&str] = &[
    "check-ref-format",
    "clone",
    "diff",
    "hash-object",
    "help",
    "init",
    "ls-remote",
    "rev-parse",
    "stripspace",
    "version",
];

/// `cmd_name` followed by the `def_param` events (once per process).
/// `builtin`: `name` is a built-in command.
pub fn cmd_name(ctx: &Context, name: &str, builtin: bool) {
    if !trace2::enabled() {
        return;
    }
    let early = crate::alias::EarlyConfig::load(ctx).ok();
    if builtin && !NO_SETUP.contains(&name) {
        if let Some(setup) = early.as_ref().and_then(|e| e.setup.as_ref()) {
            if let Some(wt) = &setup.work_tree {
                trace2::def_repo(&setup.cwd.join(wt).to_string_lossy());
            }
        }
    }
    trace2::cmd_name(name);
    list_params(early.as_ref());
}

/// The configuration values matching `trace2.configParams` and the
/// variables of `trace2.envVars`.
fn list_params(early: Option<&crate::alias::EarlyConfig>) {
    let patterns = trace2::config_param_patterns();
    let mut params = Vec::new();
    if !patterns.is_empty() {
        if let Some(early) = early {
            for e in early.config.entries() {
                let key = e.name.to_string();
                if trace2::is_config_param(&patterns, &key) {
                    let value = e.value.as_ref().map_or_else(String::new, |v| v.to_string());
                    params.push((key, value, e.source.scope.as_str().to_string()));
                }
            }
        }
    }
    trace2::def_params(&params);
}
