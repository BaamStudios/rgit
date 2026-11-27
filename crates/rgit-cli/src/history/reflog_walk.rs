// SPDX-License-Identifier: MIT
//! `log -g` / `--walk-reflogs` (Documentation/rev-list-options.adoc): the
//! reflog entries of the tips instead of the history.

use rgit_hash::ObjectId;
use rgit_revwalk::walk::grep::CommitGrep;
use rgit_revwalk::walk::{CommitSource, Shown, Walk};

use super::{Printer, options, paths, userformat};
use crate::Die;
use crate::messages::rev_list as msg;

/// One reflog being walked by `log -g`.
struct ReflogWalk {
    source: crate::commands::reflog::WalkSource,
    /// Position, counted from the newest entry.
    next: usize,
}

impl ReflogWalk {
    /// The entry at the current position (newest first).
    fn current(&self) -> Option<&rgit_refs::reflog::Entry> {
        let n = self.source.entries.len();
        (self.next < n).then(|| &self.source.entries[n - 1 - self.next])
    }
}

/// `log -g` / `--walk-reflogs` (Documentation/rev-list-options.adoc):
/// the reflog entries of every tip (`HEAD` by default) from the newest,
/// each shown as its commit with the reflog selector, identity and
/// message. Several reflogs are interleaved by commit date.
pub(super) fn run_reflogs<S: CommitSource<Error = ObjectId>>(
    p: &mut Printer<'_, '_>,
    walk: &Walk<S>,
    mut paths: Option<paths::PathFilter>,
) -> Result<i32, Die> {
    let opts = p.opts;
    if opts.walk.reverse {
        return Err(Die::Fatal(msg::cannot_be_used_together(
            "--reverse",
            "--walk-reflogs",
        )));
    }
    if opts.graph {
        return Err(Die::Fatal(msg::REFLOG_HISTORY_LIMITING.to_string()));
    }
    let mut args = Vec::new();
    let mut ref_sets = false;
    for t in &opts.tips {
        match t {
            options::Tip::Rev { arg, negate } => {
                if *negate || arg.starts_with('^') || arg.contains("..") {
                    let name = arg.trim_start_matches('^');
                    let name = name.split("..").next().unwrap_or(name);
                    return Err(Die::Fatal(msg::cannot_walk_reflogs(name)));
                }
                args.push(arg.clone());
            }
            // `--all`, `--branches`, ...: the reflog of every ref of the
            // set under its full name (`HEAD` last for `--all`); refs
            // without a reflog add nothing.
            options::Tip::Refs {
                set,
                negative: false,
                exclude,
                hidden,
            } if matches!(
                set,
                options::RefSet::All | options::RefSet::Prefix { .. } | options::RefSet::Glob(_)
            ) =>
            {
                ref_sets = true;
                let hidden = hidden
                    .as_deref()
                    .map(|s| super::tips::Hidden::new(p.repo, s));
                let names = super::tips::ref_set(p.repo, opts, set, exclude, hidden.as_ref())?;
                let mut seen = std::collections::HashSet::new();
                for (name, _) in names {
                    // `--branches` names its refs without `refs/heads/`
                    // (the other sets keep the full name).
                    let name = match set {
                        options::RefSet::Prefix {
                            prefix: "refs/heads/",
                            ..
                        } => name.strip_prefix("refs/heads/").unwrap_or(&name).to_owned(),
                        _ => name,
                    };
                    if seen.insert(name.clone()) {
                        args.push(name);
                    }
                }
            }
            options::Tip::Refs { .. } => {}
        }
    }
    if args.is_empty() && !ref_sets {
        args.push("HEAD".to_string());
    }
    let mut walks = Vec::new();
    for arg in &args {
        let source = crate::commands::reflog::walk_source(p.repo.setup, arg)?;
        let next = source.start;
        walks.push(ReflogWalk { source, next });
    }
    let grep = if opts.grep.is_empty() {
        None
    } else {
        Some(CommitGrep::new(&opts.grep).map_err(Die::Fatal)?)
    };
    let mut skip = opts.walk.skip;
    let mut shown = 0usize;
    loop {
        // Entries whose object is not a commit are passed over.
        for w in walks.iter_mut() {
            while let Some(e) = w.current() {
                if p.repo.read_commit(&e.new).is_some() {
                    break;
                }
                w.next += 1;
            }
        }
        let pick = walks
            .iter()
            .enumerate()
            .filter_map(|(i, w)| w.current().map(|e| (i, e.time)))
            .fold(None, |best: Option<(usize, u64)>, (i, t)| match best {
                Some((_, bt)) if bt >= t => best,
                _ => Some((i, t)),
            });
        let Some((i, _)) = pick else {
            break;
        };
        if opts.walk.max_count.is_some_and(|m| shown >= m) {
            break;
        }
        let w = &mut walks[i];
        let index = w.next;
        w.next += 1;
        let Some(e) = w
            .source
            .entries
            .get(w.source.entries.len() - 1 - index)
            .cloned()
        else {
            continue;
        };
        if !reflog_entry_wanted(p, &e, grep.as_ref(), paths.as_mut())? {
            continue;
        }
        if skip > 0 {
            skip -= 1;
            continue;
        }
        p.reflog = Some(reflog_info(p, &w.source, index, &e));
        let s = Shown {
            id: e.new,
            boundary: false,
            side: None,
            same: false,
        };
        p.commit(walk, &s)?;
        shown += 1;
    }
    p.reflog = None;
    p.flush();
    Ok(0)
}

