// SPDX-License-Identifier: MIT
//! Archives of trees for rgit (`git archive`, `git upload-archive`,
//! `git get-tar-commit-id`).
//!
//! This crate holds the parts that do not need a repository: the tar
//! writer ([`tar`]), the zip writer ([`zip`]), the internal gzip
//! ([`gzip`]) and the `upload-archive` protocol ([`remote`]). Walking a
//! tree, attributes and content conversion live in the `archive` command
//! of `rgit-cli`.
//!
//! Behaviour follows Documentation/git-archive.adoc,
//! Documentation/git-upload-archive.adoc,
//! Documentation/git-get-tar-commit-id.adoc, Documentation/config/tar.adoc
//! and, where those leave the bytes open, the reference `git` binary.

pub mod gzip;
pub mod messages;
pub mod remote;
pub mod tar;
pub mod zip;
