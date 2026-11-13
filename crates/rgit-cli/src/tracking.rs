// SPDX-License-Identifier: MIT
//! Upstream tracking for new branches, shared by `branch`, `switch` and
//! `checkout` (Documentation/git-branch.adoc `--track`,
//! Documentation/config/branch.adoc `branch.autoSetupMerge` and
//! `branch.autoSetupRebase`, Documentation/git-switch.adoc `--guess`,
//! Documentation/config/checkout.adoc `checkout.defaultRemote`).
//!
//! Creating a branch with tracking has two phases, like git:
//!
//! 1. [`plan`] before the ref is written: resolves the start point as a ref
//!    and decides whether (and how) the new branch tracks it. Refuses with
//!    git's messages when `--track` cannot be honoured (start point not a
//!    branch, requested upstream missing) or the start point is ambiguous.
//! 2. [`Plan::apply`] after the ref is written: writes `branch.<name>.remote`,
//!    `.merge` and `.rebase` and prints "branch 'x' set up to track ...".
//!    Ambiguous remotes are only detected here, so git leaves the new
//!    branch behind in that case, and so does this.
//!
//! `switch`/`checkout` additionally use [`guess_remote_branch`] (the
//! `--guess` DWIM) and [`derive_branch_name`] (`-t` without `-c`/`-b`).

use std::cell::Cell;
use std::path::{Path, PathBuf};

use bstr::ByteSlice;
use rgit_config::write as config_write;
use rgit_config::{ConfigSet, EditError, SetRequest, ValuePattern};
use rgit_options::{CallbackError, Opt};
use rgit_refs::files::FilesStore;
use rgit_refs::{RefStore, Target};
use rgit_remote::{BranchConfig, Remotes};
use rgit_repo::Setup;

use crate::Die;
use crate::messages::tracking as msg;
use crate::refs_support::Revs;

/// `-t`/`--track[=direct|inherit]`/`--no-track` as given on the command
/// line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Track {
    /// Neither option: `branch.autoSetupMerge` decides.
    #[default]
    Unspecified,
    /// `--no-track`.
    Never,
    /// `-t`, `--track`, `--track=direct`.
    Direct,
    /// `--track=inherit`.
    Inherit,
}

impl Track {
    /// The `-t, --[no-]track[=(direct|inherit)]` option storing into
    /// `slot`; the last of `--track`/`--no-track` wins.
    pub fn option(slot: &Cell<Track>) -> Opt<'_> {
        Opt::callback(
            't',
            "track",
            Some(msg::TRACK_ARGH),
            msg::HELP_TRACK,
            move |arg, neg| {
                let t = match (neg, arg) {
                    (true, _) => Track::Never,
                    (false, None | Some("direct")) => Track::Direct,
                    (false, Some("inherit")) => Track::Inherit,
                    (false, Some(_)) => {
                        return Err(CallbackError::Error(msg::TRACK_EXPECTS.to_owned()));
                    }
                };
                slot.set(t);
                Ok(())
            },
        )
        .literal_argh()
        .optional(None)
    }

    /// Whether the option was given at all (`switch`/`checkout`: `-t` and
    /// `--no-track` imply branch creation).
    #[allow(dead_code)] // for switch/checkout (#42, #49)
    pub fn is_given(self) -> bool {
        self != Track::Unspecified
    }
}

/// `branch.autoSetupMerge`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoMerge {
    False,
    True,
    Always,
    Inherit,
    Simple,
}

/// `branch.autoSetupRebase`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoRebase {
    Never,
    Local,
    Remote,
    Always,
}

/// `branch.autoSetupMerge` and `branch.autoSetupRebase`.
#[derive(Debug, Clone, Copy)]
pub struct AutoSetup {
    merge: AutoMerge,
    rebase: AutoRebase,
}

impl Default for AutoSetup {
    fn default() -> Self {
        AutoSetup {
            merge: AutoMerge::True,
            rebase: AutoRebase::Never,
        }
    }
}

