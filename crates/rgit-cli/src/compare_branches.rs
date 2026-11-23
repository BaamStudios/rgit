// SPDX-License-Identifier: MIT
//! `status.compareBranches` (Documentation/config/status.adoc): which
//! comparisons of the current branch the long status, `commit` and
//! `checkout` show, and in which order: with its upstream (`@{upstream}`,
//! the default) and with where `git push` would send it (`@{push}`).
//!
//! The upstream comparison is computed by each command as before; this
//! module adds the push comparison. A push ref that is missing or the same
//! as the upstream is not shown. When a push comparison is shown, it
//! carries the "git push" and "--ahead-behind" advice instead of the
//! upstream one; falling behind it suggests `git pull <remote> <branch>`
//! when the ref is the remote's conventional remote-tracking ref.

use rgit_hash::ObjectId;

use crate::messages::status as msg;
use crate::repository::Repo;

/// One entry of `status.compareBranches`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spec {
    Upstream,
    Push,
}

/// The configured comparisons in order, duplicates dropped; entries other
/// than `@{upstream}` and `@{push}` are warned about.
fn specs(repo: &Repo) -> Vec<Spec> {
    let Ok(Some(value)) = repo.config.string("status.comparebranches") else {
        return vec![Spec::Upstream];
    };
    let value = String::from_utf8_lossy(value).into_owned();
    let mut out = Vec::new();
    for word in value.split_ascii_whitespace() {
        let spec = match word {
            "@{upstream}" => Spec::Upstream,
            "@{push}" => Spec::Push,
            _ => {
                eprintln!("warning: {}", msg::compare_branches_ignored(word));
                continue;
            }
        };
        if !out.contains(&spec) {
            out.push(spec);
        }
    }
    out
}

/// How the branch relates to a ref it is compared with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Ahead and behind counts.
    Counts(usize, usize),
    /// Different commits, not counted (`--no-ahead-behind`).
    Different,
}

/// The comparison with the push destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Push {
    /// Short name of the remote-tracking ref (`origin/topic`).
    pub name: String,
    pub state: State,
    /// `<remote> <branch>` for the "git pull" advice, when the ref is the
    /// remote's conventional remote-tracking ref.
    pub pull: Option<String>,
}

/// A comparison to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// The upstream comparison (when the branch has an upstream).
    Upstream,
    Push(Push),
    /// A push destination that is not shown (its remote-tracking ref does
    /// not exist, or `@{push}` is not asked for), but still takes the push
    /// advice from the upstream.
    PushMissing,
}

/// The comparisons of `branch` at `head` to show, in order;
/// `upstream_full` is the full name of its upstream ref (to suppress an
/// identical push comparison), `count` whether to count commits.
pub fn plan(
    repo: &Repo,
    branch: &str,
    head: Option<ObjectId>,
    upstream_full: Option<&str>,
    count: bool,
) -> Vec<Item> {
    let specs = specs(repo);
    let mut out = Vec::new();
    for spec in specs {
        let item = match spec {
            Spec::Upstream => Some(Item::Upstream),
            Spec::Push => push(repo, branch, head, upstream_full, count),
        };
        // `@{push}` naming the upstream shows the upstream comparison.
        if let Some(item) = item.filter(|i| !out.contains(i)) {
            out.push(item);
        }
    }
    // A push destination apart from the upstream takes the push advice
    // from the upstream even when it is not shown.
    if !has_push(&out)
        && matches!(
            push(repo, branch, head, upstream_full, false),
            Some(Item::Push(_) | Item::PushMissing)
        )
    {
        out.push(Item::PushMissing);
    }
    out
}

/// Whether `items` has a push destination apart from the upstream.
pub fn has_push(items: &[Item]) -> bool {
    items
        .iter()
        .any(|i| matches!(i, Item::Push(_) | Item::PushMissing))
}

fn push(
    repo: &Repo,
    branch: &str,
    head: Option<ObjectId>,
    upstream_full: Option<&str>,
    count: bool,
) -> Option<Item> {
    let revs = crate::refs_support::Revs::with_config(&repo.setup, repo.config.clone());
    let target = revs.with_resolver(|r| {
        let t = r.push_target(branch);
        let _ = r.take_diagnostics();
        t
    })?;
    if upstream_full == Some(target.tracking.as_str()) {
        return Some(Item::Upstream);
    }
    let (Some(head), Some(id)) = (head, repo.refs.resolve(&target.tracking)) else {
        return Some(Item::PushMissing);
    };
    let state = if id == head {
        State::Counts(0, 0)
    } else if !count {
        State::Different
    } else {
        let (a, b) = crate::wt_status::ahead_behind(repo, head, id);
        State::Counts(a, b)
    };
    let conventional = target
        .tracking
        .strip_prefix("refs/remotes/")
        .and_then(|rest| rest.strip_prefix(target.remote.as_str()))
        .is_some_and(|rest| rest.starts_with('/'));
    let pull = conventional.then(|| {
        let dst = target
            .dst
            .strip_prefix("refs/heads/")
            .unwrap_or(&target.dst);
        format!("{} {dst}", target.remote)
    });
    Some(Item::Push(Push {
        name: short_name(&target.tracking),
        state,
        pull,
    }))
}

fn short_name(full: &str) -> String {
    ["refs/heads/", "refs/remotes/", "refs/"]
        .iter()
        .find_map(|p| full.strip_prefix(p))
        .unwrap_or(full)
        .to_string()
}

/// The lines of the push comparison: `(text, is_advice)`; advice lines
/// are the bare text without the `  (` `)` frame.
pub fn push_lines(p: &Push) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    match p.state {
        State::Counts(0, 0) => out.push((msg::up_to_date(&p.name), false)),
        State::Counts(a, 0) => {
            out.push((msg::ahead(&p.name, a), false));
            out.push((msg::HINT_PUSH.to_owned(), true));
        }
        State::Counts(0, b) => {
            out.push((msg::behind(&p.name, b), false));
            if let Some(pull) = &p.pull {
                out.push((msg::hint_pull_from(pull), true));
            }
        }
        State::Counts(a, b) => {
            for l in msg::diverged(&p.name, a, b).split('\n') {
                out.push((l.to_owned(), false));
            }
        }
        State::Different => {
            out.push((msg::different(&p.name), false));
            out.push((msg::HINT_AHEAD_BEHIND.to_owned(), true));
        }
    }
    out
}
