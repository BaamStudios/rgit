#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
#
# Render a result file of bench/run.sh as a Markdown table.
#
#   bench/render.sh bench/results/<version>.json > bench/results/<version>.md
set -euo pipefail

[[ $# -eq 1 ]] || { echo "usage: render.sh RESULT.json" >&2; exit 2; }

jq -r '
def t: if . == null then "–"
       elif . < 1 then "\(. * 1000 | round) ms"
       elif . < 100 then "\(. * 100 | round / 100) s"
       else "\(. | round) s" end;
def cpu: if .user_s == null then "–" else (.user_s + .system_s | t) end;
def rss: if .max_rss_kb == null then "–"
         elif .max_rss_kb < 1048576 then "\(.max_rss_kb / 1024 | round) MB"
         else "\(.max_rss_kb / 1048576 * 10 | round / 10) GB" end;
def ratio: if . == null then "–" else "\(. * 100 | round / 100)×" end;
"# rgit vs. git: \(.label)",
"",
"- rgit: \(.tools.rgit)\(if .rgit_commit != "" then " (commit `\(.rgit_commit[0:12])`)" else "" end)",
"- git: \(.tools.git) (reference build)",
"- Machine: \(.machine.host), \(.machine.cpu), \(.machine.cpus) CPUs, \(.machine.mem_kb / 1048576 | round) GB RAM, kernel \(.machine.kernel), CPU governor \(.machine.governor)",
"- \(.tools.hyperfine), warm cache, \(.settings.runs) runs after \(.settings.warmup) warmups (\(.settings.slow_runs) runs without warmup when one validation run takes over \(.settings.slow_threshold_s) s); \(.settings.dirty_files) files dirty for status-dirty, add-A, commit",
(.settings.notes // empty | "- Note: \(.)"),
"- Run \(.started) – \(.finished)",
"",
"Times are hyperfine medians (wall clock); CPU is mean user + system time per run; RSS is the peak resident set size of one validation run (`/usr/bin/time -v`). Ratio = rgit / git (below 1 means rgit is faster).",
"",
"| Scenario | Repo | git | rgit | rgit/git | CPU git | CPU rgit | RSS git | RSS rgit | Runs | Status |",
"|---|---|--:|--:|--:|--:|--:|--:|--:|--:|---|",
(.scenarios[] |
  "| \(.scenario) | \(.repo) | \(.git.median_s | t) | \(.rgit.median_s | t) | \(.ratio | ratio) | \(.git | cpu) | \(.rgit | cpu) | \(.git | rss) | \(.rgit | rss) | \(.git.runs // .rgit.runs // "–") | \(.status) |"),
([.scenarios[] | select(.status | startswith("ok") | not)] |
  if length == 0 then empty else
    "", "## Failures", "",
    (.[] | "- **\(.scenario) / \(.repo)** (`\(.command)`): \(.status)" +
      (if .rgit.stderr_tail != "" then "\n\n  ```\n  \(.rgit.stderr_tail | gsub("\n"; "\n  "))\n  ```" else "" end))
  end)
' "$1"
