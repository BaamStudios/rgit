// SPDX-License-Identifier: MIT

//! Streaming hashing of objects and file contents.

use std::io;

use sha1_checked::CollisionResult;
use sha2::Digest as _;

use crate::{CollisionError, HashKind, ObjectId};

/// An incremental hasher for one [`HashKind`].
///
/// SHA-1 always runs with collision detection (the published
/// counter-cryptanalysis of Stevens and Shumow, via the `sha1-checked`
/// crate); [`Hasher::finalize`] fails with [`CollisionError`] on input
/// that carries a collision attack. SHA-256 cannot fail.
///
/// Use it for object ids (see [`object_hasher`]) and for the trailing
/// checksums of packs, indexes and the index file. Implements
/// [`io::Write`], so `io::copy` into it works.
#[derive(Clone)]
pub struct Hasher(Inner);

#[derive(Clone)]
enum Inner {
    Sha1(Box<sha1_checked::Sha1>),
    Sha256(sha2::Sha256),
}

impl Hasher {
    /// A fresh hasher for `kind`.
    pub fn new(kind: HashKind) -> Hasher {
        Hasher(match kind {
            HashKind::Sha1 => Inner::Sha1(Box::new(
                // No "safe hash": on detection we refuse the input anyway and
                // report the plain SHA-1 digest, like git does.
                sha1_checked::Sha1::builder().safe_hash(false).build(),
            )),
            HashKind::Sha256 => Inner::Sha256(sha2::Sha256::new()),
        })
    }

    /// The kind this hasher produces.
    pub fn kind(&self) -> HashKind {
        match self.0 {
            Inner::Sha1(_) => HashKind::Sha1,
            Inner::Sha256(_) => HashKind::Sha256,
        }
    }

    /// Feed more data.
    pub fn update(&mut self, data: &[u8]) {
        match &mut self.0 {
            Inner::Sha1(h) => sha1_checked::Digest::update(h.as_mut(), data),
            Inner::Sha256(h) => h.update(data),
        }
    }

    /// Finish and return the digest as an [`ObjectId`] of [`Hasher::kind`].
    pub fn finalize(self) -> Result<ObjectId, CollisionError> {
        match self.0 {
            Inner::Sha1(h) => match h.try_finalize() {
                CollisionResult::Ok(out) => Ok(ObjectId::Sha1(out.into())),
                CollisionResult::Collision(out) | CollisionResult::Mitigated(out) => {
                    Err(CollisionError {
                        id: ObjectId::Sha1(out.into()),
                    })
                }
            },
            Inner::Sha256(h) => Ok(ObjectId::Sha256(h.finalize().into())),
        }
    }
}

impl std::fmt::Debug for Hasher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Hasher").field(&self.kind()).finish()
    }
}

impl io::Write for Hasher {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Hash `data` in one go (no object header).
pub fn hash(kind: HashKind, data: &[u8]) -> Result<ObjectId, CollisionError> {
    let mut h = Hasher::new(kind);
    h.update(data);
    h.finalize()
}

/// A hasher already fed with the object header `<type> <len>\0`.
///
/// Feed exactly `len` bytes of content afterwards, then call
/// [`Hasher::finalize`] to get the object id. `object_type` is the type
/// name as stored in the header (`"blob"`, `"tree"`, `"commit"`, `"tag"`);
/// it is not validated here.
pub fn object_hasher(kind: HashKind, object_type: &str, len: u64) -> Hasher {
    let mut h = Hasher::new(kind);
    h.update(object_type.as_bytes());
    h.update(b" ");
    h.update(len.to_string().as_bytes());
    h.update(b"\0");
    h
}

/// The id of an object of `object_type` with content `data`: the hash of
/// `<type> <len>\0<data>`, as `git hash-object -t <type>` computes it
/// (without any content filters).
pub fn hash_object(
    kind: HashKind,
    object_type: &str,
    data: &[u8],
) -> Result<ObjectId, CollisionError> {
    let mut h = object_hasher(kind, object_type, data.len() as u64);
    h.update(data);
    h.finalize()
}
