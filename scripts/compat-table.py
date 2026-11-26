#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Generate the "Compatibility with git" table of README.md.

For every command of git's command list (upstream/git/command-list.txt, the
reference version) the table shows whether rgit implements it, and how many
cases of git's own test suite (ported in rgit-conformance) cover it and pass
in strict mode (everything runs through rgit).

Inputs
  * the command list of the reference git (a data file, read as data only);
  * the rgit binary: its command dispatch is asked, never guessed
    (`rgit --list-cmds=main`, then `rgit <cmd> -h` for every other command);
  * the conformance results: by default the results artifact of the newest
    successful Conformance run on main (downloaded with `gh run download`),
    or a local `results/` directory of rgit-conformance's `./run`.

Attribution of test cases to commands (each valid case counts exactly once,
so the column sums equal the run's totals):
  1. a script named after a command (`tNNNN-<cmd>[-...].sh`, also
     `tNNNN-git-<cmd>...`) attributes all its cases to that command (the
     longest command name that matches wins);
  2. in other scripts each case goes to the command it exercises, taken from
     the fallback pass's per-case columns "forwarded subcommands" and "routed
     implemented.txt lines"; of several commands the most specific one (the
     one exercised by the fewest cases overall) wins;
  3. cases that call no git command (test-tool, shell checks only) land in
     a row per test family of git's t/README (the script's first digit).

Usage
  scripts/compat-table.py [--update-readme] [--json docs/compat/compat.json]
                          [--rgit target/debug/rgit] [--run <id>] [results-dir]

With a local results directory pass --commit/--date to name what was run.
Python 3 standard library only.
"""

import argparse
import datetime
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from collections import Counter, defaultdict
from pathlib import Path

REPO = "BaamStudios/rgit"
ISSUES_URL = f"https://github.com/{REPO}/issues"
START = "<!-- compat-table:start -->"
END = "<!-- compat-table:end -->"
NO_COMMAND = "(no git command)"

# Categories of command-list.txt, in the order `git help -a` lists them.
CATEGORIES = [
    ("mainporcelain", "Main porcelain commands"),
    ("ancillarymanipulators", "Ancillary commands: manipulators"),
    ("ancillaryinterrogators", "Ancillary commands: interrogators"),
    ("foreignscminterface", "Interacting with others"),
    ("plumbingmanipulators", "Low-level commands: manipulators"),
    ("plumbinginterrogators", "Low-level commands: interrogators"),
    ("synchingrepositories", "Low-level commands: syncing repositories"),
    ("synchelpers", "Low-level commands: internal helpers (sync)"),
    ("purehelpers", "Low-level commands: internal helpers"),
    ("alias", "Aliases (listed for completion only)"),
    ("extra", "Built-ins and helpers outside command-list.txt"),
]
DOC_ONLY = {"guide", "userinterfaces", "developerinterfaces"}

# The issue that tracks a command rgit does not have. Everything not listed
# here falls under the scope decision for non-core commands (#186).
ISSUES = {
    "backfill": 640,
    "imap-send": 715,
    "request-pull": 715,
    "send-email": 715,
    "p4": 710,
    "svn": 710,
    "sh-i18n": 432,
    "stage": 686,
}
SCOPE_ISSUE = 186

# Script names that abbreviate or rename the command they test, or that
# start with a command name but test something else (None: attribute per case).
SCRIPT_OVERRIDES = {
    "t5300-pack-object.sh": "pack-objects",
    "t5541-http-push-smart.sh": "push",
    "t5551-http-fetch-smart.sh": "fetch",
    "t5559-http-fetch-smart-http2.sh": "fetch",
    "t1302-repo-version.sh": None,
    "t1406-submodule-ref-store.sh": None,
    "t1407-worktree-ref-store.sh": None,
    "t1510-repo-setup.sh": None,
    "t5406-remote-rejects.sh": None,
    "t5507-remote-environment.sh": None,
    "t5801-remote-helpers.sh": None,
    "t5900-repo-selection.sh": None,
}
# Old command names in script names.
SCRIPT_ALIASES = {"checkout-cache": "checkout-index"}
# Calls the test library makes in almost every script; they never decide
# which command a case tests.
INFRASTRUCTURE = {"version"}

# git's test families (t/README, "Naming Tests"): the first digit.
FAMILIES = {
    "0": "the absolute basics and global stuff",
    "1": "the basic commands concerning database",
    "2": "the basic commands concerning the working tree",
    "3": "the other basic commands",
    "4": "the diff commands",
    "5": "the pull and exporting commands",
    "6": "the revision tree commands",
    "7": "the porcelainish commands concerning the working tree",
    "8": "the porcelainish commands concerning forensics",
    "9": "the git tools",
}


# ------------------------------------------------------------------ inputs
def read_command_list(path):
    """[(name, category)] of the commands (not the guides) in command-list.txt."""
    cmds = []
    started = False
    for line in Path(path).read_text().splitlines():
        if line.startswith("### command list"):
            started = True
            continue
        if not started or not line.strip() or line.startswith("#"):
            continue
        fields = line.split()
        name, attrs = fields[0], fields[1:]
        if attrs and attrs[0] in DOC_ONLY:
            continue
        if name.startswith("git-"):
            name = name[4:]
        cat = attrs[0] if attrs and attrs[0] != "complete" else "alias"
        cmds.append((name, cat))
    return cmds


def isolated_env(tmp):
    """An environment that sees neither the user's config nor other gits."""
    empty = tmp / "empty"
    empty.mkdir(exist_ok=True)
    env = {
        "HOME": str(tmp),
        "XDG_CONFIG_HOME": str(tmp / ".config"),
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_EXEC_PATH": str(empty),
        "PATH": str(empty),
        "LANG": "C",
        "LC_ALL": "C",
        "GIT_PAGER": "cat",
        "PAGER": "cat",
    }
    return env


