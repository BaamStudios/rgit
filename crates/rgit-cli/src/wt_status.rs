// SPDX-License-Identifier: MIT
//! The long status format as `commit` uses it: printed when there is
//! nothing to commit, and as comment lines in the message template
//! (Documentation/git-status.adoc, "OUTPUT"; layout and hints checked
//! against the `git` binary). `git status` itself still prints only the
//! porcelain formats (#40).

use std::collections::{BTreeMap, HashSet, VecDeque};

use rgit_hash::ObjectId;
use rgit_index::Index;
use rgit_object::Object;

use crate::Die;
use crate::commands::status::{Change, Untracked, check_worktree};
use crate::messages::wt_status as msg;
use crate::quote::{relative_to, write_quoted};
use crate::repository::Repo;
use crate::tree_changes::{self, Flat, Pair, Side};

/// A change between the index and the work tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WtChange {
    Modified,
    Deleted,
    TypeChanged,
    /// An intent-to-add entry.
    New,
}

/// What the status shows.
#[derive(Debug, Default)]
pub struct Collected {
    /// Base tree → index.
    pub staged: Vec<Pair>,
    /// Index → work tree, in index order.
    pub unstaged: Vec<(WtChange, Vec<u8>)>,
    /// Untracked paths (directories end in `/`).
    pub untracked: Vec<Vec<u8>>,
}

/// The stage-0 entries of `index` (without intent-to-add entries).
pub fn index_flat(index: &Index) -> Flat {
    let mut out = Flat::new();
    for e in index.entries() {
        if e.stage().get() == 0 && !e.flags.intent_to_add {
            out.insert(
                e.path().to_vec(),
                Side {
                    mode: e.mode.0,
                    oid: e.id,
                },
            );
        }
    }
    out
}

/// Compare `base` (the parent's tree), `index` and the work tree.
pub fn collect(
    repo: &Repo,
    base: &Flat,
    index: &Index,
    index_mtime: Option<rgit_index::Time>,
    untracked: Untracked,
    renames: bool,
) -> Result<Collected, Die> {
    let staged = tree_changes::diff(repo, base, &index_flat(index), renames);
    let work_tree = repo.work_tree()?;
    let checker = repo.checker(index_mtime)?;
    let mut unstaged = Vec::new();
    let mut tracked: BTreeMap<Vec<u8>, Change> = BTreeMap::new();
    for e in index.entries() {
        let c = tracked.entry(e.path().to_vec()).or_default();
        if e.stage().get() != 0 {
            c.stages[usize::from(e.stage().get()) - 1] = Some((e.mode.0, e.id));
            continue;
        }
        c.index = Some((e.mode.0, e.id, e.flags.intent_to_add));
        check_worktree(repo, &checker, &work_tree, e, c);
        let kind = match c.wt {
            b'M' => WtChange::Modified,
            b'D' => WtChange::Deleted,
            b'T' => WtChange::TypeChanged,
            b'A' => WtChange::New,
            _ => continue,
        };
        unstaged.push((kind, e.path().to_vec()));
    }
    let (found, _) = crate::commands::status::untracked_and_ignored(
        repo,
        &work_tree,
        index,
        None,
        untracked,
        crate::untracked::IgnoredMode::No,
    )?;
    Ok(Collected {
        staged,
        unstaged,
        untracked: found,
    })
}

/// Where `HEAD` is.
#[derive(Debug, Clone)]
pub enum HeadState {
    /// On a branch (short name).
    Branch(String),
    /// Detached; the name last checked out and whether `HEAD` still
    /// points there.
    Detached(Option<(String, bool)>),
}

/// The upstream of the current branch.
#[derive(Debug, Clone)]
pub struct Tracking {
    /// Short name (`origin/main`).
    pub name: String,
    /// Commits ahead and behind; `None` when the upstream ref is gone.
    pub counts: Option<(usize, usize)>,
}

/// The header of the status.
#[derive(Debug, Clone)]
pub struct Header {
    pub head: HeadState,
    pub tracking: Option<Tracking>,
    /// The branch has no commit yet.
    pub initial: bool,
    /// A merge is being concluded (`MERGE_HEAD`).
    pub merging: bool,
    /// `commit --amend`: the changes are against `HEAD^1`.
    pub amend: bool,
}

