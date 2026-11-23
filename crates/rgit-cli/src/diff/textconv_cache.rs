// SPDX-License-Identifier: MIT
//! The textconv cache (`diff.<driver>.cachetextconv`, git-config(1)):
//! the output of a driver's textconv filter for blobs, kept as notes in
//! `refs/notes/textconv/<driver>`.
//!
//! Layout as the `git` binary writes it (observed, not specified in the
//! documentation): a root commit without parents whose message is the
//! textconv command line (no trailing newline) and whose tree maps the
//! hex id of each converted blob to a blob with the converted text
//! (possibly fanned out into `xx/` directories). A commit whose message
//! differs from the current command is stale and ignored. Each update
//! (every new entry) writes a new root commit with the reflog message
//! `update notes cache`.
//! Only blobs from the object database are cached, never work-tree files.

use std::collections::BTreeMap;

use rgit_hash::ObjectId;
use rgit_index::{AddOptions, Entry, Index, Mode, Stage};
use rgit_object::{Object, ObjectKind};
use rgit_refs::{RefStore, RefUpdate, Transaction};

use crate::refs_support::{self, Objects, UpdateHooks};
use crate::repository::Repo;

/// The cache of one driver.
pub struct Cache {
    ref_name: String,
    command: String,
    /// Hex id of the source blob -> blob with the converted text.
    entries: BTreeMap<Vec<u8>, ObjectId>,
}

impl Cache {
    /// Reads the cache of `driver` that is valid for `command`.
    pub fn open(repo: &Repo, driver: &str, command: &str) -> Cache {
        let ref_name = format!("refs/notes/textconv/{driver}");
        let mut entries = BTreeMap::new();
        let store = refs_support::ref_store(&repo.setup, &repo.config);
        if let Ok(Some(id)) = store.resolve_oid(&ref_name) {
            if let Some(Object::Commit(c)) = repo.read_object(&id) {
                if c.message() == command.as_bytes() {
                    if let Ok(tree) = c.tree() {
                        read_tree(repo, &tree, Vec::new(), &mut entries, 0);
                    }
                }
            }
        }
        Cache {
            ref_name,
            command: command.to_owned(),
            entries,
        }
    }

    /// The cached text of blob `oid`.
    pub fn get(&self, repo: &Repo, oid: &ObjectId) -> Option<Vec<u8>> {
        let id = self.entries.get(oid.to_hex().as_bytes())?;
        match repo.read_object(id) {
            Some(Object::Blob(b)) => Some(b.data),
            _ => None,
        }
    }

    /// Remembers the text `conv` of blob `oid` and writes the cache
    /// back right away, as git does for every new entry. Failures are
    /// silent (the cache is an optimisation only).
    pub fn put(&mut self, repo: &Repo, oid: &ObjectId, conv: &[u8]) {
        if let Ok(id) = repo.odb().write(ObjectKind::Blob, conv) {
            self.entries.insert(oid.to_hex().into_bytes(), id);
            let _ = self.write(repo);
        }
    }

    fn write(&self, repo: &Repo) -> Option<()> {
        let mut index = Index::new(repo.hash);
        let add = AddOptions {
            ok_to_replace: true,
            skip_df_check: false,
            skip_path_check: true,
        };
        for (name, id) in &self.entries {
            index
                .add(
                    Entry::new(name.clone(), Stage::MERGED, Mode(0o100644), *id),
                    add,
                )
                .ok()?;
        }
        let tree = rgit_commit::write_tree(
            repo.odb(),
            &index,
            &rgit_commit::WriteTreeOptions::default(),
        )
        .ok()?;
        let committer = ident(repo, rgit_ident::ident::Role::Committer)?;
        let author = ident(repo, rgit_ident::ident::Role::Author)?;
        let new = rgit_commit::NewCommit {
            tree,
            parents: &[],
            author: &author,
            committer: &committer,
            encoding: None,
            message: self.command.as_bytes(),
        };
        let commit = rgit_commit::create_commit(repo.odb(), &new).ok()?;
        let objects = Objects::open(&repo.setup);
        let store = refs_support::ref_store(&repo.setup, &repo.config);
        let mut hooks = UpdateHooks::new(&repo.setup, &repo.config, &objects);
        let update = RefUpdate::update(self.ref_name.as_str(), commit, None)
            .message(crate::messages::diff::NOTES_CACHE_REFLOG);
        let mut tx = Transaction::new(&store);
        tx.push(update).and_then(|()| tx.commit(&mut hooks)).ok()
    }
}

/// Collects the notes of `tree`, joining fanout directories.
fn read_tree(
    repo: &Repo,
    tree: &ObjectId,
    prefix: Vec<u8>,
    out: &mut BTreeMap<Vec<u8>, ObjectId>,
    depth: usize,
) {
    let Some(Object::Tree(t)) = repo.read_object(tree) else {
        return;
    };
    for e in t.entries {
        let mut name = prefix.clone();
        name.extend_from_slice(&e.name);
        if e.mode.is_tree() {
            if depth < 40 {
                read_tree(repo, &e.oid, name, out, depth + 1);
            }
        } else {
            out.insert(name, e.oid);
        }
    }
}

/// The identity for the cache commit, without complaints.
fn ident(repo: &Repo, role: rgit_ident::ident::Role) -> Option<rgit_object::Signature> {
    use rgit_ident::ident::{IdentConfig, Resolver, Strictness};
    let env = rgit_config::SystemEnv::default();
    let icfg = IdentConfig::from_config(&repo.config).ok()?;
    let ident = Resolver::new(&icfg, &env)
        .ident(role, Strictness::Lenient)
        .ok()?;
    let raw = ident.to_bstring();
    rgit_object::SignatureRef::parse(&raw).and_then(|s| s.to_owned())
}