def probe_rgit(rgit, names):
    """{name: 'native' | 'handoff' | 'missing'} from running the binary."""
    status = {}
    with tempfile.TemporaryDirectory(prefix="compat-table-") as d:
        tmp = Path(d)
        env = isolated_env(tmp)
        out = subprocess.run([rgit, "--list-cmds=main"], env=env, cwd=tmp,
                             capture_output=True, text=True, check=True).stdout
        main = set(out.split())
        for name in names:
            if name in main:
                status[name] = "native"
                continue
            if not re.fullmatch(r"[a-z0-9][a-z0-9-]*", name) or name in ("gitk", "gitweb", "scalar"):
                # Separate programs, not `git <name>`.
                status[name] = "missing"
                continue
            p = subprocess.run([rgit, name, "-h"], env=env, cwd=tmp, stdin=subprocess.DEVNULL,
                               capture_output=True, text=True, timeout=60)
            err = p.stderr
            if "is not a git command" in err:
                status[name] = "missing"
            elif "not built into rgit" in err or re.search(r"\binstall\b", err):
                status[name] = "handoff"
            else:
                status[name] = "native"
        return status, main


def gh_json(args):
    return json.loads(subprocess.run(["gh"] + args, capture_output=True, text=True,
                                     check=True).stdout)


def download_results(run_id, dest):
    """Download the results artifact of a run (default: newest successful main run)."""
    if run_id is None:
        runs = gh_json(["run", "list", "-R", REPO, "--workflow", "conformance.yml",
                        "--branch", "main", "--status", "success", "--limit", "1",
                        "--json", "databaseId"])
        if not runs:
            sys.exit("no successful Conformance run on main")
        run_id = runs[0]["databaseId"]
    info = gh_json(["run", "view", str(run_id), "-R", REPO, "--json",
                    "databaseId,headSha,createdAt,url,headBranch,conclusion"])
    subprocess.run(["gh", "run", "download", str(run_id), "-R", REPO,
                    "-n", "conformance-results", "-D", str(dest)], check=True)
    return info


def results_root(d):
    d = Path(d)
    for cand in (d / "default", d):
        if (cand / "reference").is_dir():
            return cand
    sys.exit(f"{d}: no reference/ results found")


