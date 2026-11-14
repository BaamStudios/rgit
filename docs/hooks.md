# Hooks

rgit finds and runs hooks with the crate `rgit-hook` (issue #76), following
`githooks(5)`, `git-hook(1)` and `config/hook.adoc`:

- the hooks configured with `hook.<name>.command` / `hook.<name>.event` run
  first, in the order of their `event` lines, then the executable file named
  after the event in the hook directory (`core.hooksPath`, else
  `$GIT_DIR/hooks`); a file without the executable bit is skipped with the
  `advice.ignoredHook` hint;
- `hook.<name>.enabled`, `hook.<event>.enabled`, `hook.<name>.parallel`,
  `hook.<event>.jobs`, `hook.jobs` and `-j` work as documented; output of
  hooks that run in parallel is collected per hook;
- hooks run at the top of the work tree (in the git directory of a bare
  repository) with `GIT_PREFIX` and the command's own variables
  (`GIT_INDEX_FILE`, `GIT_EDITOR`, `GIT_AUTHOR_*` for the commit hooks);
  their standard output goes to standard error, except for `pre-push`;
- the exit codes of all hooks of an event are or-ed together.

`git hook run` and `git hook list` are implemented.

## Call sites

Every hook of `githooks(5)`, where git runs it, and where rgit stands.

| Hook | Run by | rgit | Owner |
|---|---|---|---|
| `applypatch-msg` | `am` | not yet (no `am`) | #136 |
| `pre-applypatch` | `am` | not yet (no `am`) | #136 |
| `post-applypatch` | `am` | not yet (no `am`) | #136 |
| `pre-commit` | `commit`, `merge` (via `pre-merge-commit`) | `commit` | #76 |
| `pre-merge-commit` | `merge` | not yet (no `merge`) | #57 |
| `prepare-commit-msg` | `commit`, `merge`, `rebase`, ... | `commit` | #76 |
| `commit-msg` | `commit`, `merge`, `rebase -i` | `commit` | #76 |
| `post-commit` | `commit` | `commit` | #76 |
| `pre-rebase` | `rebase` | not yet (no `rebase`) | #154 |
| `post-checkout` | `checkout`, `switch`, `clone`, `worktree add`, `rebase` | `checkout`, `switch`, `clone` | #76 (`worktree add`: #104) |
| `post-merge` | `merge`, `pull` | not yet (no `merge`) | #57 |
| `pre-push` | `push` | `push` | #76 |
| `pre-receive` | `receive-pack` | not yet | #175 |
| `update` | `receive-pack` | not yet | #175 |
| `proc-receive` | `receive-pack` | not yet | #175 |
| `post-receive` | `receive-pack` | not yet | #175 |
| `post-update` | `receive-pack` | not yet | #175 |
| `reference-transaction` | every reference update | not yet | #76 |
| `push-to-checkout` | `receive-pack` | not yet | #175 |
| `pre-auto-gc` | `gc --auto` | not yet (no `gc`) | #92 |
| `post-rewrite` | `commit --amend`, `rebase` | not yet; `commit --amend` warns when one is enabled | #154 |
| `sendemail-validate` | `send-email` | out of scope | #186 |
| `fsmonitor-watchman` | `core.fsmonitor` | not yet | #118 |
| `p4-changelist` | `git-p4` | out of scope | #186 |
| `p4-prepare-changelist` | `git-p4` | out of scope | #186 |
| `p4-post-changelist` | `git-p4` | out of scope | #186 |
| `p4-pre-submit` | `git-p4` | out of scope | #186 |
| `post-index-change` | every index write | not yet | #76 |

## Known differences

- The usage of `git hook run` aligns its second line under the first
  argument (the option parser's rule); git indents it by seven columns.
- `trace2` regions for hooks (`GIT_TRACE2_EVENT`) are not written.
- `commit` does not pass `template`, `squash` or `commit <rev>` for
  `-t`, `--squash` and `-c`/`-C` to `prepare-commit-msg` yet, because those
  options are not implemented (#128).
