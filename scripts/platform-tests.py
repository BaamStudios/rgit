#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Platform tests (issue #183): the workspace's tests on a platform other
than Linux, compared with the recorded baseline of that platform.

Builds every test target (`cargo test --all --no-run`), runs each test
executable on its own with a time limit, runs the doc tests, and compares
the targets that fail with `ci/platform/<platform>.txt`:

  - a target fails (or exceeds the time limit) and is not listed -> error
  - a listed target passes                                       -> notice
  - the workspace does not build                                 -> error

Baseline format, one entry per line:

    -p rgit-cli --test add    #679 reason
    -p rgit-cli               #679 every target of the crate
    skip -p rgit-cli --test fetch   #680 hangs

A target is named as cargo names it (`-p <crate> --lib`, `--bin <name>`,
`--test <name>`, `--doc`). `skip` entries are not run at all (for targets
that hang). Every entry needs an issue reference (`#<number>`), so nothing
is skipped or tolerated silently.

Usage: scripts/platform-tests.py <platform> [--timeout <seconds>]

`PLATFORM_TESTS_ONLY=<regex>` runs only the targets whose name matches (for
trying the script locally; the comparison then covers those targets only).
"""

import json
import os
import re
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TAIL_LINES = 150


def read_baseline(path):
    """Returns (known targets or crates, skipped targets); exits on a bad line."""
    known, skipped, bad = set(), set(), []
    with open(path, encoding="utf-8") as f:
        for raw in f:
            line = raw.strip()
            if not line or line.startswith("#"):
                continue
            entry, _, comment = line.partition("#")
            if not re.match(r"\d+", comment):
                bad.append(line)
                continue
            entry = " ".join(entry.split())
            if entry.startswith("skip "):
                entry = entry[len("skip "):]
                skipped.add(entry)
            known.add(entry)
    if bad:
        for line in bad:
            print(f"error: {path}: no issue reference (#<number>) in: {line}", file=sys.stderr)
        sys.exit(2)
    return known, skipped


def build():
    """Builds all test targets; returns [(target name, executable, manifest dir)]."""
    proc = subprocess.Popen(
        ["cargo", "test", "--all", "--no-run", "--message-format=json"],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    targets = []
    for line in proc.stdout:
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        if msg.get("reason") == "compiler-message":
            rendered = msg.get("message", {}).get("rendered")
            if rendered:
                sys.stderr.write(rendered)
            continue
        if msg.get("reason") != "compiler-artifact" or not msg.get("executable"):
            continue
        if not msg.get("profile", {}).get("test"):
            continue
        manifest_dir = os.path.dirname(msg["manifest_path"])
        package = os.path.basename(manifest_dir)
        target = msg["target"]
        kinds = target.get("kind", [])
        if "test" in kinds:
            name = f"-p {package} --test {target['name']}"
        elif "bin" in kinds:
            name = f"-p {package} --bin {target['name']}"
        else:
            name = f"-p {package} --lib"
        targets.append((name, msg["executable"], manifest_dir))
    if proc.wait() != 0:
        print("error: the test targets do not build", file=sys.stderr)
        sys.exit(1)
    return sorted(targets)


def run(name, command, cwd, limit, env=None):
    """Runs one target; returns (passed, note)."""
    start = time.monotonic()
    try:
        proc = subprocess.run(
            command,
            cwd=cwd,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=limit,
            check=False,
        )
        output, code, note = proc.stdout, proc.returncode, ""
    except subprocess.TimeoutExpired as e:
        output, code, note = e.stdout or b"", None, f"no result after {limit} s"
    seconds = time.monotonic() - start
    if code == 0:
        print(f"ok      {name} ({seconds:.0f} s)", flush=True)
        return True, ""
    if not note:
        note = f"exit code {code}"
    print(f"FAILED  {name} ({seconds:.0f} s, {note})", flush=True)
    lines = output.decode("utf-8", errors="replace").splitlines()
    print(f"::group::output of {name} (last {min(len(lines), TAIL_LINES)} of {len(lines)} lines)")
    for line in lines[-TAIL_LINES:]:
        print(line)
    print("::endgroup::", flush=True)
    return False, note


def main():
    args = sys.argv[1:]
    limit = 900
    if "--timeout" in args:
        i = args.index("--timeout")
        limit = int(args[i + 1])
        del args[i:i + 2]
    if len(args) != 1:
        print("usage: scripts/platform-tests.py <platform> [--timeout <seconds>]", file=sys.stderr)
        sys.exit(2)
    platform = args[0]
    baseline = os.path.join("ci", "platform", f"{platform}.txt")
    known, skipped = read_baseline(os.path.join(ROOT, baseline))

    def listed(name):
        crate = " ".join(name.split()[:2])
        return name in known or crate in known

    only = re.compile(os.environ.get("PLATFORM_TESTS_ONLY", ""))
    failed = []
    seen = set()
    for name, exe, manifest_dir in build():
        if not only.search(name):
            continue
        seen.add(name)
        if name in skipped:
            print(f"skipped {name} (listed in {baseline})", flush=True)
            continue
        env = dict(os.environ, CARGO_MANIFEST_DIR=manifest_dir)
        passed, _ = run(name, [exe], manifest_dir, limit, env)
        if not passed:
            failed.append(name)
    # Doc tests go through cargo; a crate's doc tests are `-p <crate> --doc`.
    crates = sorted({name.split()[1] for name in seen})
    for crate in crates:
        name = f"-p {crate} --doc"
        seen.add(name)
        if name in skipped:
            print(f"skipped {name} (listed in {baseline})", flush=True)
            continue
        lib = os.path.join(ROOT, "crates", crate, "src", "lib.rs")
        if not os.path.exists(lib):
            continue
        passed, _ = run(name, ["cargo", "test", "-p", crate, "--doc"], ROOT, limit)
        if not passed:
            failed.append(name)

    print(f"\n== {platform}: {len(failed)} failed test targets, compared with {baseline} ==")
    new = [name for name in failed if not listed(name)]
    for name in failed:
        if listed(name):
            print(f"known failure: {name}")
    for name in new:
        print(f"::error::{platform}: test target fails and is not in {baseline}: {name}")
    for entry in sorted(known - skipped):
        if len(entry.split()) > 2:
            if entry in seen and entry not in failed:
                print(f"::notice::{platform}: {entry} passes now, remove it from {baseline}")
        elif not any(name.startswith(entry + " ") for name in failed):
            print(f"::notice::{platform}: every target of {entry} passes now, remove it from {baseline}")
    if new:
        print(
            f"error: new failures on {platform}. Fix them, or record them in "
            f"{baseline} with an issue reference.",
            file=sys.stderr,
        )
        sys.exit(1)
    print(f"ok: no failures on {platform} beyond the recorded baseline")


if __name__ == "__main__":
    main()
