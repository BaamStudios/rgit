#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
#
# rgit vs. git benchmark suite (issue #77).
#
#   bench/run.sh --repos DIR --rgit PATH --git PATH --out bench/results/<version>.json
#
# DIR holds bare clones named git.git, gitea.git and linux.git (missing ones are
# skipped). Every scenario of bench/scenarios.sh runs once per tool as a
# validation run (exit status, stdout checksum, peak RSS via /usr/bin/time -v)
# and is then timed with hyperfine (warm cache). The JSON result goes to --out,
# the Markdown table (bench/render.sh) next to it with the suffix .md.
#
# Options:
#   --repos DIR        directory with the bare source repositories (required)
#   --rgit PATH        rgit binary (required)
#   --git PATH         reference git binary; also runs all setup steps (required)
#   --out FILE         result JSON (required)
#   --work DIR         scratch space for worktrees and copies (default: DIR/../work)
#   --label TEXT       version label in the result (default: rgit --version)
#   --rgit-commit SHA  commit rgit was built from, recorded in the result
#   --only LIST        comma-separated scenarios to run; each item is a scenario
#                      name or "<scenario>/<repo>" (extended regexes, matched
#                      whole), e.g. --only fetch-noop,status-.*,log-L/linux
#   --repo-list LIST   comma-separated repositories (default: git,gitea,linux)
#   --tool both|rgit   time both tools (default) or only rgit; with rgit the
#                      git side of every scenario comes from --baseline
#   --baseline FILE    earlier result JSON of this suite (required with --tool rgit)
#   --runs N           timed runs per tool (default 10)
#   --warmup N         warmup runs per tool (default 2)
#   --slow SECONDS     scenarios whose validation run takes longer use
#                      --slow-runs runs and no warmup (default 60, 3 runs)
#   --slow-runs N
#   --timeout SECONDS  limit for one validation run (default 1800)
#   --dirty-files N    files modified for status-dirty, add-A, commit (default 1000)
#   --check CMD        shell command run before and after every scenario; when
#                      it fails after a scenario, the scenario is repeated (up
#                      to 3 times). Used to make sure nothing else loads the machine.
#
# Requires: bash, hyperfine, jq, GNU time (/usr/bin/time), coreutils.
set -euo pipefail

bench_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

die() {
    echo "run.sh: $*" >&2
    exit 1
}

repos='' rgit='' git='' out='' work='' label='' rgit_commit='' only='' check='' tool_sel=both baseline=''
repo_list=git,gitea,linux runs=10 warmup=2 slow=60 slow_runs=3 timeout_s=1800 dirty_files=1000
while (($#)); do
    case $1 in
    --repos) repos=$2 ;;
    --rgit) rgit=$2 ;;
    --git) git=$2 ;;
    --out) out=$2 ;;
    --work) work=$2 ;;
    --label) label=$2 ;;
    --rgit-commit) rgit_commit=$2 ;;
    --only) only=$2 ;;
    --repo-list) repo_list=$2 ;;
    --runs) runs=$2 ;;
    --warmup) warmup=$2 ;;
    --slow) slow=$2 ;;
    --slow-runs) slow_runs=$2 ;;
    --timeout) timeout_s=$2 ;;
    --dirty-files) dirty_files=$2 ;;
    --check) check=$2 ;;
    --tool) tool_sel=$2 ;;
    --baseline) baseline=$2 ;;
    -h | --help)
        sed -n '3,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d; s/^# \{0,1\}//'
        exit 0
        ;;
    *) die "unknown option '$1' (see --help)" ;;
    esac
    shift 2 || die "option '$1' needs a value"
done
[[ -n $repos && -n $rgit && -n $git && -n $out ]] || die "--repos, --rgit, --git and --out are required"
case $tool_sel in
both) ;;
rgit)
    [[ -n $baseline ]] || die "--tool rgit needs --baseline FILE (the git side comes from there)"
    [[ -r $baseline ]] || die "cannot read baseline $baseline"
    baseline=$(cd "$(dirname "$baseline")" && echo "$(pwd)/$(basename "$baseline")")
    ;;
*) die "--tool must be 'both' or 'rgit'" ;;
esac
for tool in hyperfine jq /usr/bin/time timeout sha256sum; do
    command -v "$tool" >/dev/null || die "missing dependency: $tool"
done

