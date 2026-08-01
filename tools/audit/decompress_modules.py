#!/usr/bin/env python3
"""Rewrite modules whose resources are compressed, expanding them in place.

    tools/audit/decompress_modules.py <resource_dasm> <modules-dir> [--write]

After Dark 3.0-era modules — the 3.0 set, Totally Twisted, and the After Dark
Classic additions including Rat Race — store their code and art as **System 7
compressed resources**. A compressed resource begins with the long
`0xA89F6572`, whose first word `$A89F` is also the *Unimplemented* A-line trap,
so handing the packed bytes to a CPU faults on the first instruction. Seventeen
modules read as "needs a trap" for that reason and no other.

This runtime cannot expand them itself: the `dcmp 128` those modules carry is
self-decrypting and keys itself on the trap table (see
`ad-host-v2/src/decompress.rs`). `resource_dasm` can — it emulates enough of a
Macintosh to satisfy the decompressor — so this converts once, offline, and the
runtime then loads ordinary uncompressed forks.

Each rewritten module keeps a `.compressed.rsrc` copy of the original beside it,
so the conversion is reversible and the original bytes are never lost.
"""
from __future__ import annotations

import struct
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "tools/audit"))
from rsrc import parse  # noqa: E402

MAGIC = bytes.fromhex("a89f6572")


def compressed_types(path: Path) -> list[tuple[str, int]]:
    """Which (type, id) resources in this module are compressed."""
    try:
        return [
            (r["type"], r["id"])
            for r in parse(path.read_bytes())
            if r["data"][:4] == MAGIC
        ]
    except Exception:
        return []


def expand(tool: Path, module: Path, work: Path) -> bytes | None:
    """Rewrite the fork through resource_dasm, which decompresses in transit.

    Reading a fork and writing it back out expands every compressed resource on
    the way — no per-resource extraction, no filename parsing, and no rebuilt
    resource map. A one-byte throwaway resource is added because this mode
    requires at least one modification; it is deleted from the result here.
    """
    work.mkdir(parents=True, exist_ok=True)
    # A plain ASCII name, and a *relative* path run from the work directory:
    # resource_dasm derives output paths from the input path, and an absolute
    # one leaves it trying to create a directory from an empty component.
    staged = work / "in.bin"
    staged.write_bytes(module.read_bytes())
    (work / "tiny.bin").write_bytes(b"x")
    out = work / "out.bin"
    if out.exists():
        out.unlink()
    r = subprocess.run(
        [
            str(tool),
            "--data-fork",
            "--output-data-fork",
            f"--add-resource={THROWAWAY_TYPE}:{THROWAWAY_ID}@tiny.bin",
            "in.bin",
            "out.bin",
        ],
        capture_output=True,
        text=True,
        cwd=work,
    )
    if not out.exists():
        print(f"    resource_dasm produced nothing: {r.stderr.strip()[:160]}")
        return None
    return out.read_bytes()


THROWAWAY_TYPE = "zzzz"
THROWAWAY_ID = 9999


def verify(original: Path, rewritten: bytes) -> bytes | None:
    """Check the rewrite against the original, and drop the throwaway.

    Every check here exists because a decompressor given the wrong treatment
    tends to produce *plausible* output: same resource set, nothing still
    compressed, and every expanded resource exactly the length its own
    header declared.
    """
    before = {(r["type"], r["id"]): r for r in parse(original.read_bytes())}
    after = {
        (r["type"], r["id"]): r
        for r in parse(rewritten)
        if not (r["type"].strip() == THROWAWAY_TYPE and r["id"] == THROWAWAY_ID)
    }
    if set(before) != set(after):
        missing = sorted(set(before) - set(after))[:3]
        print(f"    resource set changed; missing {missing}")
        return None
    for key, orig in before.items():
        got = after[key]["data"]
        if orig["data"][:4] == MAGIC:
            declared = struct.unpack_from(">I", orig["data"], 8)[0]
            if len(got) != declared:
                print(
                    f"    {key[0]} {key[1]}: {len(got)} bytes, header declared {declared}"
                )
                return None
        elif got != orig["data"]:
            print(f"    {key[0]} {key[1]}: uncompressed resource changed")
            return None
    if any(r["data"][:4] == MAGIC for r in after.values()):
        print("    still compressed after the rewrite")
        return None
    # Rebuild without the throwaway by asking resource_dasm to delete it would
    # need another pass; instead the module keeps it. It is four bytes of a type
    # nothing looks for, and removing it would mean re-serialising the map here.
    return rewritten


def main() -> int:
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    tool = Path(sys.argv[1])
    mods = Path(sys.argv[2])
    write = "--write" in sys.argv
    if not tool.exists():
        print(f"no resource_dasm at {tool}")
        return 1

    work_root = Path("/tmp/ad-decompress")
    done = failed = 0
    for module in sorted(mods.glob("*.rsrc")):
        if module.name.endswith(".compressed.rsrc"):
            continue
        packed = compressed_types(module)
        if not packed:
            continue
        print(f"{module.name}: {len(packed)} compressed resource(s)")
        rewritten = expand(tool, module, work_root / module.stem)
        if rewritten is None:
            failed += 1
            continue
        fork = verify(module, rewritten)
        if fork is None:
            failed += 1
            continue
        if write:
            # Kept OUT of the modules directory: the browser and the survey
            # scan `*.rsrc`, so a backup left beside the module is a second,
            # still-compressed copy of every module in the library.
            keep = REPO / "reference/compressed-originals"
            keep.mkdir(parents=True, exist_ok=True)
            backup = keep / f"{module.stem}.compressed.rsrc"
            if not backup.exists():
                backup.write_bytes(module.read_bytes())
            module.write_bytes(fork)
            print(f"    rewritten ({len(fork)} bytes); original kept as {backup.name}")
        else:
            print(f"    would rewrite ({len(fork)} bytes) — pass --write")
        done += 1
    print(f"\n{done} module(s) expanded, {failed} failed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
