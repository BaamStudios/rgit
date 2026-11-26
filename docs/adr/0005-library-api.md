# ADR 0005: rgit as a Rust library

- Status: accepted (owner decision 2026-11-23: rgit becomes a Rust library)
- Date: 2026-11-26
- Issue: #587 (milestone M6)

## Context

ADR 0003 says "one library crate per subsystem, the binary only parses
arguments and dispatches". The plumbing follows that rule; the porcelain does
not. Measured on `main` (2026-11-26, lines of Rust below `src/`):

| Where | Lines | Share |
|---|---:|---:|
| `rgit-cli` (435 files, 198 of them in `src/commands/`) | 170,006 | 54 % |
| the 45 other crates together | 142,971 | 46 % |

What a Rust program can use today is therefore the plumbing only: objects,
packs, refs, config, index, the revision parser, the diff and merge
algorithms. Everything a user of git thinks in (status, commit, log, switch,
merge, rebase, fetch, push) lives in the binary crate and is written for one
process that runs one command:

| In `rgit-cli/src` | Count |
|---|---:|
| mentions of the error type `Die` (message + exit code) | about 3,800 |
| `print!`/`println!`/`eprint!`/`eprintln!` | 1,175 |
| reads of the process environment (`std::env::var*`) | 142 |
| uses of the current directory (`current_dir`, `set_current_dir`) | 44 |
| `process::exit` and helpers that end the process (`die_now`, ...) | 25 |
| process-wide `static`s and thread-locals with state | 26 |

The handle every command works with (`repository::Repo`) changes the current
directory during setup, installs process-wide hooks (sparse index reader,
index write observer, replace refs, path protection settings) and prints
warnings while opening. No command can be called from another program as it
is.

The plumbing crates are closer, but not clean either. They print in about 85
places (`rgit-fetch` 33, `rgit-convert` 12, `rgit-transport` 9, `rgit-pack`
9, ...) and read the environment or keep process-wide state in these places:

| Crate | Process-wide state |
|---|---|
| `rgit-odb` | `GIT_OBJECT_DIRECTORY`, `GIT_ALTERNATE_OBJECT_DIRECTORIES`; the lazy-fetch hook (`static FETCHER`); skipped object copies printed by default |
| `rgit-index` | `GIT_INDEX_FILE`; `MAX_PATH_LEN`, `PROTECT_NTFS`/`PROTECT_HFS`; thread-local sparse-index reader and write transform; write observer |
| `rgit-refs` | reftable options and the registry of open stacks |
| `rgit-date` | `TZ`, `GIT_TEST_DATE_NOW` in `DateContext::from_env` |
| `rgit-config` | `PWD` for `includeIf "gitdir:"` |
| `rgit-pathspec` | `GIT_*_PATHSPECS` in `GlobalFlags::from_env` |
| `rgit-worktree` | `core.precomposeUnicode` switch |
| `rgit-convert` | the running filter processes and their base environment |
| `rgit-protocol`, `rgit-http`, `rgit-trace` | trace and sideband switches (`GIT_TRACE*`, `GIT_CURL_VERBOSE`) |
| `rgit-fs` | the registry of lock files, with an `atexit` hook and signal handlers |
| `rgit-messages` | `NO_ADVICE` |

Good news from the same survey: `rgit-repo` never changes the current
directory and takes it as an argument, `rgit-config` reads the environment
through a trait (`Environment`, with `MapEnv` for a fixed map), 20 crates
have typed errors with `thiserror`, and the object database, the reference
store and the configuration are `Send + Sync`.

## Goals

1. **Embedding.** A Rust program (a forge or CI server, an editor or IDE, a
   build or release tool) opens repositories and runs git operations in its
   own process, on many repositories and from many threads, without starting
   `rgit`.
2. **One implementation.** The command line runs the same code as the
   library. A command is: parse arguments, call the library, render the
   result, choose the exit code.
3. **A small, stable surface.** One documented crate with a few types; the
   crates below it stay free to change.
