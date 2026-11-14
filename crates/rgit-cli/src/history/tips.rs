// SPDX-License-Identifier: MIT
//! Resolving the starting points of `rev-list` and `log`: revision
//! arguments, ref sets (`--all`, `--branches=<pattern>`, `--glob`,
//! `--exclude`) and, for `--objects`, the tags, trees and blobs among them.

use std::io::Write;

use rgit_hash::ObjectId;
use rgit_object::ObjectKind;
use rgit_pathspec::wildmatch::{WildmatchFlags, wildmatch};
use rgit_revwalk::revision::{
    Error as RevError, Flags, Hint, ObjectLookup, RefLookup, Resolver, RevItem,
};
use rgit_revwalk::walk::Side;

use super::Repo;
use super::options::{Options, RefSet, Tip};
use crate::Die;
use crate::messages::rev_list as msg;

/// A tag, tree or blob given as a starting point (`--objects`).
#[derive(Debug, Clone)]
pub struct Pending {
    pub id: ObjectId,
    pub kind: ObjectKind,
    /// The name it is listed with.
    pub name: Vec<u8>,
}

/// The resolved starting points.
#[derive(Debug, Default)]
pub struct Resolved {
    /// Commits in command-line order: `(commit, excluded, side)`.
    pub commits: Vec<(ObjectId, bool, Option<Side>)>,
    /// The revision each of `commits` was given as (for `%S`).
    pub sources: Vec<String>,
    /// Included tags, trees and blobs, in command-line order.
    pub pending: Vec<Pending>,
    /// Excluded tags, trees and blobs.
    pub excluded: Vec<(ObjectId, ObjectKind)>,
}

fn print_diagnostics(resolver: &Resolver<'_>) {
    let mut err = std::io::stderr();
    for d in resolver.take_diagnostics() {
        let _ = writeln!(err, "{}", d.render());
    }
}

pub fn rev_error(resolver: &Resolver<'_>, e: RevError) -> Die {
    print_diagnostics(resolver);
    match e {
        RevError::Fatal(m) => Die::Fatal(m),
        RevError::Lookup(e) => {
            for line in &e.errors {
                let _ = writeln!(std::io::stderr(), "error: {line}");
            }
            match e.fatal {
                Some(m) => Die::Fatal(m),
                None => Die::Exit(rgit_options::EXIT_FATAL),
            }
        }
        RevError::NotFound => Die::Exit(rgit_options::EXIT_FATAL),
    }
}

/// The objects one revision argument stands for.
pub fn rev_items(repo: &Repo<'_>, arg: &str) -> Result<Vec<RevItem>, Die> {
    let resolver = repo.resolver;
    let flags = Flags::default().with_hint(Hint::Committish);
    let found = match resolver.try_range(arg, flags) {
        Ok(Some(items)) => Some(items),
        Ok(None) => resolver
            .try_parent_shorthand(arg, flags)
            .map_err(|e| rev_error(resolver, e))?,
        Err(e) => return Err(rev_error(resolver, e)),
    };
    if let Some(items) = found {
        print_diagnostics(resolver);
        return Ok(items);
    }
    let (name, negated) = match arg.strip_prefix('^') {
        Some(n) => (n, true),
        None => (arg, false),
    };
    match resolver.resolve_with(name, flags) {
        Ok(oid) => {
            print_diagnostics(resolver);
            Ok(vec![RevItem::new(oid, negated, Some(name.to_string()))])
        }
        Err(RevError::NotFound) => {
            print_diagnostics(resolver);
            if negated {
                Err(Die::Fatal(msg::bad_revision(arg)))
            } else if repo.is_path(arg) {
                Err(Die::Fatal(msg::not_supported("path limiting")))
            } else {
                Err(Die::Fatal(msg::ambiguous_argument(arg)))
            }
        }
        Err(e) => Err(rev_error(resolver, e)),
    }
}

/// The name a tree or blob argument is listed with: the path of
/// `<rev>:<path>` (or `:<n>:<path>`), else empty.
fn path_name(name: Option<&str>) -> Vec<u8> {
    let Some(name) = name else {
        return Vec::new();
    };
    let Some(pos) = name.find(':') else {
        return Vec::new();
    };
    let mut path = &name[pos + 1..];
    if pos == 0 {
        // `:<n>:<path>`
        let b = path.as_bytes();
        if b.len() >= 2 && b[0].is_ascii_digit() && b[1] == b':' {
            path = &path[2..];
        }
    }
    path.as_bytes().to_vec()
}

