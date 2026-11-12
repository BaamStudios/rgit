// SPDX-License-Identifier: MIT
//! Branch tracking: `branch.<name>.remote`, `.pushRemote`, `.merge`, the
//! upstream remote-tracking ref (`@{upstream}`) and the push destination's
//! remote-tracking ref (`@{push}`), following `git-config(1)` ("branch",
//! "push.default") and `gitrevisions(7)`; error texts checked against
//! `git rev-parse`.

use std::fmt;
use std::path::Path;

use bstr::{BStr, BString, ByteSlice};
use rgit_config::ConfigSet;

use crate::error::{ConfigError, string_value};
use crate::messages;
use crate::remote::{Remote, RemoteError, Remotes};

/// The tracking configuration of one branch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchConfig {
    /// Short branch name (`main`).
    pub name: BString,
    /// `branch.<name>.remote`.
    pub remote: Option<BString>,
    /// `branch.<name>.pushRemote`.
    pub push_remote: Option<BString>,
    /// `branch.<name>.merge`, in order.
    pub merge: Vec<BString>,
}

impl BranchConfig {
    /// Read the configuration of branch `name` (short name).
    pub fn from_config(config: &ConfigSet, name: &[u8]) -> Result<BranchConfig, ConfigError> {
        let key = |k: &str| {
            let mut s = BString::from("branch.");
            s.extend_from_slice(name);
            s.push(b'.');
            s.extend_from_slice(k.as_bytes());
            s
        };
        let last = |k: &str| -> Result<Option<BString>, ConfigError> {
            config
                .get(key(k))
                .map(|e| string_value(e).map(|v| v.to_owned()))
                .transpose()
        };
        let merge = config
            .get_all(key("merge"))
            .map(|e| string_value(e).map(|v| v.to_owned()))
            .collect::<Result<_, _>>()?;
        Ok(BranchConfig {
            name: name.into(),
            remote: last("remote")?,
            push_remote: last("pushremote")?,
            merge,
        })
    }

    /// The full ref name, `refs/heads/<name>`.
    pub fn full_name(&self) -> BString {
        let mut s = BString::from("refs/heads/");
        s.extend_from_slice(&self.name);
        s
    }
}

/// `push.default`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PushDefault {
    /// `nothing`.
    Nothing,
    /// `matching`.
    Matching,
    /// `simple` (the default).
    #[default]
    Simple,
    /// `upstream` (or the old `tracking`).
    Upstream,
    /// `current`.
    Current,
}

impl fmt::Display for PushDefault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PushDefault::Nothing => "nothing",
            PushDefault::Matching => "matching",
            PushDefault::Simple => "simple",
            PushDefault::Upstream => "upstream",
            PushDefault::Current => "current",
        })
    }
}

impl PushDefault {
    /// Read `push.default`.
    pub fn from_config(config: &ConfigSet) -> Result<PushDefault, ConfigError> {
        let Some(e) = config.get("push.default") else {
            return Ok(PushDefault::default());
        };
        let v = string_value(e)?;
        Ok(match v.as_bytes() {
            b"nothing" => PushDefault::Nothing,
            b"matching" => PushDefault::Matching,
            b"simple" => PushDefault::Simple,
            b"upstream" | b"tracking" => PushDefault::Upstream,
            b"current" => PushDefault::Current,
            _ => {
                return Err(ConfigError::PushDefault {
                    value: v.to_owned(),
                    origin: e.source.origin.clone(),
                    line: e.line,
                });
            }
        })
    }
}

