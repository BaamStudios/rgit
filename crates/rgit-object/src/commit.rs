// SPDX-License-Identifier: MIT

//! Commit objects.

use rgit_hash::{HashKind, ObjectId};

use crate::headers::{Header, HeaderBlock};
use crate::ident::{Signature, SignatureRef};
use crate::{Error, ObjectKind};

/// A commit object.
///
/// The commit is stored as its header block plus message, so every commit
/// round-trips byte for byte (header order, unknown headers, multi-line
/// values, missing or duplicated fields, non-UTF-8 text). The accessors
/// interpret the standard headers the way git lays them out:
/// `tree`, `parent`*, `author`, `committer`, then `encoding`, `mergetag`,
/// `gpgsig`, `gpgsig-sha256` and any other headers.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Commit {
    hash: HashKind,
    block: HeaderBlock,
}

impl Commit {
    /// Parse commit content (without the object header). Only the overall
    /// structure is checked; see [`crate::fsck::check_commit`] for validation.
    pub fn parse(hash: HashKind, data: &[u8]) -> Result<Commit, Error> {
        Ok(Commit {
            hash,
            block: HeaderBlock::parse(data)?,
        })
    }

    /// Build a commit in git's canonical layout.
    ///
    /// `extra` headers (e.g. `mergetag`, `gpgsig`) follow `encoding`.
    pub fn new(
        tree: ObjectId,
        parents: &[ObjectId],
        author: &Signature,
        committer: &Signature,
        encoding: Option<&[u8]>,
        extra: Vec<Header>,
        message: impl Into<Vec<u8>>,
    ) -> Commit {
        let mut headers = Vec::with_capacity(4 + parents.len() + extra.len());
        headers.push(Header::new("tree", tree.to_hex()));
        for p in parents {
            headers.push(Header::new("parent", p.to_hex()));
        }
        headers.push(Header::new("author", author.to_bytes()));
        headers.push(Header::new("committer", committer.to_bytes()));
        if let Some(enc) = encoding {
            headers.push(Header::new("encoding", enc));
        }
        headers.extend(extra);
        Commit {
            hash: tree.kind(),
            block: HeaderBlock {
                headers,
                message: Some(message.into()),
            },
        }
    }

    /// The hash kind used for the ids in this commit.
    pub fn hash_kind(&self) -> HashKind {
        self.hash
    }

    /// The headers and message.
    pub fn block(&self) -> &HeaderBlock {
        &self.block
    }

    /// Mutable access to the headers and message.
    pub fn block_mut(&mut self) -> &mut HeaderBlock {
        &mut self.block
    }

    /// The headers in stored order.
    pub fn headers(&self) -> &[Header] {
        &self.block.headers
    }

    /// The `tree` id: the first header must be a well-formed `tree` line.
    pub fn tree(&self) -> Result<ObjectId, Error> {
        match self.block.headers.first() {
            Some(h) if h.name == b"tree" => parse_id(self.hash, &h.value, "tree"),
            _ => Err(Error::MissingHeader("tree")),
        }
    }

    /// The `parent` ids: the `parent` lines directly after `tree`.
    pub fn parents(&self) -> Result<Vec<ObjectId>, Error> {
        self.block
            .headers
            .iter()
            .skip(1)
            .take_while(|h| h.name == b"parent")
            .map(|h| parse_id(self.hash, &h.value, "parent"))
            .collect()
    }

    /// The raw value of the first `author` header.
    pub fn author_raw(&self) -> Option<&[u8]> {
        self.block.get(b"author")
    }