impl Resolved {
    /// Add `id` (any object): commits go to the walk, tags are peeled.
    fn add(
        &mut self,
        repo: &Repo<'_>,
        mut id: ObjectId,
        negative: bool,
        side: Option<Side>,
        name: Vec<u8>,
        source: &str,
    ) -> Result<(), Die> {
        let mut name = name;
        for _ in 0..64 {
            let Ok(Some((kind, data))) = repo.objects.read(&id) else {
                return Err(Die::Fatal(msg::bad_object(&id.to_hex())));
            };
            match kind {
                ObjectKind::Commit => {
                    self.commits.push((id, negative, side));
                    self.sources.push(source.to_string());
                    return Ok(());
                }
                ObjectKind::Tag => {
                    let tag = rgit_object::Tag::parse(repo.objects.hash_kind(), &data)
                        .map_err(|_| Die::Fatal(msg::bad_object(&id.to_hex())))?;
                    let tag_name = tag.name().unwrap_or_default().to_vec();
                    if negative {
                        self.excluded.push((id, kind));
                    } else {
                        self.pending.push(Pending {
                            id,
                            kind,
                            name: tag_name,
                        });
                    }
                    id = tag
                        .object()
                        .map_err(|_| Die::Fatal(msg::bad_object(&id.to_hex())))?;
                    name = Vec::new();
                }
                ObjectKind::Tree | ObjectKind::Blob => {
                    if negative {
                        self.excluded.push((id, kind));
                    } else {
                        self.pending.push(Pending { id, kind, name });
                    }
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}

/// Whether `name` matches one of the `--exclude` patterns.
fn excluded(patterns: &[String], name: &str) -> bool {
    patterns
        .iter()
        .any(|p| wildmatch(p.as_bytes(), name.as_bytes(), WildmatchFlags::NONE))
}

/// A pattern of `--branches=`, `--glob=`, ...: `/*` is implied when it
/// has no glob characters.
fn full_pattern(pattern: &str) -> String {
    if pattern.contains(['*', '?', '[']) {
        pattern.to_string()
    } else if pattern.ends_with('/') {
        format!("{pattern}*")
    } else {
        format!("{pattern}/*")
    }
}

/// The refs of a ref set, in order.
fn ref_set(
    repo: &Repo<'_>,
    set: &RefSet,
    exclude: &[String],
) -> Result<Vec<(String, ObjectId)>, Die> {
    let mut ids = Vec::new();
    let each = |prefix: &str, f: &mut dyn FnMut(&str, &ObjectId)| {
        repo.refs
            .for_each_ref(prefix, f)
            .map_err(|e| Die::Fatal(e.message()))
    };
    match set {
        RefSet::All => {
            each("refs/", &mut |name, id| {
                if !excluded(exclude, name) {
                    ids.push((name.to_string(), *id));
                }
            })?;
            if !excluded(exclude, "HEAD")
                && let Ok(Some(head)) = repo.refs.resolve("HEAD")
                && let Some(id) = head.oid
            {
                ids.push(("HEAD".to_string(), id));
            }
            ids.extend(
                repo.other_worktree_heads()
                    .into_iter()
                    .map(|id| ("HEAD".to_string(), id)),
            );
        }
        RefSet::Prefix { prefix, pattern } => {
            let pat = pattern
                .as_deref()
                .map(|p| format!("{prefix}{}", full_pattern(p)));
            each(prefix, &mut |name, id| {
                let wanted = pat
                    .as_ref()
                    .is_none_or(|p| wildmatch(p.as_bytes(), name.as_bytes(), WildmatchFlags::NONE));
                let short = name.strip_prefix(*prefix).unwrap_or(name);
                if wanted && !excluded(exclude, short) {
                    ids.push((name.to_string(), *id));
                }
            })?;
        }
        RefSet::Glob(pattern) => {
            let pat = full_pattern(pattern);
            each("refs/", &mut |name, id| {
                if wildmatch(pat.as_bytes(), name.as_bytes(), WildmatchFlags::NONE)
                    && !excluded(exclude, name)
                {
                    ids.push((name.to_string(), *id));
                }
            })?;
        }
    }
    Ok(ids)
}

/// Resolve the starting points in command-line order.
pub fn resolve(repo: &Repo<'_>, opts: &Options) -> Result<Resolved, Die> {
    let mut out = Resolved::default();
    for tip in &opts.tips {
        match tip {
            Tip::Rev { arg, negate } => {
                let mut items = rev_items(repo, arg)?;
                let symmetric = items.iter().any(|i| i.left);
                if symmetric {
                    // The left side is pushed first, like git does.
                    if let Some(pos) = items.iter().position(|i| i.left) {
                        let left = items.remove(pos);
                        items.insert(0, left);
                    }
                }
                for item in items {
                    let side = match (symmetric, item.left) {
                        (false, _) => None,
                        (true, true) => Some(Side::Left),
                        (true, false) => Some(Side::Right),
                    };
                    let name = path_name(item.name.as_deref());
                    let source = item.name.as_deref().unwrap_or(arg);
                    out.add(repo, item.oid, item.negated != *negate, side, name, source)?;
                }
            }
            Tip::Refs {
                set,
                negative,
                exclude,
            } => {
                for (refname, id) in ref_set(repo, set, exclude)? {
                    out.add(repo, id, *negative, None, Vec::new(), &refname)?;
                }
            }
        }
    }
    Ok(out)
}
