# ADR 0003: Architecture — crate layout, errors, I/O model

Status: accepted (2026-11-12) · Issue #4

## Crate layout

One library crate per subsystem, the binary only parses arguments and
dispatches.

| Crate | Responsibility |
|---|---|
| `rgit-hash` | Object ids, SHA-1 (collision detecting), SHA-256 |
| `rgit-object` | Parse/serialize blob, tree, commit, tag |
| `rgit-odb` | Object database: loose objects, alternates, lookup across packs |
| `rgit-pack` | Pack/idx read and write, deltas, midx, bitmaps, commit-graph |
| `rgit-config` | Config files, includes, scopes |
| `rgit-refs` | Loose/packed refs, reftable, reflog, transactions |
| `rgit-repo` | Repository discovery, layout, extensions; ties the above together |
| `rgit-revwalk` | Revision syntax, rev-list walking |
| `rgit-index` | Index file format |
| `rgit-worktree` | Status, checkout, ignore, attributes, filters |
| `rgit-diff` | Diff algorithms and output formats |
| `rgit-merge` | Three-way merge, merge-file, sequencer |
| `rgit-protocol` | pkt-line, capabilities, protocol v0/v1/v2 |
| `rgit-transport` | file, SSH, smart HTTP, credentials |
| `rgit-server` | upload-pack, receive-pack, http-backend (M5) |
| `rgit-cli` | The `rgit` binary |

Crates are created when the first issue needs them, not up front.
Dependencies point downwards only (hash → object → odb/pack → repo → …).

## Errors

- Library crates: typed errors with `thiserror`, no panics on bad input
  (corrupt repositories are normal input).
- CLI: maps errors to git's exact messages and exit codes
  (`fatal:` = 128, usage = 129, ...).

## I/O model

- Client side: synchronous I/O, parallelism with threads (`std::thread`,
  `rayon` where useful). No async runtime in core crates.
- Server side (M5): decided in a separate ADR when E8 starts.

## Other rules

- MSRV = `rust-version` in the workspace `Cargo.toml`.
- `unsafe` only for memory mapping, isolated and commented.
- Object ids generic over the hash algorithm from the start (SHA-256 later).
- Every user-visible output must be byte-identical to git (checked by the
  conformance suite).
