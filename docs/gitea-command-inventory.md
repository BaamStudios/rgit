# Gitea command inventory

Every `git` call Gitea 1.24.6 makes on the server during the M5 scenario
(issue #71, `scripts/acceptance/m5-gitea-server.sh`), recorded with the
reference git as the server git (baseline run) by the logging wrapper of
`scripts/acceptance/m5-install-server-git.sh`. Regenerate with

```sh
scripts/acceptance/m5-gitea-server.sh --server reference --rgit <musl rgit> --inventory calls.txt
```

The scenario covers repository creation with auto-init, push, clone and
fetch over HTTPS, OpenSSH and Gitea's built-in SSH server (protocol v2 and
v0), file edits through the API, the web views (tree, file, log, commit,
diff, patch, blame, compare, graph, branches, tags, releases, activity),
zip/tar.gz/bundle archives, tags and releases, branch create/rename/delete,
pull requests with every merge style (merge, rebase, rebase-merge, squash,
fast-forward-only) and a conflicting one, AGit (`refs/for/main`), a
protected branch, a server-side `pre-receive` hook, the wiki, a fork, a pull
mirror with mirror-sync, push-to-create, and the cron tasks
`repo_health_check`, `git_gc_repos`, `resync_all_hooks`,
`reinit_missing_repos`. LFS is off.

