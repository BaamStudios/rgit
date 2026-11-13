// SPDX-License-Identifier: MIT
//! What the `remote` subcommands share: the repository, configuration
//! edits, branch settings and the connection to the remote.

use std::io::Write;
use std::path::PathBuf;

use bstr::ByteSlice;
use rgit_config::write::{self as config_write, WriteError};
use rgit_config::{ConfigSet, Entry, Scope, SetRequest, SystemEnv, ValuePattern};
use rgit_fetch::{ConnectSettings, Session};
use rgit_protocol::v2::RemoteRef;
use rgit_refs::files::FilesStore;
use rgit_remote::{Remote, Remotes};
use rgit_repo::Setup;
use rgit_transport::IpVersion;

use crate::prelude::*;
use crate::refs_support;
use crate::repo::{self, Mode};

/// The repository a subcommand works on.
pub struct Repo {
    pub setup: Setup,
    pub config: ConfigSet,
    pub store: FilesStore,
}

impl Repo {
    pub fn open(ctx: &Context) -> Result<Repo, Die> {
        let setup = repo::setup(ctx, Mode::Required)?;
        let config = refs_support::load_config(ctx, &setup)?;
        let store = refs_support::ref_store(&setup, &config);
        Ok(Repo {
            setup,
            config,
            store,
        })
    }

    /// The configuration file the subcommands write (`$GIT_DIR/config`).
    pub fn config_path(&self) -> PathBuf {
        self.setup.common_dir.join("config")
    }

    pub fn remotes(&self) -> Result<Remotes, Die> {
        Remotes::from_config(&self.config).map_err(|e| Die::Fatal(e.to_string()))
    }

    /// Whether the repository's own configuration (not the global or
    /// command-line one) mentions `remote.<name>.*`.
    pub fn configured_in_repo(&self, name: &str) -> bool {
        self.config.entries().iter().any(|e| {
            in_repo(e)
                && matches!(
                    rgit_config::key::split_canonical(e.name.as_bstr()),
                    (s, Some(sub), _) if s == "remote" && sub == name.as_bytes()
                )
        })
    }

    /// `$GIT_DIR/remotes/<name>` or `$GIT_DIR/branches/<name>`, if one
    /// exists.
    pub fn legacy_file(&self, name: &str) -> Option<PathBuf> {
        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return None;
        }
        ["remotes", "branches"]
            .iter()
            .map(|d| self.setup.common_dir.join(d).join(name))
            .find(|p| p.is_file())
    }

    /// Whether `name` is a remote of this repository, as `add`, `rename`,
    /// `remove` and the URL subcommands see it.
    pub fn exists(&self, name: &str) -> bool {
        self.configured_in_repo(name) || self.legacy_file(name).is_some()
    }

    /// The remote `name`, printing the warnings its definition causes.
    pub fn remote(&self, remotes: &Remotes, name: &str) -> Result<Remote, Die> {
        let remote = remotes
            .get(name.as_bytes(), Some(&self.setup.git_dir))
            .map_err(|e| Die::Fatal(e.to_string()))?;
        for w in &remote.warnings {
            eprintln!("warning: {w}");
        }
        Ok(remote)
    }

    /// Change `key` in the repository configuration file.
    pub fn set_request(
        &self,
        key: &str,
        value: Option<&str>,
        pattern: &ValuePattern,
        all: bool,
    ) -> Result<(), WriteError> {
        let req = SetRequest {
            key: key.as_bytes(),
            value: value.map(str::as_bytes),
            pattern,
            all,
            comment: None,
        };
        config_write::set_in_file(&self.config_path(), &Default::default(), &req)
    }

    /// `git config <key> <value>`.
    pub fn set(&self, key: &str, value: &str) -> Result<(), Die> {
        self.set_request(key, Some(value), &ValuePattern::Any, false)
            .map_err(write_die)
    }

    /// `git config --add <key> <value>`.
    pub fn add(&self, key: &str, value: &str) -> Result<(), Die> {
        self.set_request(key, Some(value), &ValuePattern::Never, false)
            .map_err(write_die)
    }

    /// `git config --unset-all <key>`; `Ok(false)` when there was nothing
    /// to remove.
    pub fn unset_all(&self, key: &str) -> Result<bool, Die> {
        match self.set_request(key, None, &ValuePattern::Any, true) {
            Ok(()) => Ok(true),
            Err(e) if is_nothing_to_unset(&e) => Ok(false),
            Err(e) => Err(write_die(e)),
        }
    }

    /// Rename (`new = Some`) or remove the section `old`; a missing section
    /// is not an error.
    pub fn rename_section(&self, old: &str, new: Option<&str>) -> Result<(), Die> {
        match config_write::rename_section_in_file(
            &self.config_path(),
            &Default::default(),
            old.as_bytes(),
            new.map(str::as_bytes),
        ) {
            Ok(()) => Ok(()),
            Err(e)
                if matches!(
                    e.edit_error(),
                    Some(rgit_config::EditError::NoSuchSection(_))
                ) =>
            {
                Ok(())
            }
            Err(e) => Err(write_die(e)),
        }
    }

    /// Values of `key` from the repository's own configuration.
    pub fn repo_values(&self, key: &str) -> Vec<String> {
        self.config
            .get_all(key)
            .filter(|e| in_repo(e))
            .filter_map(|e| e.value.as_ref().map(|v| v.to_str_lossy().into_owned()))
            .collect()
    }

    /// Connect to `remote` and list its references (`ls-refs`).
    pub fn query(&self, ctx: &Context, remote: &Remote) -> Result<Vec<RemoteRef>, Die> {
        let url = remote
            .fetch_url()
            .map(|u| u.to_str_lossy().into_owned())
            .unwrap_or_default();
        let env = SystemEnv::default();
        let settings = ConnectSettings {
            env: &env,
            config: &self.config,
            base_env: ctx.child_env(),
            upload_pack: remote
                .upload_pack
                .as_ref()
                .map(|u| u.to_str_lossy().into_owned()),
            ip: IpVersion::Any,
        };
        let fatal = |e: rgit_fetch::Error| Die::Fatal(e.to_string());
        let mut session = Session::connect(&url, &settings, "remote").map_err(fatal)?;
        let refs = session.ls_refs(&[]).map_err(fatal)?;
        let _ = session.close();
        Ok(refs)
    }
}

