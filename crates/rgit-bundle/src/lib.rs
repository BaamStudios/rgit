// SPDX-License-Identifier: MIT
//! Bundle files and bundle lists.
//!
//! - [`header`]: the header of a bundle file (`Documentation/gitformat-bundle.adoc`):
//!   signature (`# v2 git bundle` / `# v3 git bundle`), v3 capabilities
//!   (`@object-format`, `@filter`), prerequisites and references, followed
//!   by a pack.
//! - [`filter`]: object filter specs as the `@filter` capability and
//!   `--filter` give them (`Documentation/rev-list-options.adoc`).
//! - [`list`]: bundle lists (`Documentation/technical/bundle-uri.adoc`,
//!   `Documentation/config/bundle.adoc`): the `bundle.*` keys of a bundle
//!   list file or of a protocol v2 `bundle-uri` answer.
//!
//! User-visible messages are collected in [`messages`].

pub mod filter;
pub mod header;
pub mod list;
pub mod messages;

pub use filter::FilterSpec;
pub use header::{Header, HeaderError, Prerequisite, Reference, read_header};
pub use list::{BundleList, ListEntry, ListMode};
