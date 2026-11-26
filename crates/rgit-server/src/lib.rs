// SPDX-License-Identifier: MIT
//! The server side of git's transfer protocols (ADR 0004): the services a
//! hosting server starts once per connection and talks to over
//! standard input and output.
//!
//! The services are synchronous and written against `Read`/`Write` (plus
//! a small backend trait for the repository in `receive_pack`): a host
//! (sshd, a CGI web server, `git daemon`) starts one process per
//! connection, and tests drive the same code in memory.
//!
//! - [`daemon`]: the request and path rules of `git daemon`.
//! - [`receive_pack`]: the server side of push (`git-receive-pack`).
//! - [`upload_pack`]: serve fetches and clones (`git-upload-pack`),
//!   protocol v0, v1 and v2.
//! - [`http_backend`]: the CGI program for smart and dumb HTTP
//!   (`git-http-backend`).

pub mod daemon;
pub mod http_backend;
pub mod receive_pack;
pub mod upload_pack;