fn parents(repo: &Repo, id: &ObjectId) -> Vec<ObjectId> {
    match repo.read_object(id) {
        Some(Object::Commit(c)) => c.parents().unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn ancestors(repo: &Repo, start: ObjectId) -> HashSet<ObjectId> {
    let mut seen = HashSet::new();
    let mut queue = VecDeque::from([start]);
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id) {
            continue;
        }
        queue.extend(parents(repo, &id));
    }
    seen
}

/// Commits reachable from `a` but not `b`, and the reverse.
fn ahead_behind(repo: &Repo, a: ObjectId, b: ObjectId) -> (usize, usize) {
    if a == b {
        return (0, 0);
    }
    let sa = ancestors(repo, a);
    let sb = ancestors(repo, b);
    (sa.difference(&sb).count(), sb.difference(&sa).count())
}

/// The status header for the current `HEAD` (`head` is its commit).
pub fn header(repo: &Repo, head: Option<ObjectId>) -> Header {
    let target = repo.refs.head_target();
    let initial = head.is_none();
    let Some(target) = target else {
        return Header {
            head: HeadState::Detached(detached_name(repo, head)),
            tracking: None,
            initial,
            merging: false,
            amend: false,
        };
    };
    let branch = target
        .strip_prefix("refs/heads/")
        .unwrap_or(&target)
        .to_string();
    let tracking = tracking(repo, &branch, head);
    Header {
        head: HeadState::Branch(branch),
        tracking,
        initial,
        merging: false,
        amend: false,
    }
}

pub(crate) fn tracking(repo: &Repo, branch: &str, head: Option<ObjectId>) -> Option<Tracking> {
    let get = |k: String| repo.config.string(k).ok().flatten().map(|s| s.to_string());
    let remote = get(format!("branch.{branch}.remote"))?;
    let merge = get(format!("branch.{branch}.merge"))?;
    let full = if remote == "." {
        merge.clone()
    } else {
        // The remote-tracking ref the fetch refspecs map `merge` to; none
        // means the branch has no upstream to compare with.
        repo.config
            .get_all(format!("remote.{remote}.fetch"))
            .filter_map(|e| e.value.as_ref())
            .find_map(|v| map_refspec(&String::from_utf8_lossy(v), &merge))?
    };
    let name = ["refs/heads/", "refs/remotes/", "refs/"]
        .iter()
        .find_map(|p| full.strip_prefix(p))
        .unwrap_or(&full)
        .to_string();
    // A local upstream may be configured by its short name
    // (`branch.<name>.merge = main`); it is looked up like a ref name.
    let up = if remote == "." && !full.starts_with("refs/") {
        [
            full.clone(),
            format!("refs/{full}"),
            format!("refs/tags/{full}"),
            format!("refs/heads/{full}"),
            format!("refs/remotes/{full}"),
            format!("refs/remotes/{full}/HEAD"),
        ]
        .iter()
        .find_map(|f| repo.refs.resolve(f))
    } else {
        repo.refs.resolve(&full)
    };
    let counts = match (up, head) {
        (None, _) => None,
        (Some(u), Some(h)) => Some(ahead_behind(repo, h, u)),
        (Some(u), None) => Some((0, ancestors(repo, u).len())),
    };
    Some(Tracking { name, counts })
}

/// The destination of the fetch refspec `spec` (`[+]src:dst`, at most one
/// `*` per side) for the source ref `name`.
fn map_refspec(spec: &str, name: &str) -> Option<String> {
    let spec = spec.strip_prefix('+').unwrap_or(spec);
    let (src, dst) = spec.split_once(':')?;
    match (src.split_once('*'), dst.split_once('*')) {
        (Some((sp, ss)), Some((dp, ds))) => {
            let mid = name.strip_prefix(sp)?.strip_suffix(ss)?;
            Some(format!("{dp}{mid}{ds}"))
        }
        (None, None) if src == name && !dst.is_empty() => Some(dst.to_owned()),
        _ => None,
    }
}