def read_tsv(path):
    rows = {}
    if not path.exists():
        return rows
    for line in path.read_text(errors="replace").splitlines():
        f = line.split("\t")
        if len(f) < 2 or not f[0].isdigit():
            continue
        f += ["-"] * (5 - len(f))
        rows[int(f[0])] = f
    return rows


def meta_state(path):
    if not path.exists():
        return None
    for line in path.read_text().splitlines():
        if line.startswith("state="):
            return line[6:]
    return None


def read_results(root):
    """Per valid case: (script, num, strict_pass, exercised commands).

    A case is valid when it passes against the reference git in a script that
    completed there (the definition of rgit-conformance's STATUS.md).
    """
    cases = []
    scripts = sorted(p.name[:-4] for p in (root / "reference").glob("t[0-9]*.sh.tsv"))
    for script in scripts:
        ref = read_tsv(root / "reference" / f"{script}.tsv")
        if meta_state(root / "reference" / f"{script}.meta") not in ("complete", "complete_failed"):
            continue
        fb = read_tsv(root / "fallback" / f"{script}.tsv")
        st = read_tsv(root / "strict" / f"{script}.tsv")
        for num, f in sorted(ref.items()):
            if f[1] != "pass":
                continue
            used = set()
            if num in fb:
                fw, routed = fb[num][2], fb[num][3]
                if fw != "-":
                    used.update(x for x in fw.split(",") if x)
                if routed != "-":
                    used.update(r.split()[0] for r in routed.split("; ") if r.split())
            passed = num in st and st[num][1] == "pass"
            cases.append((script, num, passed, used))
    return scripts, cases


# ------------------------------------------------------------- attribution
def script_command(script, names):
    """The command a script is named after, or None."""
    if script in SCRIPT_OVERRIDES:
        return SCRIPT_OVERRIDES[script]
    m = re.fullmatch(r"t\d{4}-(.*)\.sh", script)
    if not m:
        return None
    rest = m.group(1)
    if rest.startswith("git-"):
        rest = rest[4:]
    best = None
    for n in list(names) + list(SCRIPT_ALIASES):
        if (rest == n or rest.startswith(n + "-")) and (best is None or len(n) > len(best)):
            best = n
    return SCRIPT_ALIASES.get(best, best)


def attribute(cases, names):
    universe = set(names)
    freq = Counter()
    for _, _, _, used in cases:
        freq.update(u for u in used if u in universe)
    per = defaultdict(lambda: {"valid": 0, "strict": 0, "scripts": set()})
    exercising = Counter()
    by_script = {}
    for script, _, passed, used in cases:
        if script not in by_script:
            by_script[script] = script_command(script, names)
        cmd = by_script[script]
        cand = [u for u in used if u in universe]
        exercising.update(set(cand) | ({cmd} if cmd else set()))
        cand = [u for u in cand if u not in INFRASTRUCTURE]
        if cmd is None:
            # No command at all: git's test family (first digit) instead.
            cmd = min(cand, key=lambda u: (freq[u], u)) if cand else f"{NO_COMMAND}:{script[1]}"
        e = per[cmd]
        e["valid"] += 1
        e["strict"] += int(passed)
        e["scripts"].add(script)
    return per, exercising, by_script


# ----------------------------------------------------------------- output
def symbol(status, valid, strict):
    if status == "missing":
        return "❌"
    if status == "handoff":
        return "↪"
    if valid == 0:
        return "⚪"
    pct = 100.0 * strict / valid
    if strict == valid:
        return "✅"
    if pct >= 95:
        return "🟢"
    if pct >= 50:
        return "🟡"
    return "🟠"


def pct_str(strict, valid):
    if not valid:
        return "–"
    p = 100.0 * strict / valid
    if strict != valid and p > 99.9:
        return "99.9 %"  # never round a failure up to 100
    return f"{p:.1f} %"


