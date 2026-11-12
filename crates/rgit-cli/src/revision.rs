// SPDX-License-Identifier: MIT
//! A small subset of revision syntax (`gitrevisions(7)`) for plumbing that
//! takes a tree-ish: full and abbreviated object names, ref names with the
//! documented lookup rules (through `rgit-refs`), `^{<type>}`, `^{}`,
//! `^<n>`, `~<n>` and `<rev>:<path>`.
//!
//! This is a stop-gap until full revision parsing (`rev-parse`) exists; it
//! only reads.

use rgit_hash::{ObjectId, Prefix};
use rgit_object::{Commit, Object, ObjectKind, Tag, Tree};
use rgit_odb::PrefixMatch;
use rgit_refs::files::FilesStore;
use rgit_refs::name::{RefNameFlags, is_valid_refname};
use rgit_refs::{MAX_SYMREF_DEPTH, RefStore, Target};

use crate::repository::Repo;

/// Why a name did not resolve. The command prints its own message
/// (usually `Not a valid object name <name>`); `error` is an extra
/// `error:` line to print before it.
#[derive(Debug, Default)]
pub struct NotFound {
    /// Text of an `error:` line explaining the failure.
    pub error: Option<String>,
}

/// Refs that live in the work tree's own git directory.
fn per_worktree(name: &str) -> bool {
    !name.starts_with("refs/")
        || name.starts_with("refs/bisect/")
        || name.starts_with("refs/worktree/")
        || name.starts_with("refs/rewritten/")
}

/// The reference store holding `name`.
fn store_for(repo: &Repo, name: &str) -> FilesStore {
    let dir = if per_worktree(name) {
        repo.setup.cwd.join(&repo.setup.git_dir)
    } else {
        repo.setup.cwd.join(&repo.setup.common_dir)
    };
    FilesStore::new(dir, repo.hash())
}

/// Resolve the ref `name`, following symbolic refs across the per-worktree
/// and common stores.
pub fn read_ref(repo: &Repo, name: &str) -> Option<ObjectId> {
    let flags = RefNameFlags::ONELEVEL;
    let mut name = name.to_string();
    for _ in 0..MAX_SYMREF_DEPTH {
        if !is_valid_refname(name.as_bytes(), flags) {
            return None;
        }
        match store_for(repo, &name).read_raw(&name).ok()?? {
            Target::Object(id) => return Some(id),
            Target::Symbolic(next) => name = next,
        }
    }
    None
}