4. **No surprises for the host.** A call does not print, does not exit, does
   not read or change state of the process the caller did not hand over, and
   can be cancelled.

## Non-goals

- A second implementation next to the command line (wrappers that shell out,
  or logic written twice). Step 1 below does contain logic written twice
  (about 1,200 lines of glue); it is named there and removed by the steps
  that follow.
- libgit2 or gitoxide API compatibility, and bindings for other languages
  (a C API can be put on top of the facade later; it is not designed here).
- An async API. ADR 0003 stays: synchronous I/O, threads inside. Async
  callers use their runtime's blocking pool.
- Stability of the `rgit-*` crates below the facade.
- Byte-identical *output* from the library. Output formats (`--porcelain`,
  `--format`, the long status, pager, colours, columns) belong to the command
  line. The library returns data; where git has an error message for a
  situation the library uses that text, so the command line stays compatible.

## Decision

### 1. Three tiers

```
rgit-cli          argv, option tables, help, pager, colour, terminal,
   │              rendering of results, exit codes
   ▼
rgit              the facade: Repository and a few value types;
   │              documented, semver, small (this ADR)
   ▼
rgit-porcelain    the operations (status, history, checkout, sequencer,
   │              transfer ...) moved out of rgit-cli; typed errors, no
   │              printing; API may change with every release
   ▼
rgit-repo, rgit-odb, rgit-refs, rgit-index, rgit-diff, ...   (plumbing)
```

- **`rgit` (facade).** What other programs depend on. It contains API
  types, composition and documentation, not algorithms.
- **`rgit-porcelain`.** One crate, created by step 2, that receives the
  modules of `rgit-cli` one group at a time (order below). One crate and not
  one per command, because the modules reference each other heavily (status
  uses diff, the sequencer uses commit, checkout and merge); moving them
  into several crates at once would mean untangling all of that first. It
  can be split later along the seams that have appeared. Heavy optional
  parts get cargo features (`http` for `rgit-http` and rustls).
- **`rgit-cli`.** May use all three lower tiers: the facade where its data
  is enough, `rgit-porcelain` for what only git's output formats need.

Dependencies still point downwards only (ADR 0003).

### 2. The facade API

One handle, `rgit::Repository`, and operations as methods on it. Options are
builders without public fields; results are plain data. In the table,
"step 1" is implemented by the PR that adds this ADR.

| Area | API (sketch) | When |
|---|---|---|
| Open | `Repository::open(path)`, `Repository::discover(path)`, `OpenOptions::{new, isolated, environment, config_override, ceiling_dir}` | step 1 |
| Facts | `git_dir()`, `common_dir()`, `work_dir()`, `is_bare()`, `object_format()` | step 1 |
| References | `head() -> Head`, `find_reference(name)`, `references(prefix)` | step 1 |
| Revisions | `rev_parse(spec) -> ObjectId` | step 1 |
| History | `log(LogOptions) -> Log` (iterator of `Commit`): revisions and ranges, order, first-parent, skip, max-count | step 1 |
| | paths, `--grep`/`--author`, merge bases, `describe`, blame | with group 5 |
| Status | `status(StatusOptions) -> Status`: staged, unstaged, conflicted, untracked | step 1 |
| | renames, ahead/behind, state of a merge or rebase in progress, submodule detail | with groups 2 and 3 |
| Create | `Repository::init(path, InitOptions)`, `Repository::clone(url, path, CloneOptions)` | groups 6, 9 |
| Index and commit | `add(paths)`, `remove(paths)`, `commit(CommitOptions) -> ObjectId`, `reset(..)`, `restore(..)` | group 6 |
| Branches and tags | `branches()`, `create_branch`, `delete_branch`, `rename_branch`, `switch(..)`, `tags()`, `create_tag`, `delete_tag`, `update_reference` (transaction) | group 7 |
| Diff | `diff(DiffOptions) -> Diff` (tree/tree, tree/index, index/work tree): entries, hunks, stats; `Diff::write_patch(&mut impl Write)` | group 4 |
| Merge and rebase | `merge(revs, MergeOptions) -> MergeOutcome`, `cherry_pick`, `revert`, `rebase(RebaseOptions) -> RebaseOutcome`, `continue_/abort_/skip_operation()` | group 8 |
| Transfer | `remotes()`, `fetch(remote, FetchOptions)`, `push(remote, PushOptions) -> PushOutcome` | group 9 |
| Configuration | `config() -> Config` (typed getters), `config_mut()` (set/unset in a chosen file) | with group 1 |
| Objects | `find_object(id)`, `find_commit`, `find_tree`, `find_blob`, `write_blob` | with group 1 |

