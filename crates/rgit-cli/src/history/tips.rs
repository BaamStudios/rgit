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
    /// The paths: after `--`, or the arguments from the first one that is
    /// not a revision but names a file (without `--`).
    pub paths: Vec<String>,
    /// All arguments turned out to be paths: no revision was given.
    pub no_revisions: bool,
}

fn print_diagnostics(resolver: &Resolver<'_>) {
    let mut err = std::io::stderr();
    for d in resolver.take_diagnostics() {
        let _ = err.write_all(d.render().as_bytes());
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
    /// Start at `HEAD` (log with paths only).
    pub fn push_head(&mut self, repo: &Repo<'_>) -> Result<(), Die> {
        for item in rev_items(repo, "HEAD")? {
            self.add(repo, item.oid, false, None, Vec::new(), "HEAD")?;
        }
        Ok(())
    }

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
            // Blobs are not read (they may be large).
            if repo.objects.kind_of(&id) == Some(ObjectKind::Blob) {
                if negative {
                    self.excluded.push((id, ObjectKind::Blob));
                } else {
                    self.pending.push(Pending {
                        id,
                        kind: ObjectKind::Blob,
                        name,
                    });
                }
                return Ok(());
            }
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

/// `--exclude-hidden=<section>`: the refs `transfer.hideRefs` and
/// `<section>.hideRefs` hide (git-config(1)): a ref is hidden by the last
/// matching pattern unless that starts with `!`; patterns match a ref name
/// or the directory it is in, after the `GIT_NAMESPACE` prefix was
/// removed, or the full name with `^`.
struct Hidden {
    patterns: Vec<String>,
    namespace: Option<String>,
}

impl Hidden {
    fn new(repo: &Repo<'_>, section: &str) -> Hidden {
        let own = format!("{section}.hiderefs");
        let patterns = repo
            .config
            .entries()
            .iter()
            .filter(|e| e.name == "transfer.hiderefs" || e.name == own)
            .filter_map(|e| e.value.as_ref().map(|v| v.to_string()))
            .collect();
        let namespace = std::env::var("GIT_NAMESPACE")
            .ok()
            .filter(|n| !n.is_empty())
            .map(|n| {
                n.split('/')
                    .filter(|c| !c.is_empty())
                    .map(|c| format!("refs/namespaces/{c}/"))
                    .collect::<String>()
            });
        Hidden {
            patterns,
            namespace,
        }
    }

    fn hides(&self, name: &str) -> bool {
        let stripped = self
            .namespace
            .as_deref()
            .and_then(|ns| name.strip_prefix(ns))
            .unwrap_or(name);
        for p in self.patterns.iter().rev() {
            let (neg, p) = match p.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, p.as_str()),
            };
            let (subject, p) = match p.strip_prefix('^') {
                Some(rest) => (name, rest),
                None => (stripped, p),
            };
            let p = p.trim_end_matches('/');
            let hit = subject
                .strip_prefix(p)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'));
            if hit {
                return !neg;
            }
        }
        false
    }
}

/// Object ids named in the reflog files below `dir` (recursively), in
/// file order: each entry's old value, then its new value.
fn reflog_ids(dir: &std::path::Path, kind: rgit_hash::HashKind, out: &mut Vec<ObjectId>) {
    let Ok(meta) = std::fs::metadata(dir) else {
        return;
    };
    if meta.is_dir() {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<_> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        paths.sort();
        for p in paths {
            reflog_ids(&p, kind, out);
        }
        return;
    }
    let Ok(text) = std::fs::read(dir) else {
        return;
    };
    let hex = kind.hex_len();
    for line in text.split(|&b| b == b'\n') {
        for range in [0..hex, hex + 1..2 * hex + 1] {
            if let Some(h) = line.get(range)
                && let Ok(id) = ObjectId::from_hex(kind, h)
                && !id.is_null()
            {
                out.push(id);
            }
        }
    }
}