/// The name of the last checkout in the `HEAD` reflog and whether `HEAD`
/// still points at it.
fn detached_name(repo: &Repo, head: Option<ObjectId>) -> Option<(String, bool)> {
    let entries = repo.refs.reflog_entries("HEAD")?;
    for e in entries.iter().rev() {
        let Some(rest) = e.message.strip_prefix(b"checkout: moving from ") else {
            continue;
        };
        let rest = String::from_utf8_lossy(rest);
        let to = rest.rsplit(" to ").next()?.to_string();
        // A ref is shown by its full name, tags and remote-tracking
        // branches without their prefix (observed with the git binary);
        // anything else as the abbreviated commit it named.
        let full = [
            to.clone(),
            format!("refs/{to}"),
            format!("refs/tags/{to}"),
            format!("refs/heads/{to}"),
            format!("refs/remotes/{to}"),
            format!("refs/remotes/{to}/HEAD"),
        ]
        .into_iter()
        .find(|f| f.starts_with("refs/") && repo.refs.resolve(f).is_some());
        let name = if let Some(full) = full {
            ["refs/tags/", "refs/remotes/"]
                .iter()
                .find_map(|p| full.strip_prefix(p))
                .map_or(full.clone(), str::to_owned)
        } else {
            repo.abbrev(&e.new, repo.default_abbrev())
        };
        return Some((name, head == Some(e.new)));
    }
    None
}

/// How to render the status.
pub struct Render<'a> {
    /// Show the `(use "git ...")` hints (`advice.statusHints`).
    pub hints: bool,
    /// The hints after the upstream line (`advice.statusHints`; also in
    /// the template).
    pub branch_hints: bool,
    /// Prefix every line with this comment string (message template).
    pub comment: Option<&'a str>,
    /// The current directory relative to the top (empty or ending in `/`).
    pub prefix: &'a [u8],
    /// `core.quotePath`.
    pub quote_high: bool,
    /// The last line ("nothing to commit, ..."), if any.
    pub last: Option<String>,
    /// Say that untracked files were not looked for (`-uno`).
    pub untracked_not_listed: bool,
}

impl Render<'_> {
    fn path(&self, p: &[u8]) -> Vec<u8> {
        let rel = relative_to(p, self.prefix);
        let mut out = Vec::new();
        write_quoted(&mut out, &rel, self.quote_high);
        out
    }
}

/// The final line when there is nothing to commit.
pub fn nothing_line(
    c: &Collected,
    header: &Header,
    untracked: Untracked,
    amend: bool,
    hints: bool,
) -> String {
    let s = |hint: &str, plain: &str| {
        if hints {
            hint.to_owned()
        } else {
            plain.to_owned()
        }
    };
    if amend {
        msg::NO_CHANGES.to_owned()
    } else if !c.unstaged.is_empty() {
        s(msg::NOTHING_ADDED_HINT, msg::NOTHING_ADDED)
    } else if !c.untracked.is_empty() {
        s(msg::UNTRACKED_ONLY_HINT, msg::UNTRACKED_ONLY)
    } else if header.initial {
        s(msg::NOTHING_INITIAL_HINT, msg::NOTHING)
    } else if untracked == Untracked::No {
        s(msg::NOTHING_NO_UNTRACKED_HINT, msg::NOTHING)
    } else {
        msg::NOTHING_CLEAN.to_owned()
    }
}

