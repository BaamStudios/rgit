// SPDX-License-Identifier: MIT
//! The server side of git's transfer protocols (ADR 0004): the services a
//! hosting server starts once per connection and talks to over
//! standard input and output.
//!
//! The services are written against `Read`/`Write` and a small backend
//! trait for the repository, so tests can drive them in memory.

pub mod receive_pack;