/// The git directories of the other work trees.
fn other_worktree_dirs(repo: &Repo<'_>) -> Vec<std::path::PathBuf> {
    let own = std::path::absolute(&repo.setup.git_dir).ok();
    let mut dirs = vec![repo.setup.common_dir.clone()];
    if let Ok(entries) = std::fs::read_dir(repo.setup.common_dir.join("worktrees")) {
        let mut wt: Vec<_> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        wt.sort();
        dirs.extend(wt);
    }
    dirs.retain(|d| std::path::absolute(d).ok() != own);
    dirs
}

/// The refs a ref-set option (`--all`, `--branches`, `--tags`, `--glob`,
/// ...) names, in order, with the objects they point to (tags not peeled),
/// for `show`, which shows each of them without walking.
pub(super) fn named_refs(
    repo: &Repo<'_>,
    opts: &Options,
    set: &RefSet,
    exclude: &[String],
    hidden: Option<&str>,
) -> Result<Vec<(String, ObjectId)>, Die> {
    let hidden = hidden.map(|s| Hidden::new(repo, s));
    ref_set(repo, opts, set, exclude, hidden.as_ref())
}

/// The refs of a ref set, in order.
fn ref_set(
    repo: &Repo<'_>,
    opts: &Options,
    set: &RefSet,
    exclude: &[String],
    hidden: Option<&Hidden>,
) -> Result<Vec<(String, ObjectId)>, Die> {
    let is_hidden = |name: &str| hidden.is_some_and(|h| h.hides(name));
    let mut ids = Vec::new();
    let each = |prefix: &str, f: &mut dyn FnMut(&str, &ObjectId)| {
        repo.refs
            .for_each_ref(prefix, f)
            .map_err(|e| Die::Fatal(e.message()))
    };
    match set {
        RefSet::All => {
            each("refs/", &mut |name, id| {
                if !excluded(exclude, name) && !is_hidden(name) {
                    ids.push((name.to_string(), *id));
                }
            })?;
            if !excluded(exclude, "HEAD")
                && let Ok(Some(head)) = repo.refs.resolve("HEAD")
                && let Some(id) = head.oid
            {
                ids.push(("HEAD".to_string(), id));
            }
            if !opts.single_worktree {
                ids.extend(
                    repo.other_worktree_heads()
                        .into_iter()
                        .map(|id| ("HEAD".to_string(), id)),
                );
            }
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
                    && !is_hidden(name)
                {
                    ids.push((name.to_string(), *id));
                }
            })?;
        }
        RefSet::Reflog => {
            let kind = repo.objects.hash_kind();
            let mut found = Vec::new();
            reflog_ids(&repo.setup.git_dir.join("logs/HEAD"), kind, &mut found);
            reflog_ids(&repo.setup.common_dir.join("logs/refs"), kind, &mut found);
            if !opts.single_worktree {
                for dir in other_worktree_dirs(repo) {
                    reflog_ids(&dir.join("logs/HEAD"), kind, &mut found);
                }
            }
            // Entries whose objects are gone are left out.
            ids.extend(
                found
                    .into_iter()
                    .filter(|id| matches!(repo.objects.read(id), Ok(Some(_))))
                    .map(|id| (String::new(), id)),
            );
        }
        RefSet::Indexed | RefSet::AlternateRefs => {}
    }
    Ok(ids)
}

/// The path part of a pathspec argument and whether it is relative to
/// the top (`:/`, `:(top)`): the magic is stripped.
fn strip_magic(arg: &str) -> (bool, &str) {
    if let Some(rest) = arg.strip_prefix(":(") {
        let Some(end) = rest.find(')') else {
            return (false, arg);
        };
        let top = rest[..end].split(',').any(|w| w == "top");
        return (top, &rest[end + 1..]);
    }
    let Some(rest) = arg.strip_prefix(':') else {
        return (false, arg);
    };
    let mut top = false;
    for (i, c) in rest.char_indices() {
        match c {
            '/' => top = true,
            '!' | '^' => {}
            ':' => return (top, &rest[i + 1..]),
            _ => return (top, &rest[i..]),
        }
    }
    (top, "")
}