/// The limits of the walk applied to one reflog entry: the commit filters
/// (`--grep`, `--merges`, `--no-merges`, paths) and the entry's date
/// against `--since`/`--until`.
fn reflog_entry_wanted(
    p: &Printer<'_, '_>,
    e: &rgit_refs::reflog::Entry,
    grep: Option<&CommitGrep>,
    paths: Option<&mut paths::PathFilter>,
) -> Result<bool, Die> {
    let w = &p.opts.walk;
    let time = i64::try_from(e.time).unwrap_or(i64::MAX);
    if w.max_age
        .or(w.max_age_filter)
        .is_some_and(|since| time < since)
        || w.min_age.is_some_and(|until| time > until)
    {
        return Ok(false);
    }
    let Some((commit, raw)) = p.repo.read_commit(&e.new) else {
        return Ok(false);
    };
    let parents = p.repo.parents(&e.new, &commit);
    if w.min_parents.is_some_and(|m| parents.len() < m)
        || w.max_parents.is_some_and(|m| parents.len() > m)
    {
        return Ok(false);
    }
    if grep.is_some_and(|g| !g.matches(&raw)) {
        return Ok(false);
    }
    if let Some(f) = paths {
        let bad = |id: ObjectId| Die::Fatal(msg::bad_object(&id.to_hex()));
        // Shown unless it equals a parent at the paths (a root commit is
        // compared with the empty tree).
        if parents.is_empty() {
            return Ok(!f.treesame(p.repo, &e.new, None).map_err(bad)?);
        }
        for parent in &parents {
            if f.treesame(p.repo, &e.new, Some(parent)).map_err(bad)? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// The `%g*` values of the entry `index` (from the newest) of a reflog.
fn reflog_info(
    p: &Printer<'_, '_>,
    source: &crate::commands::reflog::WalkSource,
    index: usize,
    e: &rgit_refs::reflog::Entry,
) -> userformat::ReflogInfo {
    // `@{<n>}` unless the walk started at a date, or `--date` was given
    // (and the start was not an explicit count).
    let by_date = source.by_date || (p.opts.date_given && !source.by_index);
    let label = if by_date {
        let mode = if p.opts.date_given {
            p.opts.date_mode.clone()
        } else {
            rgit_date::DateMode::default()
        };
        rgit_date::show_date(e.time, e.tz, &mode, &p.style.date_ctx)
            .unwrap_or_else(|_| rgit_date::format_raw(e.time, e.tz))
    } else {
        index.to_string()
    };
    let short = source
        .display
        .strip_prefix("refs/heads/")
        .or_else(|| source.display.strip_prefix("refs/tags/"))
        .or_else(|| source.display.strip_prefix("refs/remotes/"))
        .unwrap_or(&source.display);
    let (name, email) = match e.ident.iter().position(|&b| b == b'<') {
        Some(lt) => {
            let name = e.ident[..lt].trim_ascii_end().to_vec();
            let rest = &e.ident[lt + 1..];
            let end = rest.iter().position(|&b| b == b'>').unwrap_or(rest.len());
            (name, rest[..end].to_vec())
        }
        None => (e.ident.clone(), Vec::new()),
    };
    userformat::ReflogInfo {
        selector: format!("{}@{{{label}}}", source.display),
        short_selector: format!("{short}@{{{label}}}"),
        name,
        email,
        message: e.message.clone(),
    }
}