    /// The first `author`, split into its parts.
    pub fn author(&self) -> Option<SignatureRef<'_>> {
        self.author_raw().and_then(SignatureRef::parse)
    }

    /// The raw value of the first `committer` header.
    pub fn committer_raw(&self) -> Option<&[u8]> {
        self.block.get(b"committer")
    }

    /// The first `committer`, split into its parts.
    pub fn committer(&self) -> Option<SignatureRef<'_>> {
        self.committer_raw().and_then(SignatureRef::parse)
    }

    /// The `encoding` header (the character set of the message), if any.
    pub fn encoding(&self) -> Option<&[u8]> {
        self.block.get(b"encoding")
    }

    /// The `gpgsig` header (signature over the SHA-1 form of the commit).
    pub fn gpgsig(&self) -> Option<&[u8]> {
        self.block.get(b"gpgsig")
    }

    /// The `gpgsig-sha256` header (signature over the SHA-256 form).
    pub fn gpgsig_sha256(&self) -> Option<&[u8]> {
        self.block.get(b"gpgsig-sha256")
    }

    /// The embedded tag objects of `mergetag` headers.
    pub fn mergetags(&self) -> impl Iterator<Item = &[u8]> {
        self.block.get_all(b"mergetag")
    }

    /// Headers other than `tree`, `parent`, `author`, `committer` and `encoding`.
    pub fn extra_headers(&self) -> impl Iterator<Item = &Header> {
        self.block.headers.iter().filter(|h| {
            !matches!(
                h.name.as_slice(),
                b"tree" | b"parent" | b"author" | b"committer" | b"encoding"
            )
        })
    }

    /// The message (everything after the empty line), empty if none.
    pub fn message(&self) -> &[u8] {
        self.block.message()
    }

    /// The commit as it was before signing: the serialised commit without
    /// its `gpgsig` and `gpgsig-sha256` headers. This is the payload a
    /// commit signature is made over.
    pub fn payload_without_signatures(&self) -> Vec<u8> {
        let stripped = HeaderBlock {
            headers: self
                .block
                .headers
                .iter()
                .filter(|h| h.name != b"gpgsig" && h.name != b"gpgsig-sha256")
                .cloned()
                .collect(),
            message: self.block.message.clone(),
        };
        let mut out = Vec::new();
        stripped.write_to(&mut out);
        out
    }

    /// Append the serialised content to `out`.
    pub fn write_to(&self, out: &mut Vec<u8>) {
        self.block.write_to(out);
    }

    /// The serialised content (without the object header).
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_to(&mut out);
        out
    }

    /// Always [`ObjectKind::Commit`].
    pub fn kind(&self) -> ObjectKind {
        ObjectKind::Commit
    }
}

/// Parse a full hex id of `hash` that makes up a whole header value.
pub(crate) fn parse_id(
    hash: HashKind,
    value: &[u8],
    header: &'static str,
) -> Result<ObjectId, Error> {
    ObjectId::from_hex(hash, value).map_err(|_| Error::BadObjectId(header))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ident::Time;

    #[test]
    fn build_and_read() {
        let tree = HashKind::Sha1.empty_tree();
        let sig = Signature {
            name: b"A U Thor".to_vec(),
            email: b"author@example.com".to_vec(),
            time: Time::new(1112911993, -420),
        };
        let c = Commit::new(tree, &[tree], &sig, &sig, None, vec![], "msg\n");
        let data = c.serialize();
        assert_eq!(
            data,
            format!(
                "tree {tree}\nparent {tree}\nauthor A U Thor <author@example.com> 1112911993 -0700\n\
                 committer A U Thor <author@example.com> 1112911993 -0700\n\nmsg\n"
            )
            .as_bytes()
        );
        let p = Commit::parse(HashKind::Sha1, &data).unwrap();
        assert_eq!(p, c);
        assert_eq!(p.tree().unwrap(), tree);
        assert_eq!(p.parents().unwrap(), vec![tree]);
        assert_eq!(p.author().unwrap().to_owned().unwrap(), sig);
        assert_eq!(p.message(), b"msg\n");
    }

    #[test]
    fn signature_payload() {
        let data = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nauthor A <a> 1 +0000\n\
                     committer C <c> 1 +0000\ngpgsig -----BEGIN PGP SIGNATURE-----\n \n x\n \
                     -----END PGP SIGNATURE-----\n\nsigned\n";
        let c = Commit::parse(HashKind::Sha1, data).unwrap();
        assert_eq!(c.serialize(), data);
        assert!(c.gpgsig().unwrap().starts_with(b"-----BEGIN"));
        assert_eq!(
            c.payload_without_signatures(),
            b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nauthor A <a> 1 +0000\n\
              committer C <c> 1 +0000\n\nsigned\n"
        );
    }
}