/// Whether `arg` names paths without `--`: after the pathspec magic it is
/// a file or directory of the work tree, empty, or a wildcard pattern.
fn names_path(repo: &Repo<'_>, arg: &str) -> bool {
    let (top, rest) = strip_magic(arg);
    if rest.is_empty() {
        // `:/` is the top; a lone `:` is nothing.
        return arg != ":";
    }
    if rgit_pathspec::wildmatch::has_glob_special(rest.as_bytes()) {
        return true;
    }
    file_exists(repo, top, rest)
}

/// `path` exists in the work tree (relative to the top or to the current
/// directory).
fn file_exists(repo: &Repo<'_>, top: bool, path: &str) -> bool {
    let Some(root) = &repo.setup.work_tree else {
        return false;
    };
    let mut full = root.clone();
    if !top && let Some(prefix) = &repo.setup.prefix {
        full.push(prefix);
    }
    full.push(path);
    full.symlink_metadata().is_ok()
}

/// A revision argument that also names a file (git then wants `--`).
fn also_a_file(repo: &Repo<'_>, arg: &str) -> bool {
    // `:<path>` (an index entry) is no file name: git shows it without
    // asking for `--` even when `<path>` exists (t0021 "clean empty file").
    if arg.starts_with(':') && !arg.starts_with(":/") && !arg.starts_with(":(") {
        return false;
    }
    let (top, rest) = strip_magic(arg);
    !rest.is_empty() && file_exists(repo, top, rest)
}

/// The valid trees of a cache tree, with their paths.
fn cache_trees(node: &rgit_index::TreeNode, path: &[u8], out: &mut Vec<(Vec<u8>, ObjectId)>) {
    if let Some(id) = node.id() {
        out.push((path.to_vec(), *id));
    }
    for child in &node.children {
        let mut p = path.to_vec();
        if !p.is_empty() {
            p.push(b'/');
        }
        p.extend_from_slice(&child.name);
        cache_trees(child, &p, out);
    }
}