/// Whether an entry comes from the repository's own configuration files.
pub fn in_repo(e: &Entry) -> bool {
    matches!(e.source.scope, Scope::Local | Scope::Worktree)
}

pub fn is_nothing_to_unset(e: &WriteError) -> bool {
    matches!(e.edit_error(), Some(rgit_config::EditError::NothingToUnset))
}

/// Print a configuration write error like git and exit with its code.
pub fn write_die(e: WriteError) -> Die {
    let mut err = std::io::stderr().lock();
    for (level, line) in e.report() {
        let _ = writeln!(err, "{} {line}", level.prefix());
    }
    Die::Exit(e.exit_code())
}

/// Whether `name` can name a remote: its remote-tracking branches
/// `refs/remotes/<name>/<branch>` must be valid reference names (observed:
/// git rejects names with `..`, `:`, spaces, and the empty name).
pub fn valid_name(name: &str) -> bool {
    let mut probe = String::from("refs/remotes/");
    probe.push_str(name);
    probe.push_str("/x");
    rgit_refs::name::is_valid_refname(probe.as_bytes(), rgit_refs::name::RefNameFlags::STRICT)
}

/// How `git pull` integrates a branch (`branch.<name>.rebase`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rebase {
    No,
    Yes,
    Interactive,
    Merges,
}

/// The `branch.<name>.*` settings the subcommands look at.
#[derive(Debug, Clone)]
pub struct Branch {
    pub name: String,
    pub remote: Option<String>,
    pub push_remote: Option<String>,
    pub merge: Vec<String>,
    pub rebase: Rebase,
}

/// All branches with settings, in the order they first appear. A branch
/// with more than one `branch.<name>.remote` gets a warning; the last
/// value counts.
pub fn read_branches(config: &ConfigSet) -> Vec<Branch> {
    let mut out: Vec<Branch> = Vec::new();
    for e in config.entries() {
        let (section, Some(sub), key) = rgit_config::key::split_canonical(e.name.as_bstr()) else {
            continue;
        };
        if section != "branch" {
            continue;
        }
        let key = key.to_str_lossy();
        if !matches!(key.as_ref(), "remote" | "pushremote" | "merge" | "rebase") {
            continue;
        }
        let name = sub.to_str_lossy().into_owned();
        let idx = match out.iter().position(|b| b.name == name) {
            Some(i) => i,
            None => {
                out.push(Branch {
                    name: name.clone(),
                    remote: None,
                    push_remote: None,
                    merge: Vec::new(),
                    rebase: Rebase::No,
                });
                out.len() - 1
            }
        };
        let b = &mut out[idx];
        let value = e.value.as_ref().map(|v| v.to_str_lossy().into_owned());
        match key.as_ref() {
            "remote" => {
                if b.remote.is_some() {
                    eprintln!(
                        "warning: {}",
                        crate::messages::remote::more_than_one(&format!("branch.{name}.remote"))
                    );
                }
                b.remote = value;
            }
            "pushremote" => b.push_remote = value,
            "merge" => {
                if let Some(v) = value {
                    b.merge
                        .extend(v.split(' ').filter(|s| !s.is_empty()).map(str::to_owned));
                }
            }
            _ => {
                b.rebase = match value.as_deref().map(str::to_ascii_lowercase).as_deref() {
                    Some("interactive" | "i") => Rebase::Interactive,
                    Some("merges" | "m") => Rebase::Merges,
                    _ => match e.bool() {
                        Ok(false) => Rebase::No,
                        _ => Rebase::Yes,
                    },
                }
            }
        }
    }
    out
}

/// Strip `refs/heads/` for display.
pub fn short_branch(name: &str) -> &str {
    name.strip_prefix("refs/heads/").unwrap_or(name)
}
