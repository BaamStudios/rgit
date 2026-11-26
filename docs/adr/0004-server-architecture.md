# ADR 0004: Server architecture (M5)

- Status: accepted
- Date: 2026-11-26
- Issue: #173 (part of #67)

## Context

ADR 0003 left the server-side I/O model open until E8 starts. M5's goal is a
drop-in replacement for git on hosting servers (Gitea, GitLab, sshd, CGI
web servers, `git daemon`). Those hosts all start one `git-upload-pack`,
`git-receive-pack`, `git-upload-archive` or `git http-backend` process per
request or connection and talk to it over stdin/stdout (plus environment
variables such as `GIT_PROTOCOL`, `GIT_NAMESPACE`, `REMOTE_USER`).

## Decision

1. **Process model: one process per connection, started by the host.** rgit
   ships no long-running server of its own besides `rgit daemon`, which (like
   `git daemon`) only accepts connections and spawns one service process per
   connection. The dashed programs `rgit-upload-pack`, `rgit-receive-pack`,
   `rgit-upload-archive` and `rgit-http-backend` (and `git-*` names when
   installed as git) dispatch to the same code as `rgit upload-pack` etc.
2. **Synchronous, no async runtime.** A crate `rgit-server` holds the
   service logic over `Read`/`Write` (generic, so tests drive it in memory).
   Concurrency inside one service (sideband progress while packing, hook
   output while reading the pack, keepalives) uses `std::thread` and channels.
   `rgit daemon` uses one thread to accept and one child process per
   connection, as git does.
3. **Hooks** run through `rgit-hook`. Their stdout/stderr is relayed to the
   client on sideband 2 when the client negotiated `side-band(-64k)`,
   otherwise to our stderr, exactly as git does. `proc-receive` uses its
   pkt-line protocol over the hook's stdin/stdout.
4. **Untrusted input.** The server never trusts sizes, counts or depths sent
   by the client: incoming packs are indexed into a quarantine directory
   (`GIT_QUARANTINE_PATH`, `tmp_objdir`-style) and checked (`fsck`,
   connectivity) before refs move; existing limits are honoured
   (`receive.maxInputSize`, `transfer.fsckObjects`, `uploadpack.*`,
   `GIT_PROTOCOL`-negotiated features only). Delta resolution reuses
   `rgit-pack`'s bounded caches. Time-outs: `--timeout` / `daemon --timeout`
   as documented; no extra rgit-only limits by default.
5. **Library API.** `rgit-server` is a public crate with a small API
   (`upload_pack(repo, opts, input, output)`, `receive_pack(...)`) so it can
   be embedded, but stability is only promised together with the library epic
   (#587).

## Consequences

- #68 / #174 (upload-pack v2 and v0/v1), #69 / #175 (receive-pack and hooks),
  #70 (http-backend) and #176 (daemon, git-shell) build on `rgit-server`;
  their scope does not change.
- The conformance suite's server side can switch from the reference git to
  rgit script by script once a service passes its scripts.