impl AutoSetup {
    /// Read both settings. An invalid value ends the command like in git,
    /// which reads them for every `branch`, `switch` and `checkout`, so call
    /// this early (before the command does anything).
    pub fn from_config(config: &ConfigSet) -> Result<AutoSetup, Die> {
        let mut out = AutoSetup::default();
        for e in config.entries() {
            match e.name.as_slice() {
                b"branch.autosetupmerge" => {
                    out.merge = match e.value.as_deref().map(|v| v.as_bytes()) {
                        Some(b"always") => AutoMerge::Always,
                        Some(b"inherit") => AutoMerge::Inherit,
                        Some(b"simple") => AutoMerge::Simple,
                        _ => match e.bool() {
                            Ok(true) => AutoMerge::True,
                            Ok(false) => AutoMerge::False,
                            Err(err) => return Err(Die::Fatal(err.to_string())),
                        },
                    };
                }
                b"branch.autosetuprebase" => {
                    out.rebase = match e.value.as_deref().map(|v| v.as_bytes()) {
                        Some(b"never") => AutoRebase::Never,
                        Some(b"local") => AutoRebase::Local,
                        Some(b"remote") => AutoRebase::Remote,
                        Some(b"always") => AutoRebase::Always,
                        Some(_) => {
                            eprintln!("error: {}", msg::malformed_auto_setup_rebase());
                            return Err(Die::Fatal(crate::revisions::unable_to_parse(
                                "branch.autosetuprebase",
                                e,
                            )));
                        }
                        None => {
                            if let Err(err) = e.string() {
                                eprintln!("error: {err}");
                            }
                            return Err(Die::Fatal(crate::revisions::unable_to_parse(
                                "branch.autosetuprebase",
                                e,
                            )));
                        }
                    };
                }
                _ => {}
            }
        }
        Ok(out)
    }

    fn rebase(&self, local: bool) -> bool {
        match self.rebase {
            AutoRebase::Never => false,
            AutoRebase::Always => true,
            AutoRebase::Local => local,
            AutoRebase::Remote => !local,
        }
    }
}

/// What the tracking code needs of the repository.
pub struct Repo<'a> {
    pub setup: &'a Setup,
    pub config: &'a ConfigSet,
    pub store: &'a FilesStore,
    pub revs: &'a Revs,
}

impl Repo<'_> {
    fn config_path(&self) -> PathBuf {
        self.setup.common_dir.join("config")
    }

    fn remotes(&self) -> Result<Remotes, Die> {
        Remotes::from_config(self.config).map_err(|e| Die::Fatal(e.to_string()))
    }

    /// Remotes whose fetch refspecs map remote-tracking ref `full` back to
    /// a remote ref: `(remote, remote ref)` in configuration order.
    fn remotes_mapping(&self, full: &str) -> Result<Vec<(String, String)>, Die> {
        let remotes = self.remotes()?;
        let names: Vec<String> = remotes
            .names()
            .map(|n| n.to_str_lossy().into_owned())
            .collect();
        let mut out = Vec::new();
        for n in names {
            let Ok(r) = remotes.get(n.as_bytes(), Some(&self.setup.git_dir)) else {
                continue;
            };
            if let Some(src) = r.fetch.map_dst(full.as_bytes()) {
                out.push((n, src.to_str_lossy().into_owned()));
            }
        }
        Ok(out)
    }

    #[allow(dead_code)] // for switch/checkout (#42, #49)
    fn ref_exists(&self, full: &str) -> bool {
        match self.store.read_raw(full) {
            Ok(Some(Target::Object(_))) => true,
            Ok(Some(Target::Symbolic(t))) => self.revs.resolve(&t).is_some(),
            _ => false,
        }
    }
}

/// How a new branch will track its start point; made by [`plan`] (or
/// [`Plan::for_upstream`]) and carried out by [`Plan::apply`].
#[derive(Debug, Clone)]
pub struct Plan {
    kind: Kind,
}

#[derive(Debug, Clone)]
enum Kind {
    /// Track local branch `full` (`refs/heads/...`) with remote `.`.
    Local(String),
    /// Track remote-tracking ref `full` through the remote(s) mapping it.
    Remote(String),
    /// Copy the tracking configuration of `branch.<from>`.
    Inherit(String),
}