Callbacks and tokens shared by the long operations:

```rust
/// Stops an operation from another thread (implemented in step 1).
pub struct Cancel { /* Arc<AtomicBool> */ }

/// Told what a long operation is doing; called on the calling thread.
pub trait Progress: Send {
    fn start(&mut self, task: &str, total: Option<u64>);
    fn advance(&mut self, done: u64);
    fn finish(&mut self);
    /// Text the other side sent for the user (sideband 2 of a fetch or push).
    fn remote_message(&mut self, text: &BStr) {}
}

/// Asked for a user name and password or token (HTTP, and SSH key phrases).
pub trait Credentials: Send {
    fn get(&mut self, request: &CredentialRequest) -> Result<Option<Credential>>;
    fn approved(&mut self, credential: &Credential) {}
    fn rejected(&mut self, credential: &Credential) {}
}
```

- Every operation that can run long takes `cancel` and `progress` in its
  options. Cancelling returns `Error::Cancelled` at the next check; lock
  files are released by their guards, so nothing is left half-written.
- Without a `Credentials` callback the configured helpers
  (`credential.helper`) are asked and nothing else: the library never opens
  the terminal and never starts an askpass program on its own. The command
  line passes a callback that does both.
- Hooks, the editor and other programs the operations start are run by the
  library only where the operation is defined by them (`commit` runs
  `pre-commit` unless `CommitOptions::no_verify`); their output is captured
  and returned or passed to a callback, never written to the process's
  standard streams.

### 3. Names are byte strings

git does not require reference names and file names to be UTF-8.

- Reference names and paths inside a repository are `bstr::BString` when
  returned and `impl AsRef<[u8]>` (or `&BStr`) when taken; `&str` works at
  every call site. The facade re-exports `BStr` and `BString`.
- Paths in the file system are `std::path::Path`.
- Text that is defined to be bytes (commit messages, author names) is
  `BString`; text the library itself produces (error messages) is `String`.

Step 1 already has these types. `rgit-refs` still takes `str` (#214 changes
that); until then the facade converts in one module (`crates/rgit/src/refs.rs`)
and returns `Error::InvalidRefName` for a name that is not UTF-8.

### 4. Errors

- **Typed, at every tier.** `rgit-porcelain` and the plumbing crates have
  their own `thiserror` enums. The facade has one enum, `rgit::Error`,
  `#[non_exhaustive]`, with variants a caller can act on (`NotARepository`,
  `BareRepository`, `RevisionNotFound`, `Revision`, `InvalidRefName`,
  `ObjectNotFound`, `Corrupt`, `Io`, `Config`, `Open`, `Unsupported`,
  `Cancelled`; later `Conflict`, `Locked`, `NonFastForward`, `Rejected`,
  `Authentication`, `Network`, `HookFailed`, ...).
- **Lower errors are sources, not signatures.** An error of a crate below
  appears as `std::error::Error::source()` (boxed, `Send + Sync`), never as a
  type in the facade's API, so those crates can change freely.
- **No printing, no exit.** `eprintln!`, `process::exit`, `Die`, and
  `panic!` on bad input are not allowed in `rgit` and `rgit-porcelain`.
  A damaged repository is input.
