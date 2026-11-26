// SPDX-License-Identifier: MIT
//! Server side of git's wire protocol (ADR 0004).
//!
//! The services are synchronous and generic over `Read`/`Write`: a host
//! (sshd, a CGI web server, `git daemon`) starts one process per
//! connection and the service talks over its standard input and output;
//! tests drive the same code in memory.
//!
//! - [`upload_pack`]: serve fetches and clones (`git-upload-pack`),
//!   protocol v2.

pub mod upload_pack;
