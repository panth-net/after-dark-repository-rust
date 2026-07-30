#!/usr/bin/env python3
"""Generate the compatibility matrix: what is *evidenced* about each module.

    tools/lab/matrix.py <dir-of-rsrc-files> [frames] [--md out.md] [--json out.json]

Why this exists rather than a pass/fail count
---------------------------------------------
"Runs" is not "replicated", and this project has already been burned twice by a
single-number metric. An early measure counted non-zero pixels, so a **black
screen scored as fully rendered**. A later one read the framebuffer *after*
`Close`, when modules have already erased it, so **working modules scored
blank**. Both survived because one number cannot say *which* claim it is making.

So each column below is a separate claim with its own evidence, and a column
that cannot be evidenced yet reports `--` rather than anything that could be
mistaken for success. A module is not "done" because it closed without an
unhandled trap.

Columns
-------
imported     the fork parses; resource count and sha256 recorded
initializes  Initialize and Blank both return noErr
renders      draws ink in more than one colour — non-uniform output, not just
             non-zero, which is the trap the first metric fell into
animates     ink or the frame hash changes between an early and a late frame:
             a still image is not an animation
determinism  two runs with identical input produce byte-identical final frames
sound        every `snd ` resource the module plays decodes to PCM, with its
             rate and sample count recorded
mixed        the module's whole session renders through the real mixer to a WAV
             (`AD_MIX_WAV`): timing, channel model and all. This is what proves
             the *path* rather than the decoder — the mixer looped a one-shot
             sound forever and the per-sound WAVs could not have shown it.
audible      NOT EVIDENCED **per module**, deliberately. An output device exists
             (`ad_runtime::AudioDevice`, used by ad-player) and is tested, but
             the lab never opens one: 66 modules run headless in CI and nothing
             about a machine's sound hardware may affect a survey. "Audible"
             stays `--` until a module's sound is verified against the original,
             which is the ROM oracle's job.
settings     the module's settings resources decode to controls
persistence  NOT EVIDENCED — resource writes are in-memory only
stability    no wild jump, no unimplemented trap, no fault, over the run
fidelity     the strongest verification layer applied (see docs/LEARNINGS.md)
"""
from __future__ import annotations

import hashlib
import json
import os
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "tools/audit"))
from rsrc import parse  # noqa: E402

# The `lab` profile — optimised with overflow checks. See survey.py and the
# `[profile.lab]` comment in Cargo.toml.
BIN = REPO / "target/lab/examples/run_module"

NOT_EVIDENCED = "--"


def run(fork: Path, frames: int, png: Path | None) -> tuple[str, str | None]:
    """Run a module, returning its combined output and the frame's sha256."""
    cmd = [str(BIN), str(fork), str(frames)]
    if png is not None:
        cmd.append(str(png))
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=240)
        out = r.stdout + r.stderr
    except subprocess.TimeoutExpired:
        return "TIMEOUT", None
    digest = None
    if png is not None and png.exists():
        digest = hashlib.sha256(png.read_bytes()).hexdigest()[:16]
    return out, digest


def mix_rms(fork: Path, frames: int, tmp: Path) -> object:
    """RMS of the module's whole session rendered through the real mixer.

    0 means "played sounds, and the mixer produced silence" — which is a bug, and
    is exactly the shape the one-shot-loop bug had in reverse (a constant tone
    where there should have been discrete effects). `--` means the module played
    nothing, which is not a failure.
    """
    wav = tmp / f"{fork.stem}.mix.wav"
    env = dict(os.environ, AD_MIX_WAV=str(wav))
    try:
        subprocess.run(
            [str(BIN), str(fork), str(frames)],
            capture_output=True, text=True, timeout=240, env=env,
        )
    except subprocess.TimeoutExpired:
        return NOT_EVIDENCED
    if not wav.exists():
        return NOT_EVIDENCED
    data = wav.read_bytes()
    at = data.find(b"data")
    if at < 0:
        return 0
    n = int.from_bytes(data[at + 4 : at + 8], "little")
    body = data[at + 8 : at + 8 + n]
    if not body:
        return 0
    vals = [
        int.from_bytes(body[i : i + 2], "little", signed=True)
        for i in range(0, len(body) - 1, 2)
    ]
    if not vals:
        return 0
    return round((sum(v * v for v in vals) / len(vals)) ** 0.5)


def num(pattern: str, text: str, default: int = 0) -> int:
    m = re.search(pattern, text, re.M)
    return int(m.group(1)) if m else default


