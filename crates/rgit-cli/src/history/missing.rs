// SPDX-License-Identifier: MIT
//! Object filters and missing objects in `rev-list --objects`
//! (`--filter`, `--filter-print-omitted`, `--filter-provided-objects`,
//! `--missing`, `--exclude-promisor-objects`; Documentation/
//! rev-list-options.adoc, Documentation/technical/partial-clone.adoc).
//!
//! [`Tracker`] holds what to do with a missing object and collects the
//! missing objects that are passed over; the commit walk and the tree
//! lister ([`Trees`]) share it.

use std::cell::RefCell;
use std::collections::HashSet;
use std::io::Write as _;
use std::rc::Rc;

use rgit_hash::ObjectId;
use rgit_object::{ObjectKind, TreeIter};
use rgit_pack::write::{FilterState, SparsePatterns};
use rgit_revwalk::revision::ObjectLookup;
use rgit_revwalk::walk::objects::{ItemKind, TreeItem, TreeSource};

use super::Repo;
use super::options::{Missing, Options};
use crate::Die;
use crate::messages::rev_list as msg;

/// What happens to missing objects, and those passed over so far.
pub struct Tracker {
    action: Missing,
    /// The promisor objects (`--missing=allow-promisor`,
    /// `--exclude-promisor-objects`).
    promisor: Option<HashSet<ObjectId>>,
    /// The objects in promisor packs, where `--exclude-promisor-objects`
    /// stops (objects they only refer to are listed when present).
    in_promisor_packs: Option<HashSet<ObjectId>>,
    exclude_promisor: bool,
    /// Missing objects passed over, in the order found: id, type, path.
    found: Vec<(ObjectId, &'static str, Option<Vec<u8>>)>,
    seen: HashSet<ObjectId>,
    /// Commits the walk passes over (missing or promisor commits).
    hidden: HashSet<ObjectId>,
}

/// The tracker as the walk and the lister share it.
pub type Shared = Rc<RefCell<Tracker>>;

impl Tracker {
    /// The tracker for the options of a `rev-list` run.
    pub fn new(repo: &Repo<'_>, opts: &Options) -> Shared {
        let action = opts.missing.unwrap_or(Missing::Error);
        let promisor = (opts.exclude_promisor || action == Missing::AllowPromisor)
            .then(|| repo.objects.promisor_objects());
        Rc::new(RefCell::new(Tracker {
            action,
            promisor,
            in_promisor_packs: opts
                .exclude_promisor
                .then(|| repo.objects.promisor_pack_objects()),
            exclude_promisor: opts.exclude_promisor,
            found: Vec::new(),
            seen: HashSet::new(),
            hidden: HashSet::new(),
        }))
    }

    /// Whether `id` is a promisor object to leave out
    /// (`--exclude-promisor-objects`).
    pub fn excludes(&self, id: &ObjectId) -> bool {
        self.in_promisor_packs
            .as_ref()
            .is_some_and(|p| p.contains(id))
    }

    /// The promisor objects to leave out, if any.
    pub fn excluded_objects(&self) -> Vec<ObjectId> {
        self.in_promisor_packs
            .as_ref()
            .map(|p| p.iter().copied().collect())
            .unwrap_or_default()
    }

    /// A missing object: `Ok(())` when it is passed over (and noted), the
    /// error to die with otherwise.
    pub fn missing(
        &mut self,
        id: &ObjectId,
        kind: &'static str,
        path: Option<&[u8]>,
    ) -> Result<(), Die> {
        let promised = || self.promisor.as_ref().is_some_and(|p| p.contains(id));
        match self.action {
            Missing::Error if self.exclude_promisor && promised() => {}
            Missing::Error if kind == "tree" => {
                return Err(Die::Fatal(msg::bad_tree_object(&id.to_hex())));
            }
            Missing::Error => {
                return Err(Die::Fatal(msg::missing_object(kind, &id.to_hex())));
            }
            Missing::AllowPromisor if !promised() => {
                return Err(Die::Fatal(msg::unexpected_missing_object(
                    kind,
                    &id.to_hex(),
                )));
            }
            _ => {}
        }
        if self.seen.insert(*id) {
            self.found.push((*id, kind, path.map(<[u8]>::to_vec)));
        }
        Ok(())
    }