abspath() { (cd "$(dirname "$1")" && echo "$(pwd)/$(basename "$1")"); }
repos=$(cd "$repos" && pwd)
rgit=$(abspath "$rgit")
git=$(abspath "$git")
[[ -x $rgit ]] || die "not executable: $rgit"
[[ -x $git ]] || die "not executable: $git"
[[ -n $work ]] || work=$(dirname "$repos")/work
mkdir -p "$work" "$(dirname "$out")"
work=$(cd "$work" && pwd)
out=$(abspath "$out")
md_out=${out%.json}.md
raw=$work/raw/$(basename "${out%.json}")
mkdir -p "$raw" "$work/home/.config"

# Isolated, deterministic environment for both tools. The reference git's bin
# directory comes first in PATH so that helpers both tools spawn (for example
# git-upload-pack behind a local fetch) are the reference ones.
export BENCH_GIT=$git
export HOME=$work/home XDG_CONFIG_HOME=$work/home/.config GIT_CONFIG_NOSYSTEM=1
export LC_ALL=C TZ=UTC PAGER=cat GIT_PAGER=cat GIT_TERMINAL_PROMPT=0
export GIT_AUTHOR_NAME=bench GIT_AUTHOR_EMAIL=bench@example.com
export GIT_COMMITTER_NAME=bench GIT_COMMITTER_EMAIL=bench@example.com
export GIT_AUTHOR_DATE='2026-01-01T00:00:00Z' GIT_COMMITTER_DATE='2026-01-01T00:00:00Z'
export PATH=$(dirname "$git"):$PATH
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY GIT_EXEC_PATH
cat >"$HOME/.gitconfig" <<'EOF'
[gc]
	auto = 0
[maintenance]
	auto = false
[core]
	pager = cat
[advice]
	detachedHead = false
[init]
	defaultBranch = main
EOF

log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" >&2; }

# Quote for hyperfine --shell=none (POSIX single quotes).
quote() {
    local a s=''
    for a; do
        s+="'${a//\'/\'\\\'\'}' "
    done
    printf '%s' "${s% }"
}

# ---------------------------------------------------------------- setup

