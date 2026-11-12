// SPDX-License-Identifier: MIT
//! A set-up repository for plumbing that lists trees and the index:
//! [`crate::repo_objects::Repo`] (configuration, object database with
//! packs and alternates) plus revision lookup, abbreviations and output
//! settings.

use std::cell::OnceCell;
use std::ops::Deref;

use rgit_hash::{ObjectId, Prefix};
use rgit_object::{Object, ObjectKind};
use rgit_odb::ObjectDatabase;

use crate::repo::{self, Mode};
use crate::repo_objects;
use crate::revision::Resolver;
use crate::{Context, Die};

/// Shortest abbreviation git prints.
pub const MIN_ABBREV: usize = 4;

/// A repository opened by a command.
pub struct Repo {
    inner: repo_objects::Repo,
    auto_abbrev: OnceCell<usize>,
}

impl Deref for Repo {
    type Target = repo_objects::Repo;

    fn deref(&self) -> &repo_objects::Repo {
        &self.inner
    }
}

impl Repo {
    /// Set up the repository and open its configuration and objects.
    pub fn open(ctx: &Context, mode: Mode) -> Result<Repo, Die> {
        let setup = repo::setup(ctx, mode)?;
        Ok(Repo {
            inner: repo_objects::Repo::open(ctx, setup)?,
            auto_abbrev: OnceCell::new(),
        })
    }

    /// The object database (loose objects, packs, alternates).
    pub fn odb(&self) -> &ObjectDatabase {
        &self.inner.odb
    }

    /// Resolve an object name (`gitrevisions(7)`); `None` when it does not
    /// name an object. Errors git reports on the way (`expected tree type,
    /// ...`) are printed.
    pub fn resolve(&mut self, name: &str) -> Option<ObjectId> {
        Resolver::new(&mut self.inner)
            .resolve(name)
            .ok()
            .map(|r| r.oid)
    }

    /// Peel tags and commits down to an object of type `kind`.
    pub fn peel_to(&mut self, id: ObjectId, kind: ObjectKind) -> Option<ObjectId> {
        Resolver::new(&mut self.inner).peel_to(id, kind, None)
    }

    /// Read and parse an object.
    pub fn read_object(&self, id: &ObjectId) -> Option<Object> {
        self.inner.odb.read_object(id).ok().flatten()
    }

    /// The current directory relative to the top of the work tree, with a
    /// trailing `/`, or empty.
    pub fn prefix(&self) -> Vec<u8> {
        self.setup
            .prefix
            .as_ref()
            .map(|p| p.as_encoded_bytes().to_vec())
            .unwrap_or_default()
    }

    /// `core.quotePath` (default true).
    pub fn quote_path(&self) -> bool {
        self.config
            .bool("core.quotepath")
            .ok()
            .flatten()
            .unwrap_or(true)
    }

    /// The default abbreviation length: `core.abbrev`, else computed from
    /// the number of objects.
    pub fn default_abbrev(&self) -> usize {
        *self
            .auto_abbrev
            .get_or_init(|| crate::revision::default_abbrev(&self.inner))
    }

    /// `id` abbreviated to at least `len` digits (full when `len` is 0 or
    /// at least the full length), unique among all objects.
    pub fn abbrev(&self, id: &ObjectId, len: usize) -> String {
        let hex = id.to_hex();
        if len == 0 || len >= hex.len() {
            return hex;
        }
        let mut len = len.max(MIN_ABBREV);
        while len < hex.len() {
            let Ok(prefix) = Prefix::new(*id, len) else {
                break;
            };
            let mut other = false;
            for store in self.odb().stores() {
                let _ = store.for_each_prefix_match(&prefix, &mut |o| other |= o != *id);
                if other {
                    break;
                }
            }
            if !other {
                break;
            }
            len += 1;
        }
        hex[..len].to_string()
    }
}