/// Why `@{upstream}` or `@{push}` cannot be resolved. `Display` is git's
/// message (printed with `fatal: `).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TrackingError {
    /// Detached `HEAD`.
    #[error("{}", messages::HEAD_NOT_A_BRANCH)]
    NotOnBranch,
    /// No `branch.<name>.remote` or `branch.<name>.merge`.
    #[error("{}", messages::no_upstream(.0.as_bstr()))]
    NoUpstream(BString),
    /// The merge ref is not mapped by the remote's fetch refspecs.
    #[error("{}", messages::upstream_not_stored(.0.as_bstr()))]
    NotStored(BString),
    /// `push.default=nothing`.
    #[error("{}", messages::PUSH_NOTHING)]
    PushNothing,
    /// `simple` with an upstream of another name.
    #[error("{}", messages::cannot_resolve_push(.0))]
    CannotResolve(PushDefault),
    /// `remote.<name>.push` does not map the branch.
    #[error("{}", messages::push_refspecs_exclude(.remote.as_bstr(), .branch.as_bstr()))]
    PushRefspecsExclude {
        /// Remote name.
        remote: BString,
        /// Short branch name.
        branch: BString,
    },
    /// The push destination is not mapped by the remote's fetch refspecs.
    #[error("{}", messages::push_dst_untracked(.dst.as_bstr(), .remote.as_bstr()))]
    PushDestinationUntracked {
        /// The destination ref on the remote.
        dst: BString,
        /// Remote name.
        remote: BString,
    },
    /// The remote configuration is broken.
    #[error(transparent)]
    Remote(#[from] RemoteError),
}

/// The remote-tracking ref of `branch`'s upstream (`@{upstream}`). For the
/// remote `.` it is the merge ref itself.
pub fn upstream_ref(
    remotes: &Remotes,
    git_dir: Option<&Path>,
    branch: Option<&BranchConfig>,
) -> Result<BString, TrackingError> {
    let b = branch.ok_or(TrackingError::NotOnBranch)?;
    let (Some(remote), Some(merge)) = (&b.remote, b.merge.first()) else {
        return Err(TrackingError::NoUpstream(b.name.clone()));
    };
    if remote == "." {
        return Ok(merge.clone());
    }
    let r = remotes.get(remote, git_dir)?;
    r.fetch
        .map_src(merge)
        .ok_or_else(|| TrackingError::NotStored(merge.clone()))
}

/// The remote-tracking ref of where `git push` would push `branch`
/// (`@{push}`).
pub fn push_ref(
    remotes: &Remotes,
    git_dir: Option<&Path>,
    branch: Option<&BranchConfig>,
    push_default: PushDefault,
) -> Result<BString, TrackingError> {
    let b = branch.ok_or(TrackingError::NotOnBranch)?;
    let name = remotes.push_remote_name(
        b.push_remote.as_ref().map(|r| r.as_bstr()),
        b.remote.as_ref().map(|r| r.as_bstr()),
    );
    let remote = remotes.get(&name, git_dir)?;
    let full = b.full_name();
    if !remote.push.is_empty() {
        let dst = remote
            .push
            .map_src(&full)
            .ok_or_else(|| TrackingError::PushRefspecsExclude {
                remote: remote.name.clone(),
                branch: b.name.clone(),
            })?;
        return tracking_for(&remote, dst);
    }
    match push_default {
        PushDefault::Nothing => Err(TrackingError::PushNothing),
        PushDefault::Current | PushDefault::Matching => tracking_for(&remote, full),
        PushDefault::Upstream => upstream_ref(remotes, git_dir, Some(b)),
        PushDefault::Simple => {
            let up = upstream_ref(remotes, git_dir, Some(b))?;
            let cur = tracking_for(&remote, full)?;
            if up != cur {
                return Err(TrackingError::CannotResolve(PushDefault::Simple));
            }
            Ok(cur)
        }
    }
}

fn tracking_for(remote: &Remote, dst: BString) -> Result<BString, TrackingError> {
    remote
        .fetch
        .map_src(&dst)
        .ok_or_else(|| TrackingError::PushDestinationUntracked {
            dst,
            remote: remote.name.clone(),
        })
}