/// The ref lookup rules of `gitrevisions(7)`.
fn dwim_ref(repo: &Repo, name: &str) -> Option<ObjectId> {
    let name = if name == "@" { "HEAD" } else { name };
    let candidates = [
        name.to_string(),
        format!("refs/{name}"),
        format!("refs/tags/{name}"),
        format!("refs/heads/{name}"),
        format!("refs/remotes/{name}"),
        format!("refs/remotes/{name}/HEAD"),
    ];
    for (i, c) in candidates.iter().enumerate() {
        // `$GIT_DIR/<name>` only for names outside `refs/` that look like
        // HEAD, FETCH_HEAD, ...
        if i == 0
            && !c.starts_with("refs/")
            && !c.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
        {
            continue;
        }
        if let Some(id) = read_ref(repo, c) {
            return Some(id);
        }
    }
    None
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn base_object(repo: &Repo, name: &str) -> Result<ObjectId, NotFound> {
    let hexlen = repo.hash().hex_len();
    if name.len() == hexlen && is_hex(name) {
        return ObjectId::from_hex(repo.hash(), name.as_bytes()).map_err(|_| NotFound::default());
    }
    if let Some(id) = dwim_ref(repo, name) {
        return Ok(id);
    }
    if name.len() >= 4 && name.len() < hexlen && is_hex(name) {
        let prefix =
            Prefix::from_hex(repo.hash(), name.as_bytes()).map_err(|_| NotFound::default())?;
        if let Ok(PrefixMatch::Unique(id)) = repo.odb().resolve_prefix(&prefix) {
            return Ok(id);
        }
    }
    Err(NotFound::default())
}

/// Read and parse an object.
pub fn read_object(repo: &Repo, id: &ObjectId) -> Option<Object> {
    repo.odb().read_object(id).ok().flatten()
}

/// Peel tags until the object is not a tag.
fn peel_tags(repo: &Repo, mut id: ObjectId) -> Result<(ObjectId, Object), NotFound> {
    loop {
        let obj = read_object(repo, &id).ok_or_else(NotFound::default)?;
        match &obj {
            Object::Tag(t) => id = tag_target(t)?,
            _ => return Ok((id, obj)),
        }
    }
}

fn tag_target(t: &Tag) -> Result<ObjectId, NotFound> {
    t.object().map_err(|_| NotFound::default())
}

fn commit_of(repo: &Repo, id: ObjectId) -> Result<Commit, NotFound> {
    match peel_tags(repo, id)?.1 {
        Object::Commit(c) => Ok(c),
        _ => Err(NotFound::default()),
    }
}

/// Peel `id` to an object of type `kind`: tags to their target, commits
/// to their tree. On a mismatch the error names the type reached.
pub fn peel_to(repo: &Repo, id: ObjectId, kind: ObjectKind) -> Result<ObjectId, NotFound> {
    let mut id = id;
    loop {
        let obj = read_object(repo, &id).ok_or_else(NotFound::default)?;
        if obj.kind() == kind {
            return Ok(id);
        }
        id = match &obj {
            Object::Tag(t) => tag_target(t)?,
            Object::Commit(c) => c.tree().map_err(|_| NotFound::default())?,
            _ => {
                return Err(NotFound {
                    error: Some(obj.kind().name().to_string()),
                });
            }
        };
    }
}

/// `^{<type>}`: like [`peel_to`], with git's error line.
fn peel_spec(
    repo: &Repo,
    spec: &str,
    id: ObjectId,
    kind: ObjectKind,
) -> Result<ObjectId, NotFound> {
    peel_to(repo, id, kind).map_err(|e| NotFound {
        error: e
            .error
            .map(|found| crate::messages::peel_mismatch(spec, kind.name(), &found)),
    })
}

fn parse_number(s: &str) -> Option<(usize, &str)> {
    let end = s
        .bytes()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(s.len());
    if end == 0 {
        return Some((1, s));
    }
    Some((s[..end].parse().ok()?, &s[end..]))
}

/// Resolve a revision without `:<path>`.
fn resolve_rev(repo: &Repo, spec: &str) -> Result<ObjectId, NotFound> {
    let split = spec.find(['^', '~']).unwrap_or(spec.len());
    let (base, mut ops) = spec.split_at(split);
    if base.is_empty() {
        return Err(NotFound::default());
    }
    let mut id = base_object(repo, base)?;
    while !ops.is_empty() {
        if let Some(rest) = ops.strip_prefix("^{") {
            let close = rest.find('}').ok_or_else(NotFound::default)?;
            let what = &rest[..close];
            ops = &rest[close + 1..];
            id = match what {
                "" => peel_tags(repo, id)?.0,
                "object" => {
                    read_object(repo, &id).ok_or_else(NotFound::default)?;
                    id
                }
                "tree" => peel_spec(repo, spec, id, ObjectKind::Tree)?,
                "commit" => peel_spec(repo, spec, id, ObjectKind::Commit)?,
                "blob" => peel_spec(repo, spec, id, ObjectKind::Blob)?,
                "tag" => peel_spec(repo, spec, id, ObjectKind::Tag)?,
                _ => return Err(NotFound::default()),
            };
        } else if let Some(rest) = ops.strip_prefix('^') {
            let (n, rest) = parse_number(rest).ok_or_else(NotFound::default)?;
            ops = rest;
            if n == 0 {
                id = peel_to(repo, id, ObjectKind::Commit)?;
            } else {
                let parents = commit_of(repo, id)?
                    .parents()
                    .map_err(|_| NotFound::default())?;
                id = *parents.get(n - 1).ok_or_else(NotFound::default)?;
            }
        } else if let Some(rest) = ops.strip_prefix('~') {
            let (n, rest) = parse_number(rest).ok_or_else(NotFound::default)?;
            ops = rest;
            if n == 0 {
                id = peel_to(repo, id, ObjectKind::Commit)?;
            }
            for _ in 0..n {
                let parents = commit_of(repo, id)?
                    .parents()
                    .map_err(|_| NotFound::default())?;
                id = *parents.first().ok_or_else(NotFound::default)?;
            }
        } else {
            return Err(NotFound::default());
        }
    }
    Ok(id)
}

/// Normalise `<rev>:<path>`'s path: `./` and `../` are relative to the
/// current directory (`prefix`), anything else to the top.
fn tree_path(path: &str, prefix: &[u8]) -> Option<Vec<u8>> {
    let relative = path == "." || path == ".." || path.starts_with("./") || path.starts_with("../");
    let mut parts: Vec<&[u8]> = Vec::new();
    if relative {
        parts.extend(prefix.split(|&b| b == b'/').filter(|c| !c.is_empty()));
    }
    for c in path.as_bytes().split(|&b| b == b'/') {
        match c {
            b"" | b"." => {}
            b".." => {
                parts.pop()?;
            }
            c => parts.push(c),
        }
    }
    Some(parts.join(&b'/'))
}

/// Find `path` (relative to the top) below the tree `tree`.
pub fn lookup_path(repo: &Repo, tree: ObjectId, path: &[u8]) -> Option<ObjectId> {
    let mut id = tree;
    if path.is_empty() {
        return Some(id);
    }
    for comp in path.split(|&b| b == b'/') {
        let Some(Object::Tree(t)) = read_object(repo, &id) else {
            return None;
        };
        let t: Tree = t;
        id = t.entries.iter().find(|e| e.name == comp)?.oid;
    }
    Some(id)
}

/// Resolve an object name as `git rev-parse --verify <name>` would, for the
/// supported subset.
pub fn resolve(repo: &Repo, spec: &str) -> Result<ObjectId, NotFound> {
    if spec.starts_with(':') {
        // Index lookups (`:<path>`, `:<n>:<path>`) and `:/<text>`.
        return Err(NotFound::default());
    }
    match spec.split_once(':') {
        Some((rev, path)) => {
            let id = resolve_rev(repo, rev)?;
            let tree = peel_to(repo, id, ObjectKind::Tree)?;
            let path = tree_path(path, &repo.prefix()).ok_or_else(NotFound::default)?;
            lookup_path(repo, tree, &path).ok_or_else(NotFound::default)
        }
        None => resolve_rev(repo, spec),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_paths() {
        assert_eq!(tree_path("a/b", b"x/").unwrap(), b"a/b");
        assert_eq!(tree_path("./b", b"x/").unwrap(), b"x/b");
        assert_eq!(tree_path("../b", b"x/y/").unwrap(), b"x/b");
        assert_eq!(tree_path("", b"x/").unwrap(), b"");
        assert!(tree_path("../b", b"").is_none());
    }
}
