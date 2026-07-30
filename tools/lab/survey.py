#!/usr/bin/env python3
"""Compatibility lab: run every module and report where each one stops.

This is the instrument this project's compatibility-testing methodology calls
for (see docs/LEARNINGS.md). Progress on the runtime is
measured by re-running it, not by reasoning about it.

    tools/lab/survey.py <dir-of-rsrc-files> [frames]
"""
from __future__ import annotations

import json
import re
import subprocess
import sys
from collections import Counter, defaultdict
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "tools/audit"))
from rsrc import parse  # noqa: E402

# The `lab` profile: optimised, with overflow checks kept. See the comment on
# `[profile.lab]` in Cargo.toml — measuring a debug build reported Mandelbrot as
# a 240-second timeout when it finishes in a fraction of a second.
#     cargo build --profile lab --example run_module -p ad-host-v2
BIN = REPO / "target/lab/examples/run_module"


def compare(now: dict, base: dict) -> int:
    """Fail on any module that got worse. Improvements are reported, not failed.

    "Worse" is deliberately narrow: an outcome that was `full lifecycle` and no
    longer is, or a module that drew ink and now draws none. A smaller nonzero
    count is not a failure on its own — flagging that would train everyone to
    ignore this check.

    It *is* reported, though, as `drift`. The original reason not to look at ink
    at all was that it "wobbles with timing"; the matrix now measures determinism
    at 59 of 59, so it does not wobble — a run reproduces exactly, and any change
    in a count is a real change in what a module drew. Leaving that unreported
    cost something concrete: raising the emulated clock altered ten modules'
    output, in both directions, and this check passed clean and silent.
    """
    want = base.get("modules", base)
    regressions, improvements, changes, drift = [], [], [], []
    for name, was in sorted(want.items()):
        got = now.get(name)
        if got is None:
            regressions.append(f"{name}: disappeared from the survey")
            continue
        if was["outcome"] == "full lifecycle" and got["outcome"] != "full lifecycle":
            regressions.append(
                f"{name}: full lifecycle -> {got['outcome']} ({got['detail']})"
            )
        elif was["ink"] > 0 and got["ink"] == 0:
            regressions.append(f"{name}: drew {was['ink']} ink, now draws none")
        elif was["outcome"] != "full lifecycle" and got["outcome"] == "full lifecycle":
            improvements.append(f"{name}: {was['outcome']} -> full lifecycle")
        elif was["outcome"] != got["outcome"]:
            # Neither state is "working", so this is neither pass nor fail — but
            # it is the most informative line in the report. `wild jump -> hang`
            # means control flow stopped being corrupt and started merely being
            # slow, which turns a mystery into a tractable problem.
            changes.append(f"{name}: {was['outcome']} -> {got['outcome']}")
        elif was["ink"] == 0 and got["ink"] > 0:
            improvements.append(f"{name}: now draws {got['ink']} ink")
        # Same outcome, different picture. Neither better nor worse, but never
        # nothing — see the docstring.
        if (
            was["outcome"] == got["outcome"]
            and was["ink"] > 0
            and got["ink"] > 0
            and was["ink"] != got["ink"]
        ):
            pct = 100 * (got["ink"] - was["ink"]) / was["ink"]
            drift.append(f"{name}: ink {was['ink']} -> {got['ink']} ({pct:+.0f}%)")
    for line in improvements:
        print(f"  better: {line}")
    for line in changes:
        print(f"  moved:  {line}")
    for line in drift:
        print(f"  drift:  {line}")
    for line in regressions:
        print(f"  WORSE:  {line}")
    new = set(now) - set(want)
    for name in sorted(new):
        print(f"  new:    {name} ({now[name]['outcome']})")
    print(
        f"\n{len(improvements)} improved, {len(changes)} moved, "
        f"{len(drift)} drifted, {len(regressions)} regressed, {len(new)} new"
    )
    if regressions:
        print("\nBASELINE CHECK FAILED")
        return 1
    print("\nbaseline check passed")
    return 0