Not covered yet (follow-ups of #71): Gitea's own integration test suite,
code search indexer, LFS, push mirrors, repository transfer and
signing (`gpg`).

## Notes

- **Global options.** Every call carries `-c protocol.version=2 -c
  credential.helper=`; they are left out below.
- **Protocol versions.** Gitea passes the client's `GIT_PROTOCOL` on to
  `upload-pack` for smart HTTP, for OpenSSH (the image's `sshd_config` has
  `AcceptEnv GIT_PROTOCOL`) and for its built-in SSH server (observed:
  `version=2`). A client that asks for v0/v1 (`-c protocol.version=0`)
  reaches `upload-pack` without `GIT_PROTOCOL`.
- **`git version`.** `git version 2.56.1.rgit.0.1.0` passes Gitea's version
  check (minimum 2.0; features gated on 2.29+ such as AGit and SHA-256
  are enabled).
- **stdin.** `hash-object --stdin`, `update-index --index-info`,
  `check-attr --stdin`, `cat-file --batch`/`--batch-check` and the
  `--stateless-rpc` services read stdin. The `cat-file` and `check-attr`
  processes are long-running; Gitea kills them at the end of a request, so
  the wrapper records each call by its start line.
- **Exit codes.** Non-zero exits that are normal Gitea probing (also with the
  reference git): `config --get` of unset keys (1), `show-ref --verify` of a
  missing ref (128), `ls-tree` of a not yet existing wiki branch (128),
  `merge-file` reporting conflicts (1), `ls-remote` of a wiki over Gitea's
  self-signed HTTPS during migration (128).

## Calls

"With rgit" is the result of the run with rgit as Gitea's only git
(`--server rgit`): "works" means every call of that command succeeded and
the scenario steps that use it pass.

| Command | Covered by | With rgit | Calls | Forms (options; ids, paths, refs and text masked) |
|---|---|---|---:|---|
| `add` | #341 | works | 2 | `--all` ×2 |
| `archive` | #139 | works | 6 | `--prefix=repo-git/ --format=tar.gz <id>` ×2<br>`--prefix=repo-rgit/ --format=tar.gz <id>` ×2<br>`--prefix=repo-git/ --format=zip <id>`<br>`--prefix=repo-rgit/ --format=zip <id>` |
| `blame` | #133 | works | 4 | `--porcelain <id> -- <arg>...` ×4 |
| `branch` | #48 | works | 16 | `-D -- <arg>...` ×12<br>`-m <arg>...` ×4 |
| `bundle` | #172 | works | 2 | `<arg>... <path> <arg>...` ×2 |
| `cat-file` | #15 | works | 1560 | `--batch` ×788<br>`--batch-check` ×768<br>`-e <arg>...` ×4 |
| `check-attr` | #38 | works | 44 | `-z --source <id> <arg>... --stdin` ×36<br>`-z --source <id> <arg>... -- <arg>...` ×8 |
| `checkout` | #42 | works | 8 | `<arg>...` ×4<br>`-b <arg>...` ×4 |
| `clone` | #64 | works | 18 | `-s -b <arg>... <path> <path> --bare` ×8<br>`--bare <path> <path>` ×2<br>`--bare -s -b <arg>... -- <path> <path>` ×2<br>`--bare -s -- <path> <path>` ×2<br>`-c <arg>... --mirror --quiet -- <url> <path>` ×2<br>`<path> <path>` ×2 |
| `commit` | #45 | works | 8 | `--message=<arg> --no-gpg-sign` ×4<br>`--author=<arg> --message=<arg> --no-gpg-sign` ×2<br>`--message=<arg> --author=<arg> --no-gpg-sign` ×2 |
| `commit-graph` | #22 | works | 4 | `<arg>...` ×4 |
| `commit-tree` | #123 | works | 12 | `<id> -p <id> --no-gpg-sign` ×8<br>`<id> --no-gpg-sign` ×2<br>`<id> -p <arg>... --no-gpg-sign` ×2 |
| `config` | #99 | works | 104 | `--local <arg>...` ×50<br>`--get commit.gpgsign` ×24<br>`--add remote.origin.fetch <ref>` ×2<br>`--global --add receive.procReceiveRefs <ref>`<br>`--global --add safe.directory <arg>...`<br>`--global core.commitGraph <arg>...`<br>`--global core.logallrefupdates <arg>...`<br>`--global core.quotePath <arg>...`<br>`--global diff.algorithm <arg>...`<br>`--global fetch.writeCommitGraph <arg>...`<br>`--global gc.reflogexpire <arg>...`<br>`--global gc.writeCommitGraph <arg>...`<br>`--global --get core.commitGraph`<br>`--global --get core.logallrefupdates`<br>`--global --get core.quotePath`<br>`--global --get diff.algorithm`<br>`--global --get fetch.writeCommitGraph`<br>`--global --get gc.reflogexpire`<br>`--global --get gc.writeCommitGraph`<br>`--global --get receive.advertisePushOptions`<br>`--global --get receive.procReceiveRefs <ref>`<br>`--global --get safe.directory <arg>...`<br>`--global --get uploadpack.allowAnySHA1InWant`<br>`--global --get uploadpack.allowfilter`<br>`--global --get user.email`<br>`--global --get user.name`<br>`--global receive.advertisePushOptions <arg>...`<br>`--global uploadpack.allowAnySHA1InWant <arg>...`<br>`--global uploadpack.allowfilter <arg>...`<br>`--global user.email <arg>...`<br>`--global user.name <arg>...` |
| `diff` | #144 | works | 318 | `--shortstat <id> <id>` ×232<br>`-z --name-only <arg>... --` ×34<br>`--src-prefix=\a/ --dst-prefix=\b/ -M <id> <id> --` ×24<br>`-z --name-only <ref> --` ×12<br>`-p <arg>...` ×10<br>`-M <id> <id> --` ×6 |
| `diff-tree` | #144 | works | 24 | `--raw -r --find-renames --root <id> <id>` ×14<br>`--no-commit-id --name-only -r -r -z --root <arg>...` ×10 |
| `fetch` | #62 | works | 66 | `<arg>... --no-tags --no-write-commit-graph -- <arg>...` ×32<br>`--no-tags --no-write-commit-graph <arg>... <ref>` ×30<br>`--no-tags --no-write-commit-graph <arg>...` ×2<br>`--prune --tags <arg>...` ×2 |
| `for-each-ref` | #102 | works | 82 | `--format=%(refname:strip=2) --count=2 --contains <id> <ref>` ×56<br>`--format=%(objectname) %(refname) <ref> --sort=-committerdate` ×12<br>`--format=objecttype %(objecttype)%00refname:lstrip=2 %(refname:lstrip=2)%00object %(object)%00objectname %(objectname)%00creator %(creator)%00contents %(contents)%00contents:signature %(contents:signature)%00%00 --sort -*creatordate <ref>` ×8<br>`(none)` ×2<br>`--format=%(objectname) %(refname)` ×2<br>`--sort=-committerdate <ref> --count <arg>... --format=%(committerdate)` ×2 |
| `format-patch` | #135 | works | 4 | `--no-signature --stdout <arg>... --` ×4 |
| `fsck` | #95 | works | 4 | `(none)` ×4 |
| `gc` | #92 | works | 8 | `(none)` ×8 |
| `git-receive-pack` | #69, #175 | works | 6 | `<arg>...` ×6 |
| `git-upload-pack` | #68 (v2), #174 (v0/v1) | works | 18 | `<arg>...` ×18 |
| `hash-object` | #15 | works | 784 | `--stdin` ×772<br>`-w --stdin` ×12 |
| `init` | #27 | works | 41 | `--object-format <arg>...` ×32<br>`--object-format <arg>... --bare` ×7<br>`--bare` ×2 |
| `log` | #78, #126 | works | 368 | `--name-status -m --pretty=format: --first-parent --no-renames -z -1 <id>` ×232<br>`--pretty=format:%H <arg>... --` ×46<br>`-1 --pretty=format:%H <id> -- <arg>...` ×30<br>`--pretty=format:%H -10 <id>` ×22<br>`--name-status -c --format=commit%x00%H %P%x00 --parents --no-renames -t -z <id> -- <arg>...` ×16<br>`-1 --pretty=format:%H -- <arg>...` ×12<br>`--skip=0 --max-count=50 --pretty=format:%H <id>` ×4<br>`--graph --date-order --decorate=full --tags --branches -C -M --date=<arg> -n 100 --pretty=format:DATA:%D\|%H\|%ad\|%h\|%s` ×2<br>`--numstat --no-merges --pretty=format:---%n%h%n%aN%n%aE%n --date=<arg> --since=<arg> --branches=*` ×2<br>`--numstat --no-merges --pretty=format:---%n%h%n%aN%n%aE%n --date=<arg> --since=<arg> --first-parent <arg>...` ×2 |
| `ls-files` | #39 | works | 29 | `-u -z` ×19<br>`-z -- <arg>...` ×10 |
| `ls-remote` | #63 | works | 4 | `-q -h <url> <arg>...` ×4 |
| `ls-tree` | #88 | works | 28 | `-t -r --long <id>` ×24<br>`-z --name-only -- <arg>...` ×4 |
| `merge` | #397 | works | 10 | `--ff-only <arg>...` ×4<br>`--no-ff --no-commit <arg>...` ×4<br>`--squash <arg>...` ×2 |
| `merge-base` | #50 | works | 87 | `-- <arg>...` ×31<br>`--is-ancestor <id> <id>` ×22<br>`-- <arg>... <ref>` ×20<br>`-- <ref> <ref>` ×12<br>`-- <id> <arg>...` ×2 |
| `merge-file` | #56 | works | 2 | `<path> <path> <path>` ×2 |
| `name-rev` | #131 | works | 24 | `--exclude <ref> --name-only --no-undefined <id>` ×24 |
| `push` | #165 | works | 38 | `-f -- <path> <ref>` ×12<br>`<arg>... <ref>` ×10<br>`-- <path> <ref>` ×10<br>`-- <arg>... <ref>` ×4<br>`<arg>...` ×2 |
| `read-tree` | #107 | works | 39 | `-m <id> <arg>...` ×19<br>`<arg>...` ×18<br>`<id>` ×2 |
| `rebase` | #154 | works | 4 | `<arg>...` ×4 |
| `receive-pack` | #69, #175 | works | 51 | `--stateless-rpc <path>` ×26<br>`--stateless-rpc --advertise-refs <arg>...` ×25 |
| `remote` | #159 | works | 66 | `<arg>... <path>` ×32<br>`<arg>... -t <arg>... -m <arg>... <path>` ×32<br>`<arg>...` ×2 |
| `reset` | #49 | works | 2 | `--soft <id>` ×2 |
| `rev-list` | #46 | works | 168 | `--count <id>` ×55<br>`<arg>...` ×26<br>`--count --left-right <arg>... --` ×21<br>`-1 <id> -- <arg>...` ×14<br>`<arg>... --not <arg>...` ×14<br>`--count <arg>... -- <arg>...` ×12<br>`--max-count <arg>... --skip <arg>...` ×10<br>`--count --no-merges --branches=* --date=<arg> --since=<arg>` ×4<br>`--git-dir=<path> -n <arg>... --all` ×4<br>`--max-count=50 --skip=0 <arg>... -- <arg>...` ×4<br>`--all --count` ×2<br>`--max-count=1 <id> <arg>...` ×2 |
| `rev-parse` | #32 | works | 1694 | `(none)` ×1560<br>`<arg>...` ×82<br>`<ref>` ×44<br>`<id>` ×6<br>`--verify <arg>...` ×2 |
| `show` | #126 | works | 2 | `--format=%B -s` ×2 |
| `show-ref` | #29 | works | 88 | `--verify -- <ref>` ×80<br>`--tags -- <arg>...` ×8 |
| `symbolic-ref` | #29 | works | 40 | `<arg>... <ref>` ×38<br>`<arg>...` ×2 |
| `tag` | #130 | works | 8 | `-- <arg>... <id>` ×4<br>`-a -m <arg>... -- <arg>... <id>` ×2<br>`-d -- <arg>...` ×2 |
| `unpack-file` | #158 | missing: #158 (PR #691) | 6 | `<id>` ×6 |
| `update-index` | #39 | works | 52 | `--add --replace -z --index-info` ×23<br>`--remove -z --index-info` ×21<br>`--add --replace --cacheinfo <arg>... <id> <arg>...` ×8 |
| `update-ref` | #29 | works | 2 | `<ref> <id>` ×2 |
| `update-server-info` | #71 (this PR) | works | 8 | `(none)` ×8 |
| `upload-pack` | #68 (v2), #174 (v0/v1) | v0/v1 requests fail: #174 (PR #700) | 94 | `--stateless-rpc <path>` ×54<br>`--stateless-rpc --advertise-refs <arg>...` ×40 |
| `version` | #201 | works | 26 | `(none)` ×26 |
| `write-tree` | #123 | works | 29 | `(none)` ×29 |
