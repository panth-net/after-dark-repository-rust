#!/usr/bin/env python3
"""Extract Think C fixed-format (8-char) MacsBug names from 68K code.

Think C with "MacsBug names" on emits, after each function's terminating
RTS/JMP, eight bytes of space-padded ASCII naming the function that just
ended. This is the older fixed format (the variable-length high-bit format
is handled by macsbug.py). The names are the original developers' own
function names and are ground truth for what a routine does — e.g. Lunatic
Fringe's engine labels its ship-control and weapon routines.

    tools/audit/thinkc_names.py <file.rsrc> <TYPE> [id]

Prints: end-offset  name   (the function ENDS at the RTS just before).
"""
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from rsrc import parse  # noqa: E402

NAME8 = re.compile(rb"[A-Z][A-Z0-9_ #]{7}")
ENDERS = (b"\x4e\x75", b"\x4e\xd0", b"\x4e\xd1")  # RTS, JMP (A0), JMP (A1)


def extract_fixed(data: bytes):
    """Yield (offset_of_name, name) for each fixed-8 name record."""
    for i in range(0, len(data) - 10, 2):
        if data[i : i + 2] not in ENDERS:
            continue
        chunk = data[i + 2 : i + 10]
        if NAME8.fullmatch(chunk) and chunk.strip():
            yield i + 2, chunk.decode("ascii").rstrip()


def main() -> int:
    path, rtype = sys.argv[1], sys.argv[2]
    want_id = int(sys.argv[3]) if len(sys.argv) > 3 else None
    data = Path(path).read_bytes()
    for r in parse(data):
        if r["type"] != rtype:
            continue
        if want_id is not None and r["id"] != want_id:
            continue
        print(f"=== {rtype} {r['id']} ({len(r['data'])} bytes) ===")
        for off, name in extract_fixed(r["data"]):
            print(f"  {off:#08x}  {name}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