def build(args):
    upstream = Path(args.upstream)
    cmdlist = read_command_list(upstream / "command-list.txt")
    listed = [n for n, _ in cmdlist]
    status, main = probe_rgit(args.rgit, listed)
    extra = sorted(main - set(listed))
    for n in extra:
        status[n] = "native"
    allcmds = cmdlist + [(n, "extra") for n in extra]
    names = [n for n, _ in allcmds]

    tmpdir = None
    if args.results:
        root = results_root(args.results)
        source = {"kind": "local", "path": str(Path(args.results).resolve()),
                  "commit": args.commit, "date": args.date}
    else:
        tmpdir = tempfile.mkdtemp(prefix="compat-results-")
        info = download_results(args.run, Path(tmpdir))
        root = results_root(tmpdir)
        source = {"kind": "ci", "run": info["databaseId"], "url": info["url"],
                  "commit": info["headSha"], "date": info["createdAt"]}
    try:
        scripts, cases = read_results(root)
    finally:
        if tmpdir:
            shutil.rmtree(tmpdir, ignore_errors=True)

    per, exercising, by_script = attribute(cases, names)

    # Upstream scripts per command that rgit-conformance has not imported.
    imported = set(scripts)
    upstream_scripts = sorted(p.name for p in (upstream / "t").glob("t[0-9][0-9][0-9][0-9]-*.sh"))
    not_imported = defaultdict(list)
    for s in upstream_scripts:
        if s not in imported:
            c = script_command(s, names)
            if c:
                not_imported[c].append(s)

    commands = []
    for name, cat in allcmds:
        e = per.get(name, {"valid": 0, "strict": 0, "scripts": set()})
        commands.append({
            "name": name,
            "category": cat,
            "rgit": status[name],
            "issue": (ISSUES.get(name, SCOPE_ISSUE) if status[name] != "native" else None),
            "valid": e["valid"],
            "strict": e["strict"],
            "scripts": sorted(e["scripts"]),
            "cases_exercising": exercising.get(name, 0),
            "scripts_not_imported": not_imported.get(name, []),
        })
    no_command = []
    for fam, desc in FAMILIES.items():
        e = per.get(f"{NO_COMMAND}:{fam}")
        if e:
            no_command.append({"family": f"t{fam}xxx", "description": desc, "valid": e["valid"],
                               "strict": e["strict"], "scripts": sorted(e["scripts"])})
    total_valid = len(cases)
    total_strict = sum(1 for c in cases if c[2])
    data = {
        "generator": "scripts/compat-table.py",
        "reference": read_version(),
        "source": source,
        "totals": {
            "commands": len(cmdlist),
            "native": sum(1 for n, _ in cmdlist if status[n] == "native"),
            "handoff": sum(1 for n, _ in cmdlist if status[n] == "handoff"),
            "missing": sum(1 for n, _ in cmdlist if status[n] == "missing"),
            "full": sum(1 for c in commands if c["category"] != "extra"
                        and symbol(c["rgit"], c["valid"], c["strict"]) == "✅"),
            "scripts": len(scripts),
            "valid": total_valid,
            "strict": total_strict,
        },
        "no_command": no_command,
        "commands": commands,
    }
    attributed = sum(c["valid"] for c in commands + no_command)
    attributed_strict = sum(c["strict"] for c in commands + no_command)
    if attributed != total_valid or attributed_strict != total_strict:
        sys.exit(f"internal error: attributed {attributed_strict}/{attributed} "
                 f"of {total_strict}/{total_valid} cases")
    return data


def read_version():
    """The reference git version pinned in scripts/fetch-upstream.sh."""
    for f in (Path(__file__).resolve().parent / "fetch-upstream.sh",):
        m = re.search(r'GIT_REF="\$\{GIT_REF:-(.*)\}"', f.read_text())
        if m:
            return m.group(1)
    return "unknown"


def tests_cell(c):
    if c["valid"]:
        cell = f"{c['valid']:,}"
        if c["scripts_not_imported"]:
            n = len(c["scripts_not_imported"])
            cell += f" (+{n} script{'' if n == 1 else 's'} not imported)"
        return cell
    if c["scripts_not_imported"]:
        return "tests not imported"
    return "no tests"