/// Render the long status.
pub fn render(c: &Collected, header: &Header, r: &Render<'_>) -> Vec<u8> {
    let mut lines: Vec<Vec<u8>> = Vec::new();
    let mut line = |s: &str| lines.push(s.as_bytes().to_vec());
    match &header.head {
        HeadState::Branch(b) => line(&msg::on_branch(b)),
        HeadState::Detached(Some((name, true))) => line(&msg::detached_at(name)),
        HeadState::Detached(Some((name, false))) => line(&msg::detached_from(name)),
        HeadState::Detached(None) => line(msg::NOT_ON_BRANCH),
    }
    if let Some(t) = &header.tracking {
        match t.counts {
            None => {
                line(&msg::gone(&t.name));
                if r.branch_hints {
                    line(msg::HINT_UNSET_UPSTREAM);
                }
            }
            Some((0, 0)) => line(&msg::up_to_date(&t.name)),
            Some((a, 0)) => {
                line(&msg::ahead(&t.name, a));
                if r.branch_hints {
                    line(msg::HINT_PUSH);
                }
            }
            Some((0, b)) => {
                line(&msg::behind(&t.name, b));
                if r.branch_hints {
                    line(msg::HINT_PULL_FF);
                }
            }
            Some((a, b)) => {
                for l in msg::diverged(&t.name, a, b) {
                    line(&l);
                }
            }
        }
        line("");
    }
    if header.merging {
        line(msg::STILL_MERGING);
        if r.hints {
            line(msg::HINT_CONCLUDE_MERGE);
        }
        line("");
    }
    if header.initial {
        line("");
        // `commit` says "Initial commit" where `git status` says "No
        // commits yet".
        line(msg::INITIAL_COMMIT);
        line("");
    }
    let entry = |lines: &mut Vec<Vec<u8>>, label: &str, path: Vec<u8>| {
        let mut l = format!("\t{label:<12}").into_bytes();
        l.extend_from_slice(&path);
        lines.push(l);
    };
    if !c.staged.is_empty() {
        lines.push(msg::STAGED_HEADER.into());
        if r.hints {
            lines.push(
                if header.initial {
                    msg::HINT_UNSTAGE_INITIAL
                } else if header.amend {
                    msg::HINT_UNSTAGE_AMEND
                } else {
                    msg::HINT_UNSTAGE
                }
                .into(),
            );
        }
        for p in &c.staged {
            if let Some(old) = &p.old_path {
                let mut text = r.path(old);
                text.extend_from_slice(b" -> ");
                text.extend_from_slice(&r.path(&p.path));
                entry(&mut lines, msg::LABEL_RENAMED, text);
                continue;
            }
            let label = match (p.old, p.new) {
                (None, _) => msg::LABEL_NEW,
                (_, None) => msg::LABEL_DELETED,
                _ if p.type_changed() => msg::LABEL_TYPECHANGE,
                _ => msg::LABEL_MODIFIED,
            };
            entry(&mut lines, label, r.path(&p.path));
        }
        lines.push(Vec::new());
    }
    if !c.unstaged.is_empty() {
        lines.push(msg::UNSTAGED_HEADER.into());
        if r.hints {
            let any_deleted = c.unstaged.iter().any(|(k, _)| *k == WtChange::Deleted);
            lines.push(
                if any_deleted {
                    msg::HINT_ADD_RM
                } else {
                    msg::HINT_ADD
                }
                .into(),
            );
            lines.push(msg::HINT_RESTORE.into());
        }
        for (kind, path) in &c.unstaged {
            let label = match kind {
                WtChange::Modified => msg::LABEL_MODIFIED,
                WtChange::Deleted => msg::LABEL_DELETED,
                WtChange::TypeChanged => msg::LABEL_TYPECHANGE,
                WtChange::New => msg::LABEL_NEW,
            };
            entry(&mut lines, label, r.path(path));
        }
        lines.push(Vec::new());
    }
    if !c.untracked.is_empty() {
        lines.push(msg::UNTRACKED_HEADER.into());
        if r.hints {
            lines.push(msg::HINT_INCLUDE.into());
        }
        for p in &c.untracked {
            let mut l = b"\t".to_vec();
            l.extend_from_slice(&r.path(p));
            lines.push(l);
        }
        lines.push(Vec::new());
    }
    if r.untracked_not_listed {
        lines.push(
            if r.hints {
                msg::UNTRACKED_NOT_LISTED_HINT
            } else {
                msg::UNTRACKED_NOT_LISTED
            }
            .into(),
        );
    }
    if let Some(last) = &r.last {
        lines.push(last.as_bytes().to_vec());
    }
    let mut out = Vec::new();
    for l in lines {
        if let Some(cm) = r.comment {
            out.extend_from_slice(cm.as_bytes());
            if !l.is_empty() && l[0] != b'\t' {
                out.push(b' ');
            }
        }
        out.extend_from_slice(&l);
        out.push(b'\n');
    }
    out
}