/// Resolve `start` (as given by the user, `HEAD` or the current branch name
/// when none was given) for a new branch `new_name`, and decide its
/// tracking from `track` and `auto`. `resolves` says whether `start` names
/// an object at all; the caller reports an unresolvable start point itself
/// (`branch`: "not a valid object name", `switch`: "invalid reference")
/// when this returns `Ok`.
///
/// Errors, as git: an ambiguous ref name; with `--track` (direct) a start
/// point that is not a branch or does not exist (with the
/// `advice.setUpstreamFailure` hint).
pub fn plan(
    repo: &Repo<'_>,
    new_name: &str,
    start: &str,
    resolves: bool,
    track: Track,
    auto: &AutoSetup,
) -> Result<Option<Plan>, Die> {
    let dwim = repo
        .revs
        .with_resolver(|r| r.dwim_ref(start))
        .map_err(|e| Die::Fatal(e.to_string()))?;
    if dwim.count > 1 {
        eprintln!("warning: {}", msg::refname_ambiguous(start));
        return Err(Die::Fatal(msg::ambiguous_object_name(start)));
    }
    let explicit = matches!(track, Track::Direct | Track::Inherit);
    let mode = match track {
        Track::Never => return Ok(None),
        Track::Direct => AutoMerge::Always,
        Track::Inherit => AutoMerge::Inherit,
        Track::Unspecified => auto.merge,
    };
    if mode == AutoMerge::False {
        return Ok(None);
    }
    let full = (dwim.count == 1 && dwim.oid.is_some()).then_some(dwim.full_name);
    let Some(full) = full else {
        if track == Track::Direct {
            if resolves {
                return Err(Die::Fatal(msg::not_a_branch(start)));
            }
            eprintln!("fatal: {}", msg::upstream_does_not_exist(start));
            // `--no-advice` reaches this advisor through
            // `rgit_messages::advice::disable_all`.
            rgit_messages::Advisor::new(repo.config, &rgit_config::SystemEnv::default(), false)
                .and_then(|a| {
                    a.advise(
                        rgit_messages::Advice::SetUpstreamFailure,
                        msg::HINT_SET_UPSTREAM_FAILURE,
                    )
                })
                .map_err(Die::Fatal)?;
            return Err(Die::Exit(128));
        }
        return Ok(None);
    };
    let local = full.starts_with("refs/heads/");
    let remote = full.starts_with("refs/remotes/");
    if mode == AutoMerge::Inherit {
        if !local && (!remote || repo.remotes_mapping(&full)?.is_empty()) {
            return Ok(None);
        }
        let from = full.strip_prefix("refs/heads/").unwrap_or(&full).to_owned();
        return Ok(Some(Plan {
            kind: Kind::Inherit(from),
        }));
    }
    let not_a_branch = || {
        if explicit {
            Err(Die::Fatal(msg::not_a_branch(start)))
        } else {
            Ok(None)
        }
    };
    if local {
        if mode != AutoMerge::Always {
            return Ok(None);
        }
        return Ok(Some(Plan {
            kind: Kind::Local(full),
        }));
    }
    if !remote {
        return not_a_branch();
    }
    let mapped = repo.remotes_mapping(&full)?;
    let Some((_, src)) = mapped.first() else {
        return not_a_branch();
    };
    if mode == AutoMerge::Simple {
        let short = src.strip_prefix("refs/heads/").unwrap_or(src);
        if short != new_name {
            return Ok(None);
        }
    }
    Ok(Some(Plan {
        kind: Kind::Remote(full),
    }))
}

/// One resolved upstream: remote name and the ref on that remote.
struct Upstream {
    remote: String,
    merges: Vec<String>,
    /// Shown as `<merge>` instead of `<remote>/<merge>`.
    local: bool,
}

impl Plan {
    /// Whether tracking would make `branch` its own upstream (`-u` warns
    /// and does nothing then).
    pub fn is_own_upstream(&self, branch: &str) -> bool {
        matches!(&self.kind, Kind::Local(full) if full.strip_prefix("refs/heads/") == Some(branch))
    }