setup_repo() {
    local r=$1
    BARE=$repos/$r.git
    WT=$work/wt/$r DIRTY=$work/dirty/$r MUT=$work/mut/$r TMP=$work/tmp/$r
    OBJS=$work/input/$r.objects DIRTY_FILES=$dirty_files
    mkdir -p "$work/wt" "$work/dirty" "$work/mut" "$TMP" "$work/input"
    # The scenarios hard-link the source; a tool that writes a file in place
    # must fail instead of changing it.
    find "$BARE" -type f -perm /222 -exec chmod a-w {} +
    local d
    for d in "$WT" "$DIRTY" "$MUT"; do
        [[ -d $d ]] || { log "setup: clone $r -> $d"; "$git" clone -q "$BARE" "$d"; }
    done
    BASE=$("$git" -C "$WT" rev-parse HEAD)
    bash "$bench_dir/lib/prep.sh" reset "$WT" "$BASE"
    bash "$bench_dir/lib/prep.sh" reset "$MUT" "$BASE"
    bash "$bench_dir/lib/prep.sh" dirty "$DIRTY" "$BASE" "$dirty_files"
    [[ -s $OBJS ]] || "$git" -C "$BARE" rev-list --objects --all >"$OBJS"
    PACK=$(ls -S "$BARE"/objects/pack/*.pack | head -n 1)
    # Let racily clean index entries age, then settle both indexes.
    sleep 2
    "$git" -C "$WT" status >/dev/null
    "$git" -C "$DIRTY" status >/dev/null
}

# ---------------------------------------------------------------- one run

# validate TOOL BIN: one untimed run. Writes $raw/<id>.<tool>.val.json.
validate() {
    local tool=$1 bin=$2 base=$raw/$id.$1
    local in=/dev/null
    [[ -n $sc_input ]] && in=$sc_input
    if [[ -n $sc_prep ]]; then bash "$bench_dir/lib/prep.sh" $sc_prep >&2; fi
    local t0 t1 ec sha
    t0=$(date +%s.%N)
    set +e
    timeout "$timeout_s" /usr/bin/time -v -o "$base.time" "$bin" "${sc_args[@]}" <"$in" 2>"$base.err" |
        sha256sum >"$base.sha"
    ec=${PIPESTATUS[0]}
    set -e
    t1=$(date +%s.%N)
    if [[ -n $sc_cleanup ]]; then bash "$bench_dir/lib/prep.sh" $sc_cleanup >&2; fi
    sha=$(cut -c1-16 "$base.sha")
    local rss
    rss=$(sed -n 's/.*Maximum resident set size (kbytes): //p' "$base.time" 2>/dev/null)
    jq -n --argjson exit "$ec" --arg sha "$sha" --argjson wall "$(awk -v a="$t0" -v b="$t1" 'BEGIN { printf "%.3f", b - a }')" \
        --argjson rss "${rss:-null}" --arg err "$(tail -n 5 "$base.err" | cut -c1-300)" \
        '{exit: $exit, timed_out: ($exit == 124), wall_s: $wall, max_rss_kb: $rss,
          stdout_sha256: $sha, stderr_tail: $err}' >"$base.val.json"
    echo "$ec"
}

# time TOOL BIN RUNS WARMUP: hyperfine. Writes $raw/<id>.<tool>.hf.json.
time_tool() {
    local tool=$1 bin=$2 n=$3 w=$4 base=$raw/$id.$1
    local -a hf=(hyperfine -N --style basic --runs "$n" --warmup "$w" --export-json "$base.hf.json")
    [[ -n $sc_prep ]] && hf+=(--prepare "bash $(quote "$bench_dir/lib/prep.sh" $sc_prep)")
    [[ -n $sc_cleanup ]] && hf+=(--cleanup "bash $(quote "$bench_dir/lib/prep.sh" $sc_cleanup)")
    [[ -n $sc_input ]] && hf+=(--input "$sc_input")
    hf+=(-n "$tool" -- "$(quote "$bin" "${sc_args[@]}")")
    "${hf[@]}" >"$base.hf.log" 2>&1
}

tool_json() {
    local base=$raw/$id.$1
    if [[ -e $base.base.json ]]; then
        jq '. + {from_baseline: true}' "$base.base.json"
    elif [[ -s $base.hf.json ]]; then
        jq --slurpfile v "$base.val.json" '.results[0] | {
            median_s: .median, mean_s: .mean, stddev_s: .stddev, min_s: .min, max_s: .max,
            user_s: .user, system_s: .system, runs: (.times | length)} + $v[0]' "$base.hf.json"
    else
        jq '{median_s: null} + .' "$base.val.json"
    fi
}

# selected NAME REPO: does --only select this scenario? Each comma-separated
# item is an extended regex matched against the whole scenario name or the
# whole "<scenario>/<repo>".
selected() {
    [[ -z $only ]] && return 0
    local item items
    IFS=, read -r -a items <<<"$only"
    for item in "${items[@]}"; do
        if [[ $1 =~ ^($item)$ || "$1/$2" =~ ^($item)$ ]]; then
            return 0
        fi
    done
    return 1
}

checked() {
    [[ -z $check ]] && return 0
    bash -c "$check" >&2
}

run_scenario() {
    local attempt
    for attempt in 1 2 3; do
        until checked; do
            log "check failed before $id; retrying in 30 s"
            sleep 30
        done
        run_scenario_once
        if checked; then
            return 0
        fi
        log "check failed after $id (attempt $attempt); repeating the scenario"
    done
    die "check keeps failing; aborting"
}

run_scenario_once() {
    rm -f "$raw/$id".*
    local ec_git ec_rgit
    if [[ $tool_sel == rgit ]]; then
        # git side from the baseline result; its validation data stands in
        # for the git validation run.
        jq --arg s "$sc_name" --arg r "$REPO" \
            'first(.scenarios[] | select(.scenario == $s and .repo == $r) | .git) // {}' \
            "$baseline" >"$raw/$id.git.base.json"
        jq '{wall_s: (.wall_s // 0), stdout_sha256: .stdout_sha256}' "$raw/$id.git.base.json" \
            >"$raw/$id.git.val.json"
        ec_git=$(jq -r '.exit // "none"' "$raw/$id.git.base.json")
    else
        ec_git=$(validate git "$git")
    fi
    ec_rgit=$(validate rgit "$rgit")
    local wall n w
    wall=$(jq -s 'map(.wall_s) | max' "$raw/$id.git.val.json" "$raw/$id.rgit.val.json")
    n=$runs w=$warmup
    if awk -v t="$wall" -v s="$slow" 'BEGIN { exit !(t > s) }'; then n=$slow_runs w=0; fi
    if awk -v t="$wall" -v s="$slow" 'BEGIN { exit !(t > 10 * s) }'; then n=1 w=0; fi
    local status=ok
    if [[ $ec_git == none ]]; then
        status="no git result in the baseline"
    elif [[ $ec_git != 0 ]]; then
        status="git failed (exit $ec_git)"
    elif [[ $tool_sel != rgit ]]; then
        time_tool git "$git" "$n" "$w" || status="hyperfine failed for git"
    fi
    if [[ $ec_rgit == 124 ]]; then
        status="rgit timed out (> ${timeout_s} s)"
    elif [[ $ec_rgit != 0 ]]; then
        status="rgit failed (exit $ec_rgit)"
    elif [[ $status == ok || $tool_sel == rgit ]]; then
        time_tool rgit "$rgit" "$n" "$w" || status="hyperfine failed for rgit"
    fi
    local same
    same=$(jq -s '.[0].stdout_sha256 == .[1].stdout_sha256' "$raw/$id.git.val.json" "$raw/$id.rgit.val.json")
    if [[ $status == ok && $same != true ]]; then status="ok, output differs"; fi
    jq -n --arg name "$sc_name" --arg repo "$REPO" --arg status "$status" \
        --arg cmd "$(quote "${sc_args[@]}")" --arg prep "$sc_prep" \
        --argjson git "$(tool_json git)" --argjson rgit "$(tool_json rgit)" \
        '{scenario: $name, repo: $repo, command: $cmd, prepare: $prep, status: $status,
          git: $git, rgit: $rgit,
          ratio: (if $git.median_s and $rgit.median_s then $rgit.median_s / $git.median_s else null end)}' \
        >"$raw/$id.result.json"
    log "$id: $status $(jq -r '"git \(.git.median_s) s, rgit \(.rgit.median_s) s, ratio \(.ratio)"' "$raw/$id.result.json")"
}

# sc NAME [--prep A] [--cleanup A] [--input F] -- ARGS...: called by scenarios.sh.
sc() {
    sc_name=$1
    shift
    sc_prep='' sc_cleanup='' sc_input=''
    while [[ $1 != -- ]]; do
        case $1 in
        --prep) sc_prep=$2 ;;
        --cleanup) sc_cleanup=$2 ;;
        --input) sc_input=$2 ;;
        *) die "sc $sc_name: unknown option $1" ;;
        esac
        shift 2
    done
    shift
    sc_args=("$@")
    id=$sc_name.$REPO
    selected "$sc_name" "$REPO" || return 0
    log "== $sc_name / $REPO"
    run_scenario
    results+=("$raw/$id.result.json")
}

# ---------------------------------------------------------------- main

results=()
started=$(date -u +%Y-%m-%dT%H:%M:%SZ)
IFS=, read -r -a repo_names <<<"$repo_list"
for REPO in "${repo_names[@]}"; do
    if [[ ! -d $repos/$REPO.git ]]; then
        log "skip $REPO: $repos/$REPO.git does not exist"
        continue
    fi
    any=false
    for name in $(sed -n 's/^sc \([^ ]*\).*/\1/p' "$bench_dir/scenarios.sh"); do
        if selected "$name" "$REPO"; then any=true; fi
    done
    $any || continue
    setup_repo "$REPO"
    # shellcheck source=bench/scenarios.sh
    source "$bench_dir/scenarios.sh"
done

governor=$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo unknown)
jq -n --arg label "${label:-$("$rgit" --version | head -n 1)}" --arg rgit_commit "$rgit_commit" \
    --arg started "$started" --arg finished "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg host "$(uname -n)" --arg kernel "$(uname -r)" \
    --arg cpu "$(sed -n 's/^model name[[:space:]]*: //p' /proc/cpuinfo | sort -u | head -n 1)" \
    --argjson cpus "$(nproc)" --argjson mem_kb "$(awk '/MemTotal/ {print $2}' /proc/meminfo)" \
    --arg governor "$governor" --arg git_version "$("$git" --version)" \
    --arg rgit_version "$("$rgit" --version | head -n 1)" --arg hyperfine "$(hyperfine --version)" \
    --argjson runs "$runs" --argjson warmup "$warmup" --argjson slow "$slow" --argjson slow_runs "$slow_runs" \
    --argjson dirty_files "$dirty_files" --arg tool "$tool_sel" --arg baseline "$baseline" \
    --slurpfile scenarios <(cat /dev/null "${results[@]}") \
    '{label: $label, rgit_commit: $rgit_commit, started: $started, finished: $finished,
      machine: {host: $host, kernel: $kernel, cpu: $cpu, cpus: $cpus, mem_kb: $mem_kb, governor: $governor},
      tools: {git: $git_version, rgit: $rgit_version, hyperfine: $hyperfine},
      settings: ({runs: $runs, warmup: $warmup, slow_threshold_s: $slow, slow_runs: $slow_runs,
                 dirty_files: $dirty_files, cache: "warm", tool: $tool}
                 + if $tool == "rgit" then {baseline: $baseline,
                     notes: "only rgit was timed; the git columns come from \($baseline | split("/") | last)"}
                   else {} end),
      scenarios: $scenarios}' >"$out"
bash "$bench_dir/render.sh" "$out" >"$md_out"
log "wrote $out and $md_out"
