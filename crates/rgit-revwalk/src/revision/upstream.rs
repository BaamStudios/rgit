// SPDX-License-Identifier: MIT
//! `<branch>@{upstream}` and `<branch>@{push}`: the remote-tracking branch
//! a branch builds on, and the one `git push` would update.

use super::refname::{Tracking, parse_previous};
use super::{Error, Resolver, messages};

/// `push.default`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PushDefault {
    /// `nothing`
    Nothing,
    /// `matching`
    Matching,
    /// `simple` (the default)
    #[default]
    Simple,
    /// `upstream` (also `tracking`)
    Upstream,
    /// `current`
    Current,
}

impl PushDefault {
    /// Parse a `push.default` value.
    pub fn from_config(value: &str) -> Option<PushDefault> {
        Some(match value {
            "nothing" => PushDefault::Nothing,
            "matching" => PushDefault::Matching,
            "simple" => PushDefault::Simple,
            "upstream" | "tracking" => PushDefault::Upstream,
            "current" => PushDefault::Current,
            _ => return None,
        })
    }
}

/// A refspec `[+]<src>:<dst>` with at most one `*` per side.
struct Refspec<'a> {
    src: &'a str,
    dst: &'a str,
}

fn parse_refspec(spec: &str) -> Option<Refspec<'_>> {
    let spec = spec.strip_prefix('+').unwrap_or(spec);
    if spec.starts_with('^') {
        return None;
    }
    let (src, dst) = spec.split_once(':')?;
    Some(Refspec { src, dst })
}

/// Map `name` from the source side to the destination side of `spec`.
fn map_refspec(spec: &Refspec<'_>, name: &str) -> Option<String> {
    match (spec.src.split_once('*'), spec.dst.split_once('*')) {
        (Some((sp, ss)), Some((dp, ds))) => {
            let mid = name.strip_prefix(sp)?.strip_suffix(ss)?;
            Some(format!("{dp}{mid}{ds}"))
        }
        (None, None) if spec.src == name => Some(spec.dst.to_string()),
        _ => None,
    }
}

impl Resolver<'_> {
    fn config(&self, key: &str) -> Option<String> {
        self.repo.config.get(key)
    }

    /// The branch an `@{upstream}` / `@{push}` suffix applies to: the
    /// current branch for an empty name or `HEAD`.
    fn tracking_branch_name(&self, branch: &str) -> Result<String, Error> {
        if branch.is_empty() || branch == "HEAD" {
            let head = self.repo.refs.resolve("HEAD")?;
            return match head
                .as_ref()
                .and_then(|v| v.name.strip_prefix("refs/heads/"))
            {
                Some(b) => Ok(b.to_string()),
                None => Err(Error::Fatal(messages::HEAD_NOT_BRANCH.to_string())),
            };
        }
        if let Some(n) = parse_previous(branch) {
            return match self.previous_branch(n)? {
                Some(b) => Ok(b),
                None => Err(Error::NotFound),
            };
        }
        Ok(branch.to_string())
    }

    /// Full name of the upstream or push tracking ref of `branch`.
    pub(crate) fn branch_tracking(&self, branch: &str, kind: Tracking) -> Result<String, Error> {
        let name = self.tracking_branch_name(branch)?;
        let full = format!("refs/heads/{name}");
        if self.repo.refs.resolve(&full)?.is_none() {
            return Err(Error::Fatal(messages::no_such_branch(&name)));
        }
        match kind {
            Tracking::Upstream => self.upstream_of(&name),
            Tracking::Push => self.push_of(&name),
        }
    }

    /// `branch.<name>.remote` + `branch.<name>.merge`, mapped through the
    /// remote's fetch refspecs.
    fn upstream_of(&self, branch: &str) -> Result<String, Error> {
        let remote = self.config(&format!("branch.{branch}.remote"));
        let merge = self
            .repo
            .config
            .get_all(&format!("branch.{branch}.merge"))
            .into_iter()
            .next();
        let (Some(remote), Some(merge)) = (remote, merge) else {
            return Err(Error::Fatal(messages::no_upstream(branch)));
        };
        if remote == "." {
            return Ok(merge);
        }
        match self.fetch_map(&remote, &merge) {
            Some(t) => Ok(t),
            None => Err(Error::Fatal(messages::upstream_not_tracked(&merge))),
        }
    }

    /// The remote-tracking ref for `name` on `remote`.
    fn fetch_map(&self, remote: &str, name: &str) -> Option<String> {
        if remote == "." {
            return None;
        }
        let specs = self.repo.config.get_all(&format!("remote.{remote}.fetch"));
        specs
            .iter()
            .filter_map(|s| parse_refspec(s))
            .find_map(|s| map_refspec(&s, name))
    }

    fn push_remote(&self, branch: &str) -> String {
        self.config(&format!("branch.{branch}.pushremote"))
            .or_else(|| self.config("remote.pushdefault"))
            .or_else(|| self.config(&format!("branch.{branch}.remote")))
            .unwrap_or_else(|| "origin".to_string())
    }

    /// The tracking ref of `refs/heads/<branch>` pushed to `remote` under
    /// the same name.
    fn tracking_for_push(&self, remote: &str, branch: &str) -> Result<String, Error> {
        let dst = format!("refs/heads/{branch}");
        self.fetch_map(remote, &dst)
            .ok_or_else(|| Error::Fatal(messages::push_no_tracking(&dst, remote)))
    }

    fn push_of(&self, branch: &str) -> Result<String, Error> {
        let remote = self.push_remote(branch);
        let push_specs = self.repo.config.get_all(&format!("remote.{remote}.push"));
        if !push_specs.is_empty() {
            let src = format!("refs/heads/{branch}");
            let dst = push_specs
                .iter()
                .filter_map(|s| parse_refspec(s))
                .find_map(|s| map_refspec(&s, &src))
                .ok_or_else(|| Error::Fatal(messages::push_refspecs_exclude(&remote, branch)))?;
            return self
                .fetch_map(&remote, &dst)
                .ok_or_else(|| Error::Fatal(messages::push_no_tracking(&dst, &remote)));
        }
        match self.opts.push_default {
            PushDefault::Nothing => Err(Error::Fatal(messages::PUSH_NOTHING.to_string())),
            PushDefault::Matching | PushDefault::Current => self.tracking_for_push(&remote, branch),
            PushDefault::Upstream => self.upstream_of(branch),
            PushDefault::Simple => {
                let up = self.upstream_of(branch)?;
                let cur = self.tracking_for_push(&remote, branch)?;
                if up != cur {
                    return Err(Error::Fatal(messages::PUSH_SIMPLE_AMBIGUOUS.to_string()));
                }
                Ok(cur)
            }
        }
    }
}
