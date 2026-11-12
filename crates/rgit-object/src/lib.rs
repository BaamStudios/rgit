// SPDX-License-Identifier: MIT

//! Parsing and serialisation of the four git object types.
//!
//! Every object git accepts round-trips byte for byte:
//! `serialize(parse(x)) == x`. This includes odd but valid objects found in
//! old or hand-made repositories: non-canonical or zero-padded tree modes,
//! commits and tags with unknown or reordered headers, multi-line headers
//! (`gpgsig`, `gpgsig-sha256`, `mergetag`), tags without `tagger`, commits
//! without a message separator, and identities or messages that are not
//! UTF-8.
//!
//! - [`ObjectKind`], [`ObjectHeader`]: the `<type> <size>\0` header.
//! - [`Tree`], [`TreeEntry`], [`EntryMode`], [`TreeIter`], [`entry_cmp`]:
//!   trees and git's tree entry order.
//! - [`Commit`], [`Tag`]: header block ([`HeaderBlock`]) plus message, with
//!   typed accessors; [`Signature`] / [`SignatureRef`] for identities.
//! - [`Object`], [`Blob`]: any object.
//! - [`fsck`]: the strict validator (git's fsck message ids), separate from
//!   the tolerant parsers.
//!
//! Parsers never panic on malformed input; they return [`Error`].
//!
//! ```
//! use rgit_hash::HashKind;
//! use rgit_object::{Object, ObjectKind};
//!
//! let data = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n\
//!              author A <a@example.com> 1 +0000\n\
//!              committer C <c@example.com> 1 +0000\n\nmessage\n";
//! let obj = Object::parse(ObjectKind::Commit, HashKind::Sha1, data).unwrap();
//! assert_eq!(obj.serialize(), data);
//! let Object::Commit(c) = obj else { unreachable!() };
//! assert_eq!(c.tree().unwrap(), HashKind::Sha1.empty_tree());
//! assert_eq!(c.author().unwrap().email, b"a@example.com");
//! ```

mod commit;
mod error;
pub mod fsck;
mod header;
mod headers;
mod ident;
mod kind;
pub mod messages;
mod object;
mod tag;
mod tree;

pub use commit::Commit;
pub use error::Error;
pub use header::{MAX_HEADER_LEN, ObjectHeader};
pub use headers::{Header, HeaderBlock};
pub use ident::{Signature, SignatureRef, Time};
pub use kind::ObjectKind;
pub use object::{Blob, Object};
pub use tag::{SIGNATURE_BEGIN_MARKERS, Tag, split_signature};
pub use tree::{EntryMode, Tree, TreeEntry, TreeEntryRef, TreeIter, entry_cmp};

pub use rgit_hash::{HashKind, ObjectId};
