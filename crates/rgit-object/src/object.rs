// SPDX-License-Identifier: MIT

//! A parsed object of any type.

use rgit_hash::{CollisionError, HashKind, ObjectId, hash_object};

use crate::{Commit, Error, ObjectHeader, ObjectKind, Tag, Tree};

/// A blob: uninterpreted bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Blob {
    /// The contents.
    pub data: Vec<u8>,
}

/// An object of any of the four types.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Object {
    /// A blob.
    Blob(Blob),
    /// A tree.
    Tree(Tree),
    /// A commit.
    Commit(Commit),
    /// A tag.
    Tag(Tag),
}

impl Object {
    /// Parse content of type `kind` (without the object header).
    pub fn parse(kind: ObjectKind, hash: HashKind, data: &[u8]) -> Result<Object, Error> {
        Ok(match kind {
            ObjectKind::Blob => Object::Blob(Blob {
                data: data.to_vec(),
            }),
            ObjectKind::Tree => Object::Tree(Tree::parse(hash, data)?),
            ObjectKind::Commit => Object::Commit(Commit::parse(hash, data)?),
            ObjectKind::Tag => Object::Tag(Tag::parse(hash, data)?),
        })
    }

    /// Parse a full object including its `<type> <size>\0` header; the size
    /// must match the content length.
    pub fn parse_with_header(hash: HashKind, data: &[u8]) -> Result<Object, Error> {
        let (h, n) = ObjectHeader::parse(data)?;
        let content = &data[n..];
        if h.size != content.len() as u64 {
            return Err(Error::SizeMismatch {
                expected: h.size,
                actual: content.len() as u64,
            });
        }
        Object::parse(h.kind, hash, content)
    }

    /// The object type.
    pub fn kind(&self) -> ObjectKind {
        match self {
            Object::Blob(_) => ObjectKind::Blob,
            Object::Tree(_) => ObjectKind::Tree,
            Object::Commit(_) => ObjectKind::Commit,
            Object::Tag(_) => ObjectKind::Tag,
        }
    }

    /// Append the serialised content (without header) to `out`.
    pub fn write_to(&self, out: &mut Vec<u8>) {
        match self {
            Object::Blob(b) => out.extend_from_slice(&b.data),
            Object::Tree(t) => t.write_to(out),
            Object::Commit(c) => c.write_to(out),
            Object::Tag(t) => t.write_to(out),
        }
    }

    /// The serialised content (without header).
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_to(&mut out);
        out
    }

    /// The serialised object with its header, as hashed and stored loose.
    pub fn serialize_with_header(&self) -> Vec<u8> {
        let content = self.serialize();
        let mut out = ObjectHeader {
            kind: self.kind(),
            size: content.len() as u64,
        }
        .to_bytes();
        out.extend_from_slice(&content);
        out
    }

    /// The object id of the serialised object.
    pub fn id(&self, hash: HashKind) -> Result<ObjectId, CollisionError> {
        hash_object(hash, self.kind().name(), &self.serialize())
    }
}

impl From<Blob> for Object {
    fn from(b: Blob) -> Object {
        Object::Blob(b)
    }
}

impl From<Tree> for Object {
    fn from(t: Tree) -> Object {
        Object::Tree(t)
    }
}

impl From<Commit> for Object {
    fn from(c: Commit) -> Object {
        Object::Commit(c)
    }
}

impl From<Tag> for Object {
    fn from(t: Tag) -> Object {
        Object::Tag(t)
    }
}