def main() -> int:
    argv = [a for a in sys.argv[1:]]
    json_out = None
    check_path = None
    for flag, setter in (("--json", "json"), ("--check", "check")):
        if flag in argv:
            i = argv.index(flag)
            value = argv[i + 1]
            del argv[i : i + 2]
            if setter == "json":
                json_out = value
            else:
                check_path = value
    forks = Path(argv[0] if argv else REPO / "modules")
    frames = argv[1] if len(argv) > 1 else "20"

    mods = []
    for f in sorted(forks.glob("*.rsrc")):
        try:
            if any(r["type"] == "ADgm" for r in parse(f.read_bytes())):
                mods.append(f)
        except Exception:
            pass

    status = Counter()
    traps: Counter[str] = Counter()
    trap_mods = defaultdict(list)
    says = Counter()
    say_mods = defaultdict(list)
    wins = []
    rows = []
    ink_of: dict[str, int] = {}
    col_of: dict[str, int] = {}

    for f in mods:
        try:
            r = subprocess.run(
                [str(BIN), str(f), frames], capture_output=True, text=True, timeout=240
            )
            out = r.stdout + r.stderr
        except subprocess.TimeoutExpired:
            status["timeout"] += 1
            rows.append((f.stem, "timeout", ""))
            continue

        trap = re.search(r"unhandled Toolbox trap \$([0-9A-F]{4})", out)
        name = re.search(r"\$[0-9A-F]{4} at PC [^:]+: (_[A-Za-z0-9]+|QuickDraw trap)", out)
        # Prefer the pre-Close reading: modules erase the screen on Close, and
        # the post-Close number scored working modules as "drew nothing".
        px = re.search(r"^ink \(live\)\s+(\d+)", out, re.M) or re.search(
            r"^ink\s+(\d+)", out, re.M
        )
        ntraps = re.search(r"^traps\s+(\d+) distinct", out, re.M)
        cols = re.search(r"colours=(\d+)", out)

        ink_of[f.stem] = int(px.group(1)) if px else 0
        col_of[f.stem] = int(cols.group(1)) if cols else 0

        if f"DrawFrame   -> {frames}/{frames}" in out:
            status["FULL LIFECYCLE"] += 1
            n = int(px.group(1)) if px else 0
            wins.append((f.stem, n, int(ntraps.group(1)) if ntraps else 0))
            rows.append((f.stem, "full lifecycle", f"ink={n}"))
        elif "CompressedCode" in out or "compressed resource" in out or "Decompression(" in out:
            # Its own bucket, not "needs a trap". These modules ship their code
            # packed with the System 7 resource compression (magic $A89F6572);
            # the first word of the packed payload happens to BE the
            # Unimplemented trap, so before the loader recognised the format
            # they all reported a trap fault that had nothing to do with them.
            status["compressed code"] += 1
            m = re.search(r"needs dcmp (-?\d+)", out)
            rows.append((f.stem, "compressed code", f"dcmp {m.group(1)}" if m else ""))
        elif "declined to initialize" in out or "declined to blank" in out:
            # A module can refuse at Initialize *or* at Blank, and both are the
            # same kind of answer: it ran, it decided it could not, and it said
            # why. PICS Player refuses at Blank for want of a picture file, and
            # was filed as "other" — and before the host learned not to send
            # DrawFrame after a refused Blank, as a *hang*.
            status["declines"] += 1
            m = re.search(r'module says: "(.*?)"', out)
            msg = m.group(1) if m else "(no message)"
            says[msg] += 1
            say_mods[msg].append(f.stem)
            rows.append((f.stem, "declines", msg[:52]))
        elif trap:
            status["needs a trap"] += 1
            key = f"${trap.group(1)} {name.group(1) if name else '?'}"
            traps[key] += 1
            trap_mods[key].append(f.stem)
            rows.append((f.stem, "needs trap", key))
        elif "host callout [" in out:
            m = re.search(r"host callout \[(.*?)\] invoked", out)
            key = f"callout {m.group(1) if m else '?'}"
            status["calls a host service"] += 1
            traps[key] += 1
            trap_mods[key].append(f.stem)
            rows.append((f.stem, "host service", key))
        elif "wild jump" in out:
            status["wild jump"] += 1
            rows.append((f.stem, "wild jump", ""))
        elif "did not return" in out:
            status["hang"] += 1
            rows.append((f.stem, "hang", ""))
        elif "entry point stub" in out:
            status["unresolved entry stub"] += 1
            rows.append((f.stem, "unresolved entry stub", ""))
        else:
            status["other"] += 1
            rows.append((f.stem, "other", ""))

    # --json writes a machine-readable baseline; --check compares against one.
    # A baseline is what makes every later refactor falsifiable: without it,
    # "no regressions" is an opinion.
    record = {
        n: {"outcome": o, "detail": d, "ink": ink_of.get(n, 0),
            "colours": col_of.get(n, 0)}
        for n, o, d in rows
    }
    if json_out:
        Path(json_out).parent.mkdir(parents=True, exist_ok=True)
        Path(json_out).write_text(
            json.dumps({"frames": int(frames), "modules": record},
                       indent=1, sort_keys=True) + "\n"
        )
        print(f"wrote baseline {json_out} ({len(record)} modules)")

    if check_path:
        return compare(record, json.loads(Path(check_path).read_text()))

    print(f"{'module':<30}{'outcome':<24}detail")
    print("-" * 92)
    for n, o, d in rows:
        print(f"{n:<30}{o:<24}{d}")

    print(f"\n=== {len(mods)} modules ===")
    for k, v in status.most_common():
        print(f"  {v:>3}  {k}")

    if wins:
        print("\nfull lifecycle:")
        for n, px, nt in sorted(wins, key=lambda x: -x[1]):
            print(f"  {n:<30} ink={px:<9} traps={nt}")

    if traps:
        print("\ntraps to implement next (most modules first):")
        for k, c in traps.most_common(20):
            print(f"  {k:<28} blocks {c:>2}   {', '.join(trap_mods[k][:4])}")

    if says:
        print("\nmodules that decline, and why:")
        for m, c in says.most_common(12):
            print(f"  x{c:<3} {m[:60]:<62} {', '.join(say_mods[m][:3])}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
