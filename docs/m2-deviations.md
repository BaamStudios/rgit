# M2: known deviations from git

Milestone M2 is "clone, edit, commit and push a real project with rgit alone"
(issue #163). The acceptance test exercises exactly that against Gitea and
github.com. The following deviations from git are deliberate for M2. Each
one is a missing feature, not different behaviour: where rgit meets one of
them it fails or warns instead of doing something else silently.

| Area | M2 behaviour | Notes |
|---|---|---|
| Wire protocol | Protocol v2 only, for local paths, `git://`, SSH and smart HTTP(S). A server that answers with v0/v1 is refused with a `protocol error`. | Dumb HTTP is not supported. GitHub, GitLab, Gitea and git ≥ 2.18 servers speak v2. |
| Object format | SHA-1 only. | Repositories with `extensions.objectFormat = sha256` are refused. |
| Diff | No rename or copy detection (`-M`, `-C`, `diff.renames`): a rename shows as a deletion plus an addition. | Applies to `diff`, `status` and `log`. |
| Hooks | Hooks were not run in M2; when a hook would have run rgit printed a warning naming it. | Since #76 the commit hooks, `post-checkout` and `pre-push` run; see [hooks.md](hooks.md) for the rest. |
| Content filters | No CRLF conversion (`core.autocrlf`, `text`/`eol` attributes), no `filter.<driver>` clean/smudge, no `ident`, no working-tree encoding. | Files are stored and checked out byte for byte. |
| Output | No pager and no colour. | Output goes straight to stdout; `color.*` and `--color` have no effect. |
| Progress | No progress meters for clone, fetch or push. | Server `remote:` messages are still shown. |
| Shallow clones | No `--depth`, `--shallow-since`, `--shallow-exclude`, and no fetching into a shallow repository. | |
| `log` formats | `log` supports the default format and `--oneline`; `--format`/`--pretty` placeholders are not implemented yet. | Tracked in #125. |

## Acceptance test

- `scripts/acceptance/m2-gitea.sh` starts a Gitea container (pinned image,
  real git inside, unique name, random host ports on 127.0.0.1), creates a
  user, a token, an SSH key and one private repository per transport, and runs
  `scripts/acceptance/scenario.sh` three times: HTTPS with the token from a
  credential helper, HTTPS with the token from askpass, and SSH (OpenSSH, key
  authentication). The client runs under `env -i` with only rgit (as `git`), a
  few shell utilities and `ssh` on `PATH`. The server side is checked with the
  reference git in the container (`git fsck --strict`, refs, history) and the
  commit ids against fixed expectations. `--reference-client "$(command -v git)"`
  runs the same scenario with git, to check the scenario itself.
- `scripts/acceptance/github-public.sh <url>...` runs anonymous `ls-remote`
  and `clone` against public repositories and compares with the reference git.
- CI: workflow `Acceptance` (`.github/workflows/acceptance.yml`). The Gitea job
  runs on every pull request; the github.com job runs nightly and is
  non-blocking.

Running it locally needs docker, curl, openssl and OpenSSH:

```sh
cargo build --release -p rgit-cli
scripts/acceptance/m2-gitea.sh                       # all three transports
scripts/acceptance/m2-gitea.sh --modes ssh --keep    # one transport, keep the container
scripts/acceptance/github-public.sh https://github.com/octocat/Hello-World.git
```