    /// The remote and merge refs; `Ok(None)` with a warning printed when an
    /// inherited configuration is incomplete.
    fn resolve(&self, repo: &Repo<'_>) -> Result<Option<Upstream>, Die> {
        match &self.kind {
            Kind::Local(full) => Ok(Some(Upstream {
                remote: ".".to_owned(),
                merges: vec![full.clone()],
                local: true,
            })),
            Kind::Remote(full) => {
                let mapped = repo.remotes_mapping(full)?;
                if mapped.len() > 1 {
                    eprintln!("fatal: {}", msg::ambiguous_tracking(full));
                    let names: Vec<String> = mapped.into_iter().map(|(n, _)| n).collect();
                    eprint!("{}", msg::hint_ambiguous_tracking(full, &names));
                    return Err(Die::Exit(128));
                }
                let Some((remote, src)) = mapped.into_iter().next() else {
                    return Ok(None);
                };
                Ok(Some(Upstream {
                    remote,
                    merges: vec![src],
                    local: false,
                }))
            }
            Kind::Inherit(from) => {
                let b = BranchConfig::from_config(repo.config, from.as_bytes())
                    .map_err(|e| Die::Fatal(e.to_string()))?;
                let Some(remote) = b.remote else {
                    eprintln!("warning: {}", msg::inherit_no_remote(from));
                    return Ok(None);
                };
                if b.merge.is_empty() {
                    eprintln!("warning: {}", msg::inherit_no_merge(from));
                    return Ok(None);
                }
                Ok(Some(Upstream {
                    remote: remote.to_str_lossy().into_owned(),
                    merges: b
                        .merge
                        .iter()
                        .map(|m| m.to_str_lossy().into_owned())
                        .collect(),
                    local: false,
                }))
            }
        }
    }

    /// Write the tracking configuration of `branch` (which must exist by
    /// now) and, unless `quiet`, print "branch '<branch>' set up to track
    /// '<upstream>'." on standard output.
    pub fn apply(
        &self,
        repo: &Repo<'_>,
        branch: &str,
        auto: &AutoSetup,
        quiet: bool,
    ) -> Result<(), Die> {
        let Some(up) = self.resolve(repo)? else {
            return Ok(());
        };
        let rebase = auto.rebase(up.local);
        if rebase && up.merges.len() > 1 {
            return Err(Die::Fatal(msg::MULTIPLE_REFS_REBASE.to_owned()));
        }
        let path = repo.config_path();
        config_edit(
            &path,
            &format!("branch.{branch}.remote"),
            Some(&up.remote),
            false,
        )?;
        config_edit(&path, &format!("branch.{branch}.merge"), None, false)?;
        for m in &up.merges {
            config_edit(&path, &format!("branch.{branch}.merge"), Some(m), true)?;
        }
        if rebase {
            config_edit(
                &path,
                &format!("branch.{branch}.rebase"),
                Some("true"),
                false,
            )?;
        }
        if quiet {
            return Ok(());
        }
        let shown: Vec<String> = up
            .merges
            .iter()
            .map(|m| {
                let short = m.strip_prefix("refs/heads/").unwrap_or(m);
                if up.local {
                    short.to_owned()
                } else {
                    format!("{}/{short}", up.remote)
                }
            })
            .collect();
        if shown.len() == 1 {
            println!("{}", msg::set_up_to_track(branch, &shown[0], rebase));
        } else {
            println!("{}", msg::set_up_to_track_many(branch, &shown));
        }
        Ok(())
    }
}

/// Set (`add` false: replace a single value), add or unset (`value` None)
/// one variable. More than one existing value is only warned about.
fn config_edit(path: &Path, key: &str, value: Option<&str>, add: bool) -> Result<(), Die> {
    let pattern = if add {
        ValuePattern::Never
    } else {
        ValuePattern::Any
    };
    let req = SetRequest {
        key: key.as_bytes(),
        value: value.map(str::as_bytes),
        pattern: &pattern,
        all: false,
        comment: None,
    };
    match config_write::set_in_file(path, &Default::default(), &req) {
        Ok(()) => Ok(()),
        Err(e) => match e.edit_error() {
            Some(EditError::NothingToUnset) => Ok(()),
            Some(err @ EditError::MultipleValues { .. }) => {
                eprintln!("warning: {err}");
                Ok(())
            }
            _ => Err(Die::Fatal(e.to_string())),
        },
    }
}

/// Result of [`guess_remote_branch`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // for switch/checkout (#42, #49)
pub enum Guess {
    /// No remote has a matching remote-tracking branch.
    None,
    /// The remote-tracking ref to create the branch from, e.g.
    /// `refs/remotes/origin/topic`.
    Unique(String),
    /// Several remotes match and `checkout.defaultRemote` does not pick one;
    /// the remote names. Report with [`ambiguous_guess`].
    Ambiguous(Vec<String>),
}