    /// A commit the walk passes over.
    pub fn hide(&mut self, id: &ObjectId) {
        self.hidden.insert(*id);
    }

    /// Whether the walk passed over commit `id`.
    pub fn is_hidden(&self, id: &ObjectId) -> bool {
        self.hidden.contains(id)
    }

    /// Whether a missing commit is passed over rather than fatal.
    pub fn tolerates_commits(&self, id: &ObjectId) -> bool {
        match self.action {
            Missing::Error => {
                self.exclude_promisor && self.promisor.as_ref().is_some_and(|p| p.contains(id))
            }
            Missing::AllowPromisor => self.promisor.as_ref().is_some_and(|p| p.contains(id)),
            _ => true,
        }
    }

    /// The lines for `--missing=print` / `print-info` (`?` dropped with
    /// `--missing-only`). With `-z` (`nul`) every field ends in NUL and is
    /// unquoted: `<id> NUL missing=yes NUL [path=<p> NUL] [type=<t> NUL]`.
    pub fn report(&self, only: bool, nul: bool) -> Vec<u8> {
        let mut out = Vec::new();
        if !matches!(self.action, Missing::Print | Missing::PrintInfo) {
            return out;
        }
        for (id, kind, path) in &self.found {
            if nul {
                out.extend_from_slice(id.to_hex().as_bytes());
                out.extend_from_slice(b"\0missing=yes\0");
                if self.action == Missing::PrintInfo {
                    if let Some(p) = path.as_deref().filter(|p| !p.is_empty()) {
                        out.extend_from_slice(b"path=");
                        out.extend_from_slice(p);
                        out.push(0);
                    }
                    out.extend_from_slice(b"type=");
                    out.extend_from_slice(kind.as_bytes());
                    out.push(0);
                }
                continue;
            }
            if !only {
                out.push(b'?');
            }
            out.extend_from_slice(id.to_hex().as_bytes());
            if self.action == Missing::PrintInfo {
                if let Some(p) = path.as_deref().filter(|p| !p.is_empty()) {
                    out.extend_from_slice(b" path=");
                    quote_info(&mut out, p);
                }
                out.extend_from_slice(b" type=");
                out.extend_from_slice(kind.as_bytes());
            }
            out.push(b'\n');
        }
        out
    }
}

/// A path in `print-info`: quoted in C style when it has a space or a
/// byte that needs escaping.
fn quote_info(out: &mut Vec<u8>, path: &[u8]) {
    if path.contains(&b' ') && !path.iter().any(|&c| c < 0x20 || c == b'"' || c == b'\\') {
        out.push(b'"');
        out.extend_from_slice(path);
        out.push(b'"');
    } else {
        crate::quote::write_quoted(out, path, true);
    }
}

/// Trees and blobs for `--objects`, with missing objects handled by the
/// tracker.
pub struct Trees<'r, 'a> {
    pub repo: &'r Repo<'a>,
    pub missing: Shared,
}

impl Trees<'_, '_> {
    fn bad(id: &ObjectId) -> Die {
        Die::Fatal(msg::bad_object(&id.to_hex()))
    }

    fn entries(&self, id: &ObjectId, data: &[u8]) -> Result<Vec<TreeItem>, Die> {
        let mut out = Vec::new();
        for entry in TreeIter::new(self.repo.objects.hash_kind(), data) {
            let entry = entry.map_err(|_| Self::bad(id))?;
            let kind = match entry.mode_bits & 0o170000 {
                0o040000 => ItemKind::Tree,
                0o160000 => ItemKind::Gitlink,
                _ => ItemKind::Blob,
            };
            out.push(TreeItem {
                kind,
                name: entry.name.to_vec(),
                id: entry.oid,
            });
        }
        Ok(out)
    }
}

impl TreeSource for Trees<'_, '_> {
    type Error = Die;

