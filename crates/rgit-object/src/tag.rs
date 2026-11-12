// SPDX-License-Identifier: MIT

//! Annotated tag objects.

use rgit_hash::{HashKind, ObjectId};

use crate::commit::parse_id;
use crate::headers::{Header, HeaderBlock};
use crate::ident::{Signature, SignatureRef};
use crate::{Error, ObjectKind};

/// Lines that start an embedded signature (see `gitformat-signature`).
pub const SIGNATURE_BEGIN_MARKERS: [&[u8]; 4] = [
    b"-----BEGIN PGP SIGNATURE-----",
    b"-----BEGIN PGP MESSAGE-----",
    b"-----BEGIN SSH SIGNATURE-----",
    b"-----BEGIN SIGNED MESSAGE-----",
];

/// An annotated tag object.
///
/// Stored as header block plus message, so every tag round-trips byte for
/// byte, including tags without `tagger` (written by old git) and tags with
/// extra headers. The standard layout is `object`, `type`, `tag`, `tagger`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Tag {
    hash: HashKind,
    block: HeaderBlock,
}

impl Tag {
    /// Parse tag content (without the object header). Only the overall
    /// structure is checked; see [`crate::fsck::check_tag`] for validation.
    pub fn parse(hash: HashKind, data: &[u8]) -> Result<Tag, Error> {
        Ok(Tag {
            hash,
            block: HeaderBlock::parse(data)?,
        })
    }

    /// Build a tag in the canonical layout. `message` should end in a
    /// newline and may carry a signature at its end.
    pub fn new(
        object: ObjectId,
        kind: ObjectKind,
        name: impl Into<Vec<u8>>,
        tagger: Option<&Signature>,
        message: impl Into<Vec<u8>>,
    ) -> Tag {
        let mut headers = vec![
            Header::new("object", object.to_hex()),
            Header::new("type", kind.name()),
            Header::new("tag", name),
        ];
        if let Some(t) = tagger {
            headers.push(Header::new("tagger", t.to_bytes()));
        }
        Tag {
            hash: object.kind(),
            block: HeaderBlock {
                headers,
                message: Some(message.into()),
            },
        }
    }

    /// The hash kind used for the ids in this tag.
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

    /// The tagged object: the first header must be a well-formed `object` line.
    pub fn object(&self) -> Result<ObjectId, Error> {
        match self.block.headers.first() {
            Some(h) if h.name == b"object" => parse_id(self.hash, &h.value, "object"),
            _ => Err(Error::MissingHeader("object")),
        }
    }

    /// The type of the tagged object (the `type` header).
    pub fn target_kind(&self) -> Result<ObjectKind, Error> {
        let v = self
            .block
            .get(b"type")
            .ok_or(Error::MissingHeader("type"))?;
        ObjectKind::from_name(v).ok_or_else(|| Error::UnknownKind(v.to_vec()))
    }

    /// The tag name (the `tag` header).
    pub fn name(&self) -> Option<&[u8]> {
        self.block.get(b"tag")
    }

    /// The raw `tagger` value; old tags have none.
    pub fn tagger_raw(&self) -> Option<&[u8]> {
        self.block.get(b"tagger")
    }

    /// The `tagger`, split into its parts.
    pub fn tagger(&self) -> Option<SignatureRef<'_>> {
        self.tagger_raw().and_then(SignatureRef::parse)
    }

    /// The message including any trailing signature.
    pub fn message(&self) -> &[u8] {
        self.block.message()
    }

    /// The message split into the part before the signature and the
    /// signature itself (from its `-----BEGIN` line to the end).
    pub fn message_and_signature(&self) -> (&[u8], Option<&[u8]>) {
        split_signature(self.message())
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
}

/// Split `message` at the start of the last line that begins a signature
/// (one of [`SIGNATURE_BEGIN_MARKERS`]).
pub fn split_signature(message: &[u8]) -> (&[u8], Option<&[u8]>) {
    let mut found = None;
    let mut start = 0;
    while start < message.len() {
        let line = &message[start..];
        if SIGNATURE_BEGIN_MARKERS.iter().any(|m| line.starts_with(m)) {
            found = Some(start);
        }
        match line.iter().position(|&b| b == b'\n') {
            Some(nl) => start += nl + 1,
            None => break,
        }
    }
    match found {
        Some(at) => (&message[..at], Some(&message[at..])),
        None => (message, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_tag() {
        let data = b"object 04b871796dc0420f8e7561a895b52484b701d51a\ntype commit\ntag signedtag\n\
                     tagger C O Mitter <committer@example.com> 1465981006 +0000\n\nsigned tag\n\n\
                     body\n-----BEGIN PGP SIGNATURE-----\nabc\n-----END PGP SIGNATURE-----\n";
        let t = Tag::parse(HashKind::Sha1, data).unwrap();
        assert_eq!(t.serialize(), data);
        assert_eq!(t.target_kind().unwrap(), ObjectKind::Commit);
        assert_eq!(t.name().unwrap(), b"signedtag");
        let (msg, sig) = t.message_and_signature();
        assert_eq!(msg, b"signed tag\n\nbody\n");
        assert!(sig.unwrap().starts_with(b"-----BEGIN PGP"));
    }

    #[test]
    fn build() {
        let id = HashKind::Sha1.empty_tree();
        let t = Tag::new(id, ObjectKind::Tree, "v1", None, "m\n");
        assert_eq!(
            t.serialize(),
            format!("object {id}\ntype tree\ntag v1\n\nm\n").as_bytes()
        );
        assert_eq!(t.object().unwrap(), id);
        assert!(t.tagger().is_none());
    }
}