- **Messages.** `Display` of an error is the text git prints after
  `fatal: ` wherever git has a message for the situation; the texts stay in
  the `messages.rs` of the crate that raises them (CLEANROOM.md). What git
  prints *before* the fatal line (`error:`, `warning:`, `hint:` lines) is
  carried as `Diagnostic { level, text }`: on the error
  (`Error::diagnostics()`) when it belongs to a failure, on the handle
  (`Repository::take_diagnostics()`) when the operation went on.
- **How the command line maps them.** One place,
  `rgit-cli/src/library.rs`: print the diagnostics with their prefixes, then
  `fatal: {error}`, exit with `Error::exit_code()` (128; 130 for
  `Cancelled`). `impl From<rgit::Error> for Die` makes `?` work in command
  code. Exit codes other than 128 that a command defines for a *result*
  (1 for "there are differences", 1 for a merge that stopped with conflicts,
  129 for usage) are decided by the command from the returned data: a
  conflict is an outcome (`MergeOutcome::Conflicts`), not an error.
- **Advice** (`advice.*`) is command line presentation. The library returns
  the fact (for example `PushOutcome::rejected` with the reason); the command
  line decides whether to print the hint.

### 5. No process-wide state

Rules for `rgit` and `rgit-porcelain`, and the target for the plumbing:

- **Current directory.** Never changed, never read after the call that
  resolves a relative path argument. Every path the library keeps is
  absolute. Paths in revision expressions and pathspecs count from the top
  of the work tree; a caller that wants "relative to here" passes a prefix
  (`rev-parse` in a subdirectory is the command line's job).
- **Environment.** Read only through the `Environment` the repository was
  opened with: `OpenOptions::new()` uses the process (so the user's
  identity and configuration apply as they do for `git`),
  `OpenOptions::isolated()` an empty one without system or user
  configuration (servers, tests), `OpenOptions::environment(MapEnv)` exactly
  what the caller lists.
- **Variables that select things for the `git` command are options here.**
  `GIT_DIR`, `GIT_WORK_TREE`, `GIT_COMMON_DIR`, `GIT_OBJECT_DIRECTORY`,
  `GIT_ALTERNATE_OBJECT_DIRECTORIES`, `GIT_INDEX_FILE`, `GIT_NAMESPACE`,
  `GIT_CEILING_DIRECTORIES` are not read by the library. Each becomes a
  setter on `OpenOptions` when the first caller needs it; the command line
  translates the environment and its global options (`--git-dir`,
  `--work-tree`, `-c`, `-C`) into `OpenOptions` in one place.
- **Configuration.** Discovered from the injected environment
  (`HOME`, `XDG_CONFIG_HOME`, `GIT_CONFIG_*`), plus
  `OpenOptions::config_override(key, value)` with the precedence of `-c`.
  Loaded once per handle; a handle is a snapshot of the configuration, not
  of the repository (index, references and objects are read when used).
- **Clock and time zone.** Passed in (`rgit-date` has `DateContext`); the
  default reads the clock, and the zone from the injected environment.
- **Signals and exit hooks.** The library installs none. Lock and temporary
  files are removed by `Drop`. The process-wide registry of `rgit-fs`
  (cleanup on `SIGINT` and at exit) becomes opt-in and is switched on by the
  command line.
- **Tracing** (`GIT_TRACE*`, trace2) is a facility of the process and stays
  process-wide; it is off unless the host enables it, and its target is set
  by the host, not read from the environment by library code.
- **Statics** with settings (`rgit-index` path protection, `rgit-refs`
  reftable options, `rgit-odb` lazy-fetch hook, `rgit-worktree`
  precompose switch) become fields of the handle or arguments.

The table in "Context" is the work list; it is tracked in #724 and done
crate by crate, each time a group that needs the crate moves.

### 6. Threads

- `Repository` is `Send + Sync` (checked at compile time in
  `crates/rgit/src/lib.rs`). Operations take `&self`. One handle serves many
  threads; opening the same repository twice is also fine.
- Per-operation state (the index, attribute and ignore stacks, caches of
  trees) lives in the operation, not in the handle, so there is no interior
  mutability a caller has to know about beyond the caches of the object
  database, which are already thread-safe.
- Writers rely on git's on-disk locking (`index.lock`, reference
  transactions), so two handles, two threads or two processes exclude each
  other the same way. A lock held by someone else is `Error::Locked`, not a
  wait, unless the option says how long to wait (`core.filesRefLockTimeout`
  and friends apply as configured).