def render(data, prev=None):
    t = data["totals"]
    src = data["source"]
    date = (src.get("date") or "unknown")[:10]
    commit = (src.get("commit") or "unknown")[:12]
    if src["kind"] == "ci":
        based = (f"[Conformance run {src['run']}]({src['url']}) of {date}, "
                 f"rgit [`{commit}`](https://github.com/{REPO}/commit/{src['commit']})")
    else:
        based = f"local results of {date}, rgit `{commit}`"
    pct = 100.0 * t["strict"] / t["valid"] if t["valid"] else 0
    out = []
    out.append(f"Generated by `scripts/compat-table.py` from the {based}; reference git "
               f"{data['reference']}. Do not edit by hand.")
    out.append("")
    out.append(f"**{t['native']} of {t['commands']}** git commands are implemented natively, "
               f"{t['handoff']} are handed off to an installed tool, {t['missing']} are missing. "
               f"**{t['full']}** commands pass all of their git test cases. "
               f"Strict mode passes **{t['strict']:,} of {t['valid']:,}** valid test cases "
               f"({pct:.1f} %) from {t['scripts']} ported test scripts.")
    if prev:
        pt = prev["totals"]
        out.append("")
        out.append(f"Since the previous table (rgit `{(prev['source'].get('commit') or '?')[:12]}`): "
                   f"strict pass {t['strict'] - pt['strict']:+,}, valid cases "
                   f"{t['valid'] - pt['valid']:+,}, native commands {t['native'] - pt['native']:+}.")
    out.append("")
    out.append("Legend: ✅ implemented and 100 % of the valid strict cases pass · "
               "🟢 ≥ 95 % · 🟡 50–95 % · 🟠 < 50 % · ⚪ implemented, no ported tests · "
               "↪ handed off to an installed tool · ❌ not implemented (issue linked). "
               "*git tests (valid)*: cases of git's test suite that pass against the "
               "reference git and are attributed to the command (by the script name, "
               "otherwise by the most specific command the case calls); each case counts once. "
               "*strict pass*: cases that pass with every git call running rgit.")
    out.append("")
    out.append("| git command | rgit | git tests (valid) | strict pass | % |")
    out.append("|---|:-:|--:|--:|--:|")
    out.append(f"| **All commands** | {t['native']}/{t['commands']} native | **{t['valid']:,}** "
               f"| **{t['strict']:,}** | **{pct:.1f} %** |")
    by_cat = defaultdict(list)
    for c in data["commands"]:
        by_cat[c["category"]].append(c)
    for key, title in CATEGORIES:
        rows = by_cat.get(key)
        if not rows:
            continue
        out.append(f"| ***{title}*** | | | | |")
        for c in sorted(rows, key=lambda c: c["name"]):
            sym = symbol(c["rgit"], c["valid"], c["strict"])
            label = sym
            if c["rgit"] != "native":
                label += f" [#{c['issue']}]({ISSUES_URL}/{c['issue']})"
            prefix = "" if c["name"] in ("gitk", "gitweb", "scalar") else "git "
            valid = c["valid"]
            out.append(f"| `{prefix}{c['name']}` | {label} | {tests_cell(c)} | "
                       f"{c['strict']:,} | {pct_str(c['strict'], valid)} |"
                       if valid else
                       f"| `{prefix}{c['name']}` | {label} | {tests_cell(c)} | – | – |")
    if data["no_command"]:
        out.append("| ***Cases that call no git command (by git's test family)*** | | | | |")
        for nc in data["no_command"]:
            out.append(f"| {nc['family']}: {nc['description']} | "
                       f"{symbol('native', nc['valid'], nc['strict'])} | {nc['valid']:,} | "
                       f"{nc['strict']:,} | {pct_str(nc['strict'], nc['valid'])} |")
    if prev:
        old = {c["name"]: c for c in prev["commands"]}
        changes = []
        for c in data["commands"]:
            o = old.get(c["name"])
            if o and (o["strict"], o["valid"], o["rgit"]) != (c["strict"], c["valid"], c["rgit"]):
                changes.append(f"`{c['name']}` {o['strict']:,}/{o['valid']:,} → "
                               f"{c['strict']:,}/{c['valid']:,}")
        if changes:
            out.append("")
            out.append(f"<details><summary>Changed since the previous table "
                       f"({len(changes)} commands)</summary>")
            out.append("")
            out.append(" · ".join(changes))
            out.append("")
            out.append("</details>")
    return "\n".join(out) + "\n"


