// SPDX-License-Identifier: MIT
//! The comment string (`core.commentChar`, `core.commentString`) and the
//! configuration of commands that work inside or outside a repository
//! (`stripspace`, `interpret-trailers`).
//!
//! Each `core.commentChar` / `core.commentString` entry is checked in
//! reading order (an empty value or one with a newline is fatal); the last
//! one wins. `auto` asks the command to pick a character the text does not
//! use; commands that do not edit text use `#`.

use bstr::ByteSlice;
use rgit_config::{ConfigSet, Entry, Origin, SystemEnv};

use crate::messages::comment as msg;
use crate::{Context, Die};

/// The configured comment string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Comment {
    /// `auto`: pick one that the text does not use.
    Auto,
    Fixed(Vec<u8>),
}

impl Comment {
    /// The string for commands that do not choose: `auto` means `#`.
    pub fn or_hash(&self) -> Vec<u8> {
        match self {
            Comment::Auto => b"#".to_vec(),
            Comment::Fixed(s) => s.clone(),
        }
    }
}

/// Report an unusable entry: `error: <what>` and git's `fatal:` line naming
/// the entry's source.
pub fn bad_entry(e: &Entry, what: &str) -> Die {
    let name = e.name.to_str_lossy();
    eprintln!("error: {what}");
    let fatal = match &e.source.origin {
        Origin::CommandLine => {
            crate::messages::unable_to_parse_config(&name, "command-line config")
        }
        Origin::File(p) => {
            // git names repository files relative to the top level it
            // moved to (`.git/config`).
            let cwd = std::env::current_dir().ok();
            let shown = cwd
                .as_deref()
                .and_then(|c| p.strip_prefix(c).ok())
                .unwrap_or(p);
            crate::messages::bad_config_variable(&name, &shown.display().to_string(), e.line)
        }
        other => crate::messages::unable_to_parse_config(&name, &other.describe()),
    };
    Die::Fatal(fatal)
}

/// The comment string from the configuration (default `#`).
pub fn comment(set: &ConfigSet) -> Result<Comment, Die> {
    let mut out = Comment::Fixed(b"#".to_vec());
    for e in set.entries() {
        if e.name != "core.commentchar" && e.name != "core.commentstring" {
            continue;
        }
        let name = e.name.to_str_lossy();
        let Some(v) = &e.value else {
            return Err(bad_entry(e, &msg::missing_value(&name)));
        };
        if v.is_empty() {
            return Err(bad_entry(e, &msg::empty_comment(&name)));
        }
        if v.contains(&b'\n') {
            return Err(bad_entry(e, &msg::newline_in_comment(&name)));
        }
        out = if v == "auto" {
            Comment::Auto
        } else {
            Comment::Fixed(v.to_vec())
        };
    }
    Ok(out)
}

/// Discover a repository the gentle way (moving to its top level like
/// git) and load the configuration; outside a repository the system,
/// global and command-line configuration. Errors in it are fatal.
pub fn gentle_config(ctx: &Context) -> Result<ConfigSet, Die> {
    if let Some(setup) = crate::repo::setup_gently(ctx)? {
        return crate::repo::load_config(ctx, &setup);
    }
    let env = SystemEnv::default();
    let opts = rgit_config::LoadOptions {
        env: &env,
        repo: None,
        includes: true,
    };
    let mut set = ConfigSet::new();
    if let Err(e) = rgit_config::sources::load_standard(
        opts,
        None,
        &crate::repo::command_line(ctx),
        rgit_config::sources::ScopeFilter::default(),
        &mut set,
    ) {
        for (level, line) in e.report() {
            eprintln!("{} {line}", level.prefix());
        }
        return Err(Die::Exit(rgit_options::EXIT_FATAL));
    }
    Ok(set)
}