- Inside an operation the library may use scoped threads (ADR 0003). It
  starts no thread that outlives the call and keeps no thread pool.
- Callbacks (`Progress`, `Credentials`) are called on the calling thread.
- No async. A handle can be moved into `spawn_blocking`.

### 7. Stability

- **`rgit`**: semantic versioning. `0.x` while this plan runs (a minor
  version may break), `1.0` when the areas of the table are covered and the
  commands of those areas run through the library. From then on: breaking
  changes only in a major version; enums and result structs are
  `#[non_exhaustive]`, options are builders, so growth is not breaking.
- **MSRV** is `rust-version` of the workspace; raising it is a minor
  release.
- **Re-exports are API.** The facade re-exports only `ObjectId`, `HashKind`
  (`rgit-hash`), `Environment`, `MapEnv`, `SystemEnv` (`rgit-config`) and
  `BStr`, `BString` (`bstr`). A breaking change to one of these is a
  breaking change of `rgit`. Everything else from below is wrapped.
- **`rgit-porcelain` and the plumbing crates** make no promise. If they are
  published (the facade needs them on crates.io), each release is a new
  `0.x`, and the facade pins exact versions (`=0.x.y`).
- **Deprecation**: at least one minor release with `#[deprecated]` before
  removal, after 1.0.
- **License**: MIT, as the repository. The GPL test suite stays in
  `rgit-conformance` and is never part of a published crate.
- **Publication** waits for the open question on the crate name (below);
  until then the facade has `publish = false`.

### 8. Moving the porcelain out of `rgit-cli`

A move takes a group of modules from `rgit-cli/src` to `rgit-porcelain`
and, in the same pull request or a directly following one, makes them
library code:

1. `Die` becomes a typed error of the module; messages move to the
   `messages.rs` of `rgit-porcelain` unchanged.
2. Printing becomes a return value, a `Diagnostic`, or (for streamed
   output such as a patch) a `&mut dyn Write` argument.
3. `std::env`, the current directory and statics become arguments or
   fields of the handle.
4. The command file in `rgit-cli` keeps the option table, the usage text,
   the call and the rendering.
5. The facade grows the methods of the area, with rustdoc examples and
   differential tests.

No behaviour changes in a move: the conformance baseline must not move
either way, and the differential tests of the commands stay as they are.