def signature(data):
    """What the table shows, without the run it comes from."""
    return json.dumps([
        data.get("reference"),
        data.get("totals"),
        data.get("no_command"),
        [[c["name"], c["category"], c["rgit"], c.get("issue"), c["valid"], c["strict"],
          len(c.get("scripts_not_imported", []))] for c in data.get("commands", [])],
    ], sort_keys=True)


def update_readme(path, table):
    text = Path(path).read_text()
    if START not in text or END not in text:
        sys.exit(f"{path}: markers {START} / {END} not found")
    head, rest = text.split(START, 1)
    _, tail = rest.split(END, 1)
    Path(path).write_text(f"{head}{START}\n{table}{END}{tail}")


def main():
    top = Path(__file__).resolve().parent.parent
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("results", nargs="?", help="local results/ dir of rgit-conformance's ./run "
                    "(default: download the newest successful main CI run)")
    ap.add_argument("--run", type=int, help="CI run id to download instead of the newest")
    ap.add_argument("--rgit", help="rgit binary (default: target/release/rgit, else target/debug/rgit)")
    ap.add_argument("--upstream", default=str(top / "upstream/git"),
                    help="reference git checkout (for command-list.txt and t/)")
    ap.add_argument("--commit", help="rgit commit of local results")
    ap.add_argument("--date", help="date of local results (YYYY-MM-DD)")
    ap.add_argument("--json", default=str(top / "docs/compat/compat.json"),
                    help="raw data: previous version read for deltas, then rewritten")
    ap.add_argument("--no-json", action="store_true", help="do not write the JSON file")
    ap.add_argument("--update-readme", action="store_true",
                    help="rewrite the table in README.md (and the JSON), unless the numbers "
                    "equal those of the stored JSON")
    ap.add_argument("--force", action="store_true",
                    help="with --update-readme: rewrite even when the numbers are unchanged")
    args = ap.parse_args()
    if not args.rgit:
        for cand in ("target/release/rgit", "target/debug/rgit"):
            if (top / cand).exists():
                args.rgit = str(top / cand)
                break
        else:
            sys.exit("no rgit binary found; build it or pass --rgit")
    if args.results and not args.date:
        args.date = datetime.date.today().isoformat()

    data = build(args)
    prev = None
    jp = Path(args.json)
    if jp.exists():
        try:
            prev = json.loads(jp.read_text())
        except ValueError:
            prev = None
        if prev and args.update_readme and not args.force and signature(prev) == signature(data):
            # Only a newer run with the same numbers: leave README and JSON
            # alone, so CI opens no pull request for it.
            print("compatibility table unchanged", file=sys.stderr)
            return
        if prev and prev.get("source", {}).get("commit") == data["source"].get("commit"):
            prev = prev.get("previous")
    table = render(data, prev)
    if not args.no_json:
        if prev:
            data["previous"] = {"source": prev["source"], "totals": prev["totals"],
                                "commands": [{k: c[k] for k in ("name", "rgit", "valid", "strict")}
                                             for c in prev["commands"]]}
        jp.parent.mkdir(parents=True, exist_ok=True)
        jp.write_text(json.dumps(data, indent=1, ensure_ascii=False) + "\n")
    if args.update_readme:
        update_readme(top / "README.md", table)
    t = data["totals"]
    print(f"based on {data['source'].get('date')} rgit {data['source'].get('commit')}: "
          f"{t['native']}/{t['commands']} native, strict {t['strict']}/{t['valid']}",
          file=sys.stderr)
    if not args.update_readme:
        sys.stdout.write(table)


if __name__ == "__main__":
    main()