/// Short form of a branch ref for messages (`refs/heads/x` → `x`).
pub fn short_branch(full: &[u8]) -> &BStr {
    full.strip_prefix(b"refs/heads/").unwrap_or(full).as_bstr()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgit_config::{Origin, Scope, parse_bytes};

    struct Setup {
        cfg: ConfigSet,
        remotes: Remotes,
    }

    fn setup(text: &str) -> Setup {
        let cfg = parse_bytes(
            text.as_bytes(),
            Origin::File(".git/config".into()),
            Scope::Local,
        )
        .unwrap();
        let remotes = Remotes::from_config(&cfg).unwrap();
        Setup { cfg, remotes }
    }

    fn up(s: &Setup) -> String {
        let b = BranchConfig::from_config(&s.cfg, b"main").unwrap();
        upstream_ref(&s.remotes, None, Some(&b)).map_or_else(|e| e.to_string(), |r| r.to_string())
    }

    fn push(s: &Setup) -> String {
        let b = BranchConfig::from_config(&s.cfg, b"main").unwrap();
        let pd = match PushDefault::from_config(&s.cfg) {
            Ok(p) => p,
            Err(e) => return e.to_string(),
        };
        push_ref(&s.remotes, None, Some(&b), pd).map_or_else(|e| e.to_string(), |r| r.to_string())
    }

    const ORIGIN: &str =
        "[remote \"origin\"]\n url = /x\n fetch = +refs/heads/*:refs/remotes/origin/*\n";

    #[test]
    fn upstream() {
        assert_eq!(up(&setup("")), "no upstream configured for branch 'main'");
        let s = setup(&format!("{ORIGIN}[branch \"main\"]\n remote = origin\n"));
        assert_eq!(up(&s), "no upstream configured for branch 'main'");
        let s = setup(&format!(
            "{ORIGIN}[branch \"main\"]\n remote = origin\n merge = refs/heads/other\n"
        ));
        assert_eq!(up(&s), "refs/remotes/origin/other");
        let s = setup("[branch \"main\"]\n remote = .\n merge = refs/heads/x\n");
        assert_eq!(up(&s), "refs/heads/x");
        let s = setup("[branch \"main\"]\n remote = nosuch\n merge = refs/heads/main\n");
        assert_eq!(
            up(&s),
            "upstream branch 'refs/heads/main' not stored as a remote-tracking branch"
        );
        let err = upstream_ref(&s.remotes, None, None).unwrap_err();
        assert_eq!(err.to_string(), "HEAD does not point to a branch");
    }

    #[test]
    fn push_destination() {
        assert_eq!(
            push(&setup(ORIGIN)),
            "no upstream configured for branch 'main'"
        );
        let tracking =
            format!("{ORIGIN}[branch \"main\"]\n remote = origin\n merge = refs/heads/other\n");
        assert_eq!(
            push(&setup(&tracking)),
            "cannot resolve 'simple' push to a single destination"
        );
        let with = |extra: &str| push(&setup(&format!("{tracking}{extra}")));
        assert_eq!(
            with("[push]\n default = nothing\n"),
            "push has no destination (push.default is 'nothing')"
        );
        assert_eq!(
            with("[push]\n default = upstream\n"),
            "refs/remotes/origin/other"
        );
        assert_eq!(
            with("[push]\n default = current\n"),
            "refs/remotes/origin/main"
        );
        assert_eq!(
            with("[push]\n default = matching\n"),
            "refs/remotes/origin/main"
        );
        assert_eq!(
            with("[push]\n default = bogus\n"),
            "bad config variable 'push.default' in file '.git/config' at line 8"
        );
        assert_eq!(
            with("[remote \"origin\"]\n push = refs/heads/other:refs/heads/x\n"),
            "push refspecs for 'origin' do not include 'main'"
        );
        assert_eq!(
            with("[remote \"origin\"]\n push = refs/heads/main\n"),
            "push refspecs for 'origin' do not include 'main'"
        );
        assert_eq!(
            with(
                "[remote \"origin\"]\n push = refs/heads/main:refs/heads/x\n fetch = refs/heads/main:refs/remotes/origin/main\n"
            ),
            "refs/remotes/origin/x"
        );
        assert_eq!(
            with(
                "[remote \"pub\"]\n url = /y\n[branch \"main\"]\n pushRemote = pub\n[push]\n default = current\n"
            ),
            "push destination 'refs/heads/main' on remote 'pub' has no local tracking branch"
        );
        assert_eq!(
            with(
                "[remote \"pub\"]\n url = /y\n fetch = +refs/heads/*:refs/remotes/pub/*\n push = refs/heads/*:refs/heads/q/*\n[remote]\n pushDefault = pub\n"
            ),
            "refs/remotes/pub/q/main"
        );
    }
}