Survey of `rgit-cli/src` by destination (lines; a command file counts with
the group whose logic it holds, although its option parsing and rendering
stay behind, so the library's share of a group is smaller than its size):

| Step | Group | Lines | Main modules |
|---:|---|---:|---|
| – | stays in `rgit-cli` | 13,347 | `main`, `cmdlist`, `alias`, `autocorrect`, `pager`, `color`, `column`, `context`, `global`, `external`, `interactive/` (terminal dialogs), `help`, the server and credential programs, small text tools |
| – | `messages/` | 13,124 | move with the code that raises them |
| 2 | Repository handle | 6,340 | `repo`, `repo_objects`, `repository`, `revisions`, `revision`, `refs_support`, `refs`, `objects`, `grafts`, `replace_objects`, `commit_graph`, `index_config`, `promisor`, `sparse_index`, `sparse`, `hooks` |
| 3 | Work tree content | 1,056 | `convert` (line endings, `ident`, encodings, filter drivers), `attributes` |
| 4 | Status | 7,563 | `wt_status`, `untracked`, `untracked_cache`, `tree_changes`, `tracking`, `compare_branches`; `status`, `ls-files`, `clean`, `check-ignore`, `check-attr` |
| 5 | Diff | 12,434 | `diff/`, `patch_id`, `range_diff/`; `diff`, `diff-tree`, `diff-index`, `diff-files`, `range-diff`, `difftool` |
| 6 | History | 30,410 | `history/`, `ref_filter/`, `format`, `notes`, `signing/`, `grep/`; `log`, `show`, `rev-list`, `rev-parse`, `for-each-ref`, `describe`, `name-rev`, `shortlog`, `blame`, `reflog`, `grep` |
| 7 | Index and commit | 12,735 | `add`, `commit`, `reset`, `restore`, `rm`, `mv`, `stash`, `update-index`, `read-tree`, `sparse-checkout` |
| 8 | Branches, tags, switch | 9,656 | `switch_branch`, `worktrees`; `checkout`, `switch`, `branch`, `tag`, `worktree`, `update-ref`, `symbolic-ref`, `replace` |
| 9 | Merge and sequencer | 24,255 | `sequencer`, `rebase/`, `rerere`; `merge`, `rebase`, `cherry-pick`, `revert`, `am`, `apply`, `replay`, `bisect`, `format-patch`, `mergetool` |
| 10 | Transfer | 17,483 | `push/`, `submodules`, `fetch_submodules`, `recurse_submodules`; `fetch`, `push`, `pull`, `clone`, `remote`, `ls-remote`, `submodule`, `bundle`, `archive`, `init` |
| 11 | Maintenance and object plumbing | 21,603 | `housekeeping/`, `maintenance/`, `reflog_expire`, `bitmaps`; `gc`, `repack`, `prune`, `pack-objects`, `index-pack`, `fsck`, `cat-file`, `fast-import`, `fast-export`, `config` |

Issues: step 2 #723, step 3 #725, step 4 #726, step 5 #727, step 6 #728,
step 7 #729, step 8 #730, step 9 #731, step 10 #732, step 11 #733.

Why this order:

- **The handle is first because everything takes it.** Every function to
  be moved has a `&Repo` parameter; `Repo` cannot leave `rgit-cli` as long
  as it prints, changes directory and installs process-wide hooks. Step 2
  creates `rgit-porcelain` with the handle (the facade's `Repository` of
  step 1 becomes a thin wrapper around it) and gives `rgit-cli` an adapter
  that builds the handle from its global options. This is also the reason
  why step 1 switches no command over: with two different handles in one
  process, a command would open the repository twice and lose replace
  refs, grafts, the commit-graph and the sparse index on the library side.
- **Content conversion is second** because status, diff, add and checkout
  all compare or write work tree files, and it is small.
- **Read-only before writing** (status, diff, history before commit,
  switch, merge): the read side is what servers and editors need first,
  it has no locking and no hooks, and the writers build on it.
- **Transfer late**: it needs credentials and progress callbacks through
  several crates that print today (`rgit-fetch`, `rgit-transport`).
- **Maintenance last**: least asked for by embedders.

Steps 4 to 6 and 7 to 11 can overlap once step 2 is in; within a step the
work is one pull request per module where the tangle allows it. The epic is
scheduled between feature waves, because a move conflicts with every open
pull request that touches the moved files.

### 9. Testing

- **The differential tests stay** where they are: the tests of `rgit-cli`
  run the binary against git and do not care where the code lives. That is
  what makes a move safe.
- **The conformance suite stays** the acceptance test of behaviour.
- **The facade has its own differential tests** (`crates/rgit/tests/`):
  repositories are built with the reference git, and each answer of the API
  is compared with what git prints for the same question (`status
  --porcelain`, `rev-list`, `rev-parse`, `for-each-ref`, `log --format`).
  They open repositories with `OpenOptions::isolated()` or a `MapEnv`, so
  they cannot read the user's configuration.
- **Three-way test** (`crates/rgit-cli/tests/library.rs`): the same
  repository asked through the binary, the library and git must give the
  same answers and the same error messages. While both code paths exist it
  is the test that notices when they drift apart; afterwards it is the test
  of the rendering.
- **Rustdoc examples** on every operation of the facade. They compile in
  CI; examples that need a repository are `no_run` until the API can create
  one itself (`init`, `add`, `commit`), then they run.
- **Purity check** (#734): a script in CI greps `crates/rgit` and
  `crates/rgit-porcelain` for `println!`, `eprintln!`, `process::exit`,
  `std::env::var`, `set_current_dir` and `static mut` outside tests and
  comments, with an allow-list for the reviewed exceptions. The plumbing crates are added as
  they are cleaned.
- **Thread-safety** is asserted at compile time (`Send + Sync`) and
  exercised by a test that shares one handle between threads.

## Step 1 (the pull request that adds this ADR)

- `crates/rgit`: `Repository`, `OpenOptions`, `Head`, `Reference`,
  `rev_parse`, `log`, `status`, `Cancel`, `Error`, `Diagnostic`; no
  dependency on `rgit-cli`, no printing, no environment access except
  through `Environment`; 19 differential tests against git and compiled
  rustdoc examples.
- `rgit-cli` depends on the facade: `src/library.rs` maps `rgit::Error` to
  the command line's error type, and `tests/library.rs` is the three-way
  test. No command is switched yet (see "The handle is first").
- The bin target `rgit` of `rgit-cli` has `doc = false`: the library crate
  has the same name and owns `target/doc/rgit`.

Limits of step 1, each removed by the step named:

| Limit | Removed by |
|---|---|
| `status` reports renames as deletion plus addition, no ahead/behind, no state of a running merge or rebase, and for a submodule only another checked-out commit | step 4 (#726) |
| `status` returns `Error::Unsupported` when a file must be read to be compared and its content goes through a `filter.<name>.clean`/`.process` driver (text, `eol`, `ident` and encodings are converted) | step 3 (#725) |
| `status`, `log` and `rev_parse` do not use replace refs, grafts or the commit-graph | step 2 (#723) |
| The facade has its own glue for revisions (`revision.rs`), content (`convert.rs`) and the status walk (`status.rs`): about 1,200 lines that duplicate what `rgit-cli` does with more features | steps 2, 3, 4 (the facade then calls `rgit-porcelain`) |
| Reference names that are not UTF-8 are refused | #214, then #736 |
| `GIT_ALTERNATE_OBJECT_DIRECTORIES`, `TZ` and the trace variables are still read from the process by the plumbing | #724 |

## Open questions (owner)

Tracked in #735.

1. **Crate name.** `rgit` on crates.io belongs to another project (a git
   web interface, 0.11.0). Options: publish the facade under another package
   name and keep the library name (`[lib] name = "rgit"` in a package such as
   `rgit-lib`), choose a new name for package and library, or ask the owner
   of the name. Until decided, `crates/rgit` has `publish = false`. The names
   `rgit-hash`, `rgit-object`, `rgit-refs`, `rgit-repo`, `rgit-config`,
   `rgit-cli` were free on 2026-11-26.
2. **Publishing the lower crates.** The facade cannot be published without
   them. Either all `rgit-*` crates go to crates.io as unstable `0.x`, or the
   facade is released as one crate with the others merged in at release time.
   Recommended: publish them, marked "internal, no stability promise" in each
   README.
3. **When 1.0.** Proposed above: when the areas of the table are covered
   and their commands run through the library.

## Consequences

- `rgit-cli` shrinks from 170,000 lines towards the 13,000 that are
  genuinely command line, plus option tables and rendering.
- A move is a large mechanical change. It is done between feature waves and
  one group at a time.
- Until step 4 the facade and the command line compute status with
  different code; the three-way test covers the gap.
- The plumbing crates lose their direct reads of the environment and their
  prints; functions gain parameters.
- New porcelain features are written in `rgit-porcelain` from the start
  once the group they belong to has moved.