/// `--indexed-objects`: the valid trees of the cache tree and the blobs
/// of the index, with their paths.
fn indexed_objects(repo: &Repo<'_>) -> Vec<(Vec<u8>, ObjectId)> {
    let kind = repo.objects.hash_kind();
    let path = std::env::var_os("GIT_INDEX_FILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| repo.setup.git_dir.join("index"));
    let Ok(index) = rgit_index::Index::read_file(&path, kind, &rgit_index::ReadOptions::default())
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(tree) = index.cache_tree() {
        cache_trees(&tree.root, b"", &mut out);
    }
    out.extend(
        index
            .entries()
            .iter()
            .filter(|e| !e.mode.is_gitlink())
            .map(|e| (e.path().to_vec(), e.id)),
    );
    out
}

/// `--alternate-refs`: the ref tips of the repositories whose object
/// directories are alternates of ours.
fn alternate_refs(repo: &Repo<'_>) -> Vec<ObjectId> {
    let objects = crate::revisions::objects_dir(repo.setup);
    rgit_fetch::alternate_tips(repo.config, repo.objects.hash_kind(), &objects)
}

/// Resolve the starting points in command-line order. Without `--`, an
/// argument that is not a revision but a file in the work tree (or looks
/// like a pathspec) starts the paths when `paths` allows it.
pub fn resolve_with(repo: &Repo<'_>, opts: &Options, paths: bool) -> Result<Resolved, Die> {
    let mut out = Resolved::default();
    let mut in_paths = false;
    let mut revisions = false;
    // `--merge`: `HEAD...<other>` before the revisions given.
    let merge_range = match opts.merge {
        true => Some(Tip::Rev {
            arg: format!("HEAD...{}", merge_other(repo)?),
            negate: false,
        }),
        false => None,
    };
    for tip in merge_range.iter().chain(&opts.tips) {
        match tip {
            Tip::Rev { arg, .. } if in_paths => {
                if !names_path(repo, arg) {
                    return Err(Die::Fatal(msg::no_such_path(arg)));
                }
                out.paths.push(arg.clone());
            }
            Tip::Rev { arg, negate } => {
                let found = rev_items(repo, arg);
                let mut items = match found {
                    Err(_) if opts.ignore_missing => continue,
                    Err(e) if paths && !opts.dashdash && !arg.starts_with('^') => {
                        if names_path(repo, arg) {
                            in_paths = true;
                            out.no_revisions = !revisions;
                            out.paths.push(arg.clone());
                            continue;
                        }
                        return Err(e);
                    }
                    // After `--` an argument can only be a revision.
                    Err(Die::Fatal(m)) if opts.dashdash && m == msg::ambiguous_argument(arg) => {
                        return Err(Die::Fatal(msg::bad_revision(arg)));
                    }
                    found => found?,
                };
                revisions = true;
                if !opts.dashdash && !opts.default_head && arg != "-" && also_a_file(repo, arg) {
                    return Err(Die::Fatal(msg::revision_and_filename(arg)));
                }
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
                    if opts.ignore_missing && !matches!(repo.objects.read(&item.oid), Ok(Some(_))) {
                        continue;
                    }
                    out.add(repo, item.oid, item.negated != *negate, side, name, source)?;
                }
            }
            Tip::Refs {
                set,
                negative,
                exclude,
                hidden,
            } => {
                revisions = true;
                out.no_revisions = false;
                let hidden = hidden.as_deref().map(|s| Hidden::new(repo, s));
                for (refname, id) in ref_set(repo, opts, set, exclude, hidden.as_ref())? {
                    out.add(repo, id, *negative, None, Vec::new(), &refname)?;
                }
                match set {
                    RefSet::Indexed => {
                        for (path, id) in indexed_objects(repo) {
                            out.add(repo, id, *negative, None, path, "")?;
                        }
                    }
                    RefSet::AlternateRefs => {
                        for id in alternate_refs(repo) {
                            if matches!(repo.objects.read(&id), Ok(Some(_))) {
                                out.add(repo, id, *negative, None, Vec::new(), "")?;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    out.paths.extend(opts.paths.iter().cloned());
    Ok(out)
}

/// `--merge`: the first pseudoref of a merge in progress there is.
fn merge_other(repo: &Repo<'_>) -> Result<&'static str, Die> {
    [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "REBASE_HEAD",
    ]
    .into_iter()
    .find(|name| repo.setup.git_dir.join(name).is_file())
    .ok_or_else(|| Die::Fatal(msg::MERGE_PSEUDOREFS.to_owned()))
}

/// `--merge`: the conflicted paths of the index that `paths` (if any)
/// match, as literal pathspecs from the top; none limits nothing.
pub fn merge_paths(repo: &Repo<'_>, paths: &rgit_pathspec::Pathspec) -> Vec<String> {
    let kind = repo.objects.hash_kind();
    let path = std::env::var_os("GIT_INDEX_FILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| repo.setup.git_dir.join("index"));
    let Ok(index) = rgit_index::Index::read_file(&path, kind, &rgit_index::ReadOptions::default())
    else {
        return Vec::new();
    };
    let mut out: Vec<String> = index
        .entries()
        .iter()
        .filter(|e| e.stage().get() != 0 && (paths.is_empty() || paths.matches(e.path())))
        .map(|e| format!(":(top,literal){}", String::from_utf8_lossy(e.path())))
        .collect();
    out.dedup();
    out
}

/// [`resolve_with`] taking paths.
pub fn resolve(repo: &Repo<'_>, opts: &Options) -> Result<Resolved, Die> {
    resolve_with(repo, opts, true)
}