/// `switch <name>`/`checkout <name>` when no branch `<name>` exists: the
/// remote-tracking branch for `refs/heads/<name>` in exactly one remote
/// (through the remotes' fetch refspecs), or in the remote named by
/// `checkout.defaultRemote` when several have one.
#[allow(dead_code)] // for switch/checkout (#42, #49)
pub fn guess_remote_branch(repo: &Repo<'_>, name: &str) -> Result<Guess, Die> {
    let remotes = repo.remotes()?;
    let names: Vec<String> = remotes
        .names()
        .map(|n| n.to_str_lossy().into_owned())
        .collect();
    let src = format!("refs/heads/{name}");
    let mut found: Vec<(String, String)> = Vec::new();
    for n in names {
        let Ok(r) = remotes.get(n.as_bytes(), Some(&repo.setup.git_dir)) else {
            continue;
        };
        let Some(dst) = r.fetch.map_src(src.as_bytes()) else {
            continue;
        };
        let dst = dst.to_str_lossy().into_owned();
        if repo.ref_exists(&dst) {
            found.push((n, dst));
        }
    }
    Ok(match found.len() {
        0 => Guess::None,
        1 => Guess::Unique(found.remove(0).1),
        _ => {
            let default = repo
                .config
                .string("checkout.defaultremote")
                .ok()
                .flatten()
                .map(|v| v.to_str_lossy().into_owned());
            match found.iter().position(|(n, _)| Some(n) == default.as_ref()) {
                Some(i) => Guess::Unique(found.remove(i).1),
                None => Guess::Ambiguous(found.into_iter().map(|(n, _)| n).collect()),
            }
        }
    })
}

/// Print the hint (unless `advice.checkoutAmbiguousRemoteBranchName` is
/// false) for [`Guess::Ambiguous`] and return the fatal error. `command` is
/// `switch` or `checkout`, used in the hint.
#[allow(dead_code)] // for switch/checkout (#42, #49)
pub fn ambiguous_guess(config: &ConfigSet, name: &str, remotes: &[String], command: &str) -> Die {
    if !matches!(
        config.bool("advice.checkoutambiguousremotebranchname"),
        Ok(Some(false))
    ) {
        eprint!(
            "{}",
            msg::hint_ambiguous_remote_branch(name, remotes, command)
        );
    }
    Die::Fatal(msg::matched_multiple_remotes(name, remotes.len()))
}

/// `switch -t <start>`/`checkout -t <start>` without a branch name: the
/// name after the remote part (`origin/hack`, `remotes/origin/hack` and
/// `refs/remotes/origin/hack` all give `hack`). `None` when there is no
/// slash or nothing after it; report that with [`missing_branch_name`].
#[allow(dead_code)] // for switch/checkout (#42, #49)
pub fn derive_branch_name(start: &str) -> Option<String> {
    let s = start.strip_prefix("refs/").unwrap_or(start);
    let s = s.strip_prefix("remotes/").unwrap_or(s);
    let (_, rest) = s.split_once('/')?;
    (!rest.is_empty()).then(|| rest.to_owned())
}

/// "fatal: missing branch name; try -c" (`create_option` `c` for switch,
/// `b` for checkout).
#[allow(dead_code)] // for switch/checkout (#42, #49)
pub fn missing_branch_name(create_option: &str) -> Die {
    Die::Fatal(msg::missing_branch_name(create_option))
}

/// The start point shown in messages and the reflog when none was given:
/// the current branch (also when unborn), else `HEAD`.
pub fn default_start(store: &FilesStore) -> String {
    match store.read_raw("HEAD") {
        Ok(Some(Target::Symbolic(t))) => t
            .strip_prefix("refs/heads/")
            .map_or_else(|| "HEAD".to_owned(), str::to_owned),
        _ => "HEAD".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive() {
        assert_eq!(derive_branch_name("origin/hack").as_deref(), Some("hack"));
        assert_eq!(
            derive_branch_name("refs/remotes/origin/a/b").as_deref(),
            Some("a/b")
        );
        assert_eq!(
            derive_branch_name("remotes/origin/hack").as_deref(),
            Some("hack")
        );
        assert_eq!(derive_branch_name("main"), None);
        assert_eq!(derive_branch_name("origin/"), None);
        assert_eq!(
            derive_branch_name("refs/heads/main").as_deref(),
            Some("main")
        );
    }
}