def assess(fork: Path, frames: int, tmp: Path) -> dict:
    """Everything one module's evidence supports, column by column."""
    row: dict[str, object] = {"module": fork.stem}

    # ---- imported ----
    data = fork.read_bytes()
    try:
        resources = parse(data)
        row["imported"] = True
        row["resources"] = len(resources)
        row["sha256"] = hashlib.sha256(data).hexdigest()[:16]
    except Exception as e:  # noqa: BLE001 - the reason belongs in the report
        row["imported"] = False
        row["note"] = f"parse failed: {e}"
        return row

    early_png = tmp / f"{fork.stem}.early.png"
    late_png = tmp / f"{fork.stem}.late.png"
    late2_png = tmp / f"{fork.stem}.late2.png"

    early_out, early_hash = run(fork, max(frames // 10, 2), early_png)
    late_out, late_hash = run(fork, frames, late_png)
    # A second identical run is the only way to tell a deterministic module from
    # one that merely looked stable once.
    _, repeat_hash = run(fork, frames, late2_png)

    # ---- initializes ----
    row["initializes"] = "Initialize  -> Ok" in late_out and "Blank       -> Ok" in late_out
    if "module says:" in late_out:
        m = re.search(r'module says: "(.*?)"', late_out)
        row["declines"] = m.group(1) if m else "(no message)"

    # ---- renders: non-uniform, not merely non-zero ----
    ink = num(r"^ink \(live\)\s+(\d+)", late_out)
    colours = num(r"colours=(\d+)", late_out)
    row["ink"] = ink
    row["colours"] = colours
    row["renders"] = ink > 0 and colours > 1

    # ---- animates ----
    early_ink = num(r"^ink \(live\)\s+(\d+)", early_out)
    row["animates"] = bool(
        row["renders"] and (early_ink != ink or (early_hash and early_hash != late_hash))
    )

    # ---- determinism ----
    # A module that never produced a frame has not *failed* determinism; it was
    # not measured. Reporting the absence of evidence as a negative is the exact
    # error this tool exists to prevent, and the first version of this column
    # made it — all 16 "non-deterministic" modules were simply ones that decline
    # at Initialize and so never write a frame at all.
    if late_hash is None or repeat_hash is None:
        row["determinism"] = NOT_EVIDENCED
    else:
        row["determinism"] = late_hash == repeat_hash
    row["frame_sha256"] = late_hash

    # ---- sound ----
    played = re.findall(r"\[snd\] play \"(.*?)\": (\d+) samples @ (\d+) Hz", late_out)
    have_snd = any(r["type"] == "snd " for r in resources)
    if not have_snd:
        row["sound"] = NOT_EVIDENCED  # nothing to decode; not a failure
    else:
        row["sound"] = len(played) if played else 0
    # An output device exists and is tested; the lab does not open one, so this
    # is "not measured here", not "not implemented". See the module docstring.
    row["audible"] = NOT_EVIDENCED
    # `mixed`: the whole session through the real mixer. Evidence that the path
    # from resource to speaker is right, which the per-sound WAVs cannot give.
    row["mixed"] = mix_rms(fork, frames, tmp) if have_snd else NOT_EVIDENCED

    # ---- settings ----
    settings_types = {"sVal", "bVal", "mVal", "xVal", "tVal", "sUnt", "Cals"}
    row["settings"] = sum(1 for r in resources if r["type"] in settings_types)

    # ---- persistence ----
    # Durable writes exist and are tested end to end through the traps
    # (ad-runtime/tests/high_score_survives.rs). Not measurable *per module*
    # here: only a module that actually saves exercises it, and reaching Lunatic
    # Fringe's high-score save takes a played game, not 20 headless frames.
    row["persistence"] = NOT_EVIDENCED

    # ---- stability ----
    bad = [
        w
        for w in ("wild jump", "is not implemented", "did not return", "TIMEOUT")
        if w in late_out
    ]
    row["stability"] = not bad
    if bad:
        row["stability_note"] = bad[0]

    # ---- fidelity confidence ----
    # L1-L6 are source- and self-evidence layers, all of which apply to any
    # module that runs here. L7 is the QEMU/ROM oracle: it boots, but no
    # module frame has been captured through it yet, so no module may claim it.
    row["fidelity"] = "L1-L6" if row["renders"] else "L1-L3"
    return row


def markdown(rows: list[dict], frames: int) -> str:
    def cell(v: object) -> str:
        if v is True:
            return "yes"
        if v is False:
            return "**no**"
        return str(v)

    head = (
        "| module | imported | init | renders | animates | determinism | "
        "sound | mixed | audible | settings | persist | stable | fidelity |"
    )
    out = [
        f"# Compatibility matrix ({len(rows)} modules, {frames} frames)",
        "",
        "`--` means **not evidenced**, never \"passed\". A module is not complete "
        "because it closed without an unhandled trap.",
        "",
        head,
        "|" + "---|" * 13,
    ]
    for r in sorted(rows, key=lambda x: (-int(bool(x.get("renders"))), x["module"])):
        out.append(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |".format(
                r["module"],
                cell(r.get("imported")),
                cell(r.get("initializes")),
                cell(r.get("renders")),
                cell(r.get("animates")),
                cell(r.get("determinism")),
                cell(r.get("sound", NOT_EVIDENCED)),
                cell(r.get("mixed", NOT_EVIDENCED)),
                cell(r.get("audible")),
                cell(r.get("settings")),
                cell(r.get("persistence")),
                cell(r.get("stability")),
                cell(r.get("fidelity")),
            )
        )
    # Totals, per column, so progress is legible without reading 66 rows.
    out += ["", "## Totals", ""]
    for col in ("imported", "initializes", "renders", "animates", "determinism", "stability"):
        yes = sum(1 for r in rows if r.get(col) is True)
        unmeasured = sum(1 for r in rows if r.get(col) == NOT_EVIDENCED)
        of = len(rows) - unmeasured
        tail = f" ({unmeasured} not measured)" if unmeasured else ""
        out.append(f"- **{col}**: {yes} / {of}{tail}")
    mixed = sum(1 for r in rows if isinstance(r.get("mixed"), (int, float)) and r["mixed"] > 0)
    with_snd = sum(1 for r in rows if r.get("mixed") != NOT_EVIDENCED)
    out.append(f"- **mixed** (session renders to audible PCM): {mixed} / {with_snd}")
    out.append(
        "- **audible**: not measured here. `ad_runtime::AudioDevice` exists and "
        "is tested, but the lab never opens an output device — a 66-module "
        "survey must not depend on a machine's sound hardware."
    )
    out.append(
        "- **persistence**: durable writes exist "
        "(`ad_runtime::ForkSink`, tmp + fsync + rename) and are tested end to "
        "end through the traps. Not measured per module: only a module that "
        "actually saves exercises it, and reaching Lunatic Fringe's high-score "
        "save needs a played game."
    )
    return "\n".join(out) + "\n"


def main() -> int:
    argv = sys.argv[1:]
    md_out = json_out = None
    for flag in ("--md", "--json"):
        if flag in argv:
            i = argv.index(flag)
            value = argv[i + 1]
            del argv[i : i + 2]
            if flag == "--md":
                md_out = value
            else:
                json_out = value
    from_json = None
    if "--from-json" in argv:
        i = argv.index("--from-json")
        from_json = argv[i + 1]
        del argv[i : i + 2]
    forks = Path(argv[0] if argv else REPO / "modules")
    frames = int(argv[1]) if len(argv) > 1 else 20

    if from_json:
        rows = json.loads(Path(from_json).read_text())
        for r in rows:
            # Re-derive only the columns whose *reporting* changed; the measured
            # facts (hashes, ink, outcomes) are reused as recorded.
            if r.get("frame_sha256") is None:
                r["determinism"] = NOT_EVIDENCED
        if md_out:
            Path(md_out).write_text(markdown(rows, frames))
            print(f"wrote {md_out} from {from_json}")
        else:
            print(markdown(rows, frames))
        return 0

    # Not every fork on the disk is a module, and a few are not parseable by the
    # independent Python reader at all — that asymmetry with the Rust parser is
    # itself worth keeping visible, so skip quietly here rather than abort.
    mods = []
    for f in sorted(forks.glob("*.rsrc")):
        try:
            if any(r["type"] == "ADgm" for r in parse(f.read_bytes())):
                mods.append(f)
        except Exception:  # noqa: BLE001 - a non-module file is not an error here
            pass
    tmp = REPO / "target/matrix"
    tmp.mkdir(parents=True, exist_ok=True)

    rows = []
    for f in mods:
        row = assess(f, frames, tmp)
        rows.append(row)
        flags = "".join(
            [
                "I" if row.get("initializes") else ".",
                "R" if row.get("renders") else ".",
                "A" if row.get("animates") else ".",
                "D" if row.get("determinism") else ".",
                "S" if row.get("stability") else ".",
            ]
        )
        print(f"  {row['module']:<30} {flags}  ink={row.get('ink', 0)}")

    if md_out:
        Path(md_out).write_text(markdown(rows, frames))
        print(f"wrote {md_out}")
    if json_out:
        Path(json_out).write_text(json.dumps(rows, indent=1, sort_keys=True) + "\n")
        print(f"wrote {json_out}")
    if not md_out and not json_out:
        print(markdown(rows, frames))
    return 0


if __name__ == "__main__":
    sys.exit(main())
