#!/usr/bin/env python3
"""The project's trap-number table, read from `crates/ad-toolbox/src/traps.rs`.

There used to be a second table, hand-written in Python, and it was **wrong**:
it placed `CountMItems` at `$A950` as "InitControls", `PlotIcon` at `$A93F`, and
`GetItem`/`SetItem` twelve entries low. Trusting it would have produced silent
fidelity bugs, and it had already cost one debugging session.

So there is one table now, and it is the runtime's. Every name in it was derived
from a call site in the modules themselves — the shape of a call identifies the
routine, and a published table cannot lie about that. If a name is missing here it
is missing on purpose: an invented name is worse than `???`.
"""
from pathlib import Path
import re

_RUST = Path(__file__).resolve().parents[2] / "crates/ad-toolbox/src/traps.rs"


def load() -> dict[int, str]:
    """Trap word -> name, parsed from the runtime's `name_of` match arms."""
    text = _RUST.read_text()
    table = {}
    # `        0xA9BF => "GetMenu",` — one arm per trap, nothing else in the file
    # has that shape.
    for word, name in re.findall(r'0x([0-9A-Fa-f]{4})\s*=>\s*"([^"]+)"', text):
        table[int(word, 16)] = name
    if len(table) < 100:
        raise SystemExit(
            f"{_RUST}: parsed only {len(table)} trap names; the table's shape "
            "must have changed, and guessing is what this file exists to stop"
        )
    return table


TRAPS = load()


def canonical(word: int) -> int:
    """Fold a trap word onto the entry that names it.

    OS traps (bit 11 clear) keep the low **eight** bits; bits 8-10 are flags, so
    `$A11E` and `$A01E` are one trap. Toolbox traps keep the low **ten**, and bit
    10 is the auto-pop bit. Masking nine would fold `$AA31` (SetGDevice) onto
    `$A831` and lose half of Color QuickDraw.
    """
    if word & 0x0800:
        return 0xA800 | (word & 0x03FF)
    return 0xA000 | (word & 0x00FF)


def name(word: int, default: str = "???") -> str:
    """The name for a trap word, flag bits and all."""
    return TRAPS.get(word) or TRAPS.get(canonical(word), default)

if __name__ == "__main__":
    for word in sorted(TRAPS):
        print(f"${word:04X} {TRAPS[word]}")
    print(f"{len(TRAPS)} names, from {_RUST}")