    fn tree(&mut self, id: &ObjectId) -> Result<Vec<TreeItem>, Die> {
        match self.repo.objects.read(id).ok().flatten() {
            Some((ObjectKind::Tree, data)) => self.entries(id, &data),
            _ => Err(Self::bad(id)),
        }
    }

    fn tree_or_missing(
        &mut self,
        id: &ObjectId,
        path: &[u8],
    ) -> Result<Option<Vec<TreeItem>>, Die> {
        match self.repo.objects.read(id) {
            Ok(Some((ObjectKind::Tree, data))) => self.entries(id, &data).map(Some),
            Ok(None) => {
                self.missing.borrow_mut().missing(id, "tree", Some(path))?;
                Ok(None)
            }
            _ => Err(Die::Fatal(msg::bad_tree_object(&id.to_hex()))),
        }
    }

    fn peek_blob(&mut self, id: &ObjectId) -> Option<Option<u64>> {
        self.repo.objects.size_here(id).map(Some)
    }

    fn blob(
        &mut self,
        id: &ObjectId,
        path: &[u8],
        listed: bool,
    ) -> Result<Option<Option<u64>>, Die> {
        if !listed {
            return Ok(Some(None));
        }
        match self.repo.objects.size(id) {
            Some(size) => Ok(Some(Some(size))),
            None => {
                self.missing.borrow_mut().missing(id, "blob", Some(path))?;
                Ok(None)
            }
        }
    }
}

/// Sparse-checkout patterns read from a blob (`--filter=sparse:oid=`).
pub struct Patterns(rgit_worktree::ignore::PatternList);

impl Patterns {
    /// The patterns of `sparse:oid=<spec>`: `id` is what `spec` resolves
    /// to, `read` reads an object.
    pub fn load(
        spec: &str,
        id: Option<ObjectId>,
        read: impl FnOnce(&ObjectId) -> Option<(ObjectKind, Vec<u8>)>,
    ) -> Result<Patterns, Die> {
        let id = id.ok_or_else(|| Die::Fatal(msg::unable_to_access_sparse_blob(spec)))?;
        match read(&id) {
            Some((ObjectKind::Blob, data)) => Ok(Patterns(
                rgit_worktree::ignore::PatternList::from_bytes(&data, None, Vec::new()),
            )),
            _ => Err(Die::Fatal(msg::unable_to_parse_sparse_data(&id.to_hex()))),
        }
    }
}

impl SparsePatterns for Patterns {
    fn decide(&self, path: &[u8], is_dir: bool) -> Option<bool> {
        self.0
            .last_match(path, is_dir, false)
            .map(|p| !p.is_negative())
    }
}

/// The object filter of `--filter` (several combine), or `None`.
pub fn object_filter(repo: &Repo<'_>, opts: &Options) -> Result<Option<FilterState>, Die> {
    let Some((filter, _)) = crate::promisor::parse_filters(&opts.filters)? else {
        return Ok(None);
    };
    let mut sparse = |spec: &str| -> Result<Box<dyn SparsePatterns>, Die> {
        let id = repo.resolver.resolve(spec).ok();
        let p = Patterns::load(spec, id, |id| repo.objects.read(id).ok().flatten())?;
        Ok(Box::new(p))
    };
    FilterState::new(&filter, opts.filter_print_omitted, &mut sparse).map(Some)
}

/// Print the `~<id>` lines of `--filter-print-omitted` and the missing
/// objects of `--missing=print`, also under `--quiet`.
pub fn print_tail(omitted: &[ObjectId], tracker: &Tracker, missing_only: bool, nul: bool) {
    let mut out = Vec::new();
    if !missing_only {
        for id in omitted {
            out.push(b'~');
            out.extend_from_slice(id.to_hex().as_bytes());
            out.push(b'\n');
        }
    }
    out.extend_from_slice(&tracker.report(missing_only, nul));
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(&out);
    let _ = stdout.flush();
}
