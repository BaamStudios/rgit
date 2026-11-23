# Benchmarks: rgit vs. git

A hyperfine-based suite that runs the same operations with rgit and with the
reference git on real repositories (git, gitea, linux) and records wall time,
CPU time and peak memory per release (issue #77).

| Path | Contents |
|---|---|
| `run.sh` | Driver: setup, one validation run per tool, hyperfine timing, result JSON |
| `scenarios.sh` | The scenarios and the per-repository parameters (ranges, files, patterns) |
| `render.sh` | Renders a result JSON as a Markdown table |
| `lib/prep.sh` | Untimed preparation steps (fresh copies, reset or dirty worktrees) |
| `results/<version>.json`, `.md` | Results per version |

## Running

Requirements: `hyperfine`, `jq`, GNU `time` (`/usr/bin/time`), bash, coreutils,
about 25 GB of disk for the three repositories and their worktrees.

```sh
mkdir -p ~/bench/repos && cd ~/bench/repos
git clone --bare https://github.com/git/git.git git.git
git clone --bare https://github.com/go-gitea/gitea.git gitea.git
git clone --bare https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git linux.git

cargo build --release -p rgit-cli
bench/run.sh --repos ~/bench/repos --rgit target/release/rgit \
    --git /path/to/git-2.56.1/bin/git --rgit-commit "$(git rev-parse HEAD)" \
    --label v0.1.0 --out bench/results/v0.1.0.json
```

`--only REGEX` selects scenarios by `<scenario>/<repo>`, `--repo-list` selects
repositories; `bench/run.sh --help` lists all options.

## Method

- Both tools run in the same isolated environment: own `HOME` with a small
  `.gitconfig` (`gc.auto=0`, `maintenance.auto=false`), `GIT_CONFIG_NOSYSTEM=1`,
  `LC_ALL=C`, fixed author/committer identity and dates. The reference git's
  `bin/` comes first in `PATH`, so helpers that either tool spawns (for example
  `git-upload-pack` behind a local fetch) are the same.
- Setup (with the reference git): three worktree clones per repository: a
  clean one for read-only scenarios, a dirty one (`--dirty-files` modified
  files plus as many untracked files), and one that mutating scenarios reset
  before every run. The source repositories are made read-only; copies for
  `repack`/`gc` are hard-linked.
- Each scenario first runs once per tool untimed: exit status, a checksum of
  stdout (the table flags "output differs"), and peak RSS via `/usr/bin/time -v`.
  A failing rgit is reported in the table and under "Failures" instead of timed.
  "output differs" is expected for `pack-objects` (pack bytes depend on delta
  choices); elsewhere it points at a behaviour difference worth a look.
- Timing: `hyperfine -N` with warm cache, `--runs 10 --warmup 2` by default;
  scenarios whose validation run takes more than 60 s get 3 runs without
  warmup, more than 600 s a single run. Preparation steps run with
  hyperfine's `--prepare` and are not timed.
- `--check CMD` runs before and after every scenario; when it fails afterwards
  the scenario is repeated. Use it to make sure no other workload ran meanwhile.
