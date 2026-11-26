# Design notes

Architecture decision records live in `docs/adr/` (`NNNN-title.md`):

- [0001 Conformance strategy](adr/0001-conformance-strategy.md)
- [0002 Own implementation, no gitoxide dependency](adr/0002-own-implementation.md)
- [0003 Architecture](adr/0003-architecture.md)
- [0004 Server architecture](adr/0004-server-architecture.md)
- [0005 rgit as a Rust library](adr/0005-library-api.md)

Milestones:

- [M2: known deviations from git and the acceptance test](m2-deviations.md)

Topics:

- [Platforms: Linux, Windows, macOS; what CI checks on each](platforms.md)
- [Hooks: runner, call sites and owners](hooks.md)
- [HTTP configuration: what rgit supports](http-config.md)
