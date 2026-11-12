# ADR 0002: Own implementation, no gitoxide dependency

Status: accepted (2026-11-12) · Issue #3

## Decision

rgit implements every subsystem itself. It does **not** depend on the
`gix-*` crates of gitoxide, and it does not use libgit2.

gitoxide (MIT OR Apache-2.0) may still be read as a reference and individual
pieces may be ported, following CLEANROOM.md: the ported file gets a header
naming the gitoxide source file, and the notice in `THIRD-PARTY-NOTICES.md`
is kept.

## Allowed dependencies

General-purpose crates with permissive licenses (checked by `cargo-deny`),
for example: hashing primitives (`sha1-checked`, `sha2`), compression
(`flate2`/`zlib-rs`), `memmap2`, `thiserror`, `clap` only if it can reproduce
git's option parsing exactly (otherwise our own parser, #73).

## Why

Full control over behaviour, performance and API; no coupling to gitoxide's
release cycle; the project's purpose is a complete independent
implementation.
