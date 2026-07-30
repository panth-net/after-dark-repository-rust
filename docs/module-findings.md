*AI written*
# Per-module findings that are not yet fixes

What is known, with evidence, about the modules that still do not show what they
should — so the next session starts from facts rather than from "it doesn't
work". Anything fixed gets deleted from here; this file should shrink.

The systemic fix that emptied most of the earlier version of this list: the
param block's QD globals copy shipped with all five `Pattern`s zeroed, so the
SDK's canonical blank (`FillRgn(blankRgn, qdGlobalsCopy->qdBlack)`) filled with
the white pattern, and two generations of "default port colour" hacks were
tuned to make that accident look right. Patterns are now written (see
`build_param_block`), port colours are per-port (see
`QuickDraw::switch_port_colours`), framed ovals record into open regions, and
lines honour the pen pattern. That brought back Bouncing Ball, Logo, Globe,
FadeAway, ProtoToasters, Spin Brush and Lissajous's colours in one stroke.

## Say What?  — quotes drawn in black ink over its black blank

- The module runs its whole lifecycle and *does* draw: words arrive one at a
  time at y=182 with correct pen advance (`GetPen`/`StringWidth` per word).
- Its blank is the canonical `qdBlack` fill (now correctly black), and the
  first quote is drawn after an explicit `ForeColor(33)` — blackColor. Black on
  black. That cannot be what the original showed, so something about the
  *sequence* is still misunderstood.
- It later calls `ForeColor(329)`. 329 is not one of the eight classic colour
  constants; the classic set encodes as `(hue << 6) | (4·hue + 1)` and 329
  decodes to hue 5's high bits with hue 2's low bits. Where 329 comes from —
  a computed table, a saved-and-restored port field, or something else — needs
  the disassembly around the call site at code offset `+0x05fe`.
- Suspects, in order: a colour-table trap we stub that the module uses to pick
  quote colours; the quote colour intended to arrive via `InvertColor` or a
  Palette Manager call; the black first pass being a shadow/measure pass whose
  visible pass never runs because a trap answers wrongly.

## Blackboard — letters spelled, regions built, chalk never painted

- Spells words by looking up `HVof` stroke resources keyed by ASCII code
  (`HVof 110` is 'n'); the lookups succeed.
- Builds letter shapes as regions — thousands of `SetRect`/`RectRgn`/`UnionRgn`
  per minute — uses `SetClip`, `EmptyRgn`, `DiffRgn`… and then never issues a
  paint verb. One `FillRgn` in 60 frames, which is the blank.
- So the drawing it intends is clipped-region work our region model (bounding
  boxes only, `SetClip` accepted but fills not clipped) cannot yet express.
  The likely real fix is honouring the clip region in the rasteriser and
  letting a region be more than its bbox. That is a QuickDraw milestone, not a
  patch.

## Meadow — blanks and stops

- Runs clean, blanks black, and draws nothing else in 120 frames; almost no
  drawing traps at all after Blank. Needs its own investigation from scratch —
  possibly waiting on a trap that answers "nothing" where the real Mac said
  something (its census is small, so this should be quick to diff).

## Messages — needs TextEdit

- Declines at `_TENew` ($A9D2), honestly reported as `needs trap`. Drawing
  scrolling text is its whole job and it does it through TextEdit
  (`TENew`/`TESetText`/`TEUpdate`…). A minimal TE record implementation is a
  bounded, known piece of work — but it is a subsystem, not a stub.

## Movies 'Til Dawn — needs QuickTime

- Declines cleanly with its own message: "QuickTime must be installed". It
  plays QuickTime movies; the runtime has no QuickTime and faking the Gestalt
  answer would only move the failure somewhere less honest. Out of scope
  unless movie playback ever becomes a goal.

## Mountains — renders (check your expectations against the matrix)

- Draws and animates headless: fractal wireframe with filled flats. The fill
  shading is banded/pink where the original shaded solid facets — polygon
  fill with patterns is approximate. Improvable, not broken.

## Rat Race — a Mac module after all, blocked by the 3.0 engine ABI

**Correction (2026-07-27).** An earlier entry here concluded "Windows-only, no
Mac version". That was wrong, and wrong for a reason worth remembering: it
generalised from the discs I had *searched* (After Dark 3.0 floppies, Totally
Twisted) to the discs that *exist*. After Dark **Classic** — a later
compilation — carries a Mac `Rat Race`, and it is now in `modules/`.

Where it hides, for anyone re-treading this: the Classic CD is a hybrid image.
The ISO 9660 side is the **Windows** installer (`SETUP.EXE`, `RATRACE.ZIP`,
`DUNZIP.DLL`) — that side is what the old, wrong conclusion was drawn from. The
Mac side is a separate HFS volume ("After Dark Classic") holding one StuffIt
InstallerMaker application whose data fork *is* a StuffIt archive; `unar` reads
it directly, and 21 Mac modules fall out.

It is **68K, not PowerPC** — so the CPU is not the obstacle. The obstacle is
that its `ADgm 0` is a 1331-byte stub whose very first instruction is the
`$A89F` *Unimplemented* A-line trap, with the real code sitting in `CODE`,
`MAIN`, `JUMP` and `DATA` resources. That is the After Dark 3.0 engine's module
loader convention: the engine patches `$A89F` and dispatches through it into
segmented code. So Rat Race needs exactly the same missing piece as every other
3.0-era module (below), plus the Segment Loader work (see `docs/LEARNINGS.md`).

## The After Dark 3.0 era: SOLVED — the resources were compressed

**Resolved 2026-07-27.** Every 3.0-era module — the 3.0 set, Totally Twisted,
and the After Dark Classic additions including Rat Race — reported "unhandled
Toolbox trap $A89F" for one reason: their resources are **System 7 compressed
resources**, and a compressed resource opens with the long `0xA89F6572`, whose
first word is also the *Unimplemented* A-line trap. Handing packed bytes to a
CPU faults on the first instruction. Nothing was wrong with the modules.

### How they are expanded

`tools/audit/decompress_modules.py` converts them once, offline, using
[`resource_dasm`](https://github.com/fuzziqersoftware/resource_dasm), which
emulates enough of a Macintosh to run the decompressor the modules carry:

```sh
tools/audit/decompress_modules.py <path-to-resource_dasm> modules --write
```

It reads each fork and writes it back through resource_dasm's fork-to-fork mode,
which expands every compressed resource in transit — no per-resource extraction,
no rebuilt resource map. Originals are preserved in
`reference/compressed-originals/`, so the conversion is reversible.

Verified per module, because a decompressor mishandled produces *plausible*
output: the resource set must be unchanged, every uncompressed resource must be
byte-identical, every expanded resource must be exactly the length its own
header declared, and nothing may remain compressed.

**Result: 20 modules expanded, 0 failures.** Rat Race's `ADgm` went from 1331
packed bytes to 1926 bytes opening `600E 0000 'ADgm'` — the standard header and
entry stub.

### Why our own runtime still cannot do it

`ad-host-v2/src/decompress.rs` runs the module's own `dcmp 128` on our 68K core
and is left in place, because the diagnosis it produced is worth keeping: that
decompressor is **self-decrypting**, and it keys itself on the **trap table**,
calling `_GetToolTrapAddress` for `$A89F` and `_GetOSTrapAddress` before
rewriting its own instructions with `neg.l`/`not.l`. This runtime answers
`_GetTrapAddress` with synthetic addresses by design, so the body decrypts to
garbage that runs harmlessly and returns — which is exactly what it does.
resource_dasm succeeds because it emulates a fuller machine. Converting offline
is the better trade: the runtime keeps its trap policy, and the modules arrive
as ordinary uncompressed forks.

### What the expanded modules do now

Decompression was necessary, not sufficient — but it moved every one of them
from a fault to an honest answer:

- **Guts, Zooommm!, Nonsense** run their full lifecycle. Guts renders animated
  colour ribbons (19,898 pixels, 45 colours).
- **Most of the rest decline in their own words**: "This module requires After
  Dark 3.0 or later." See the section below for what that check really is —
  it is not a version number.
- **A few need specific traps**, now individually named rather than all hiding
  behind `$A89F`.

Library totals after this work: **62 full lifecycle, 29 declining with a
reason, 6 needing a named trap.**


## Why the 3.0 modules decline, precisely — and why it is not a version flag

The obvious theory is that they read `params->adVersion` and refuse below
0x0300. That was tested and is **wrong**. There are two places a version can be
read — the param block and the AD info record — and `AD_VERSION=0300` now moves
both (it defaults to 0x0200, so nothing changes unless asked). With 3.0 declared
in both, Rat Race, Bugs and Bad Dog give the *same* refusal.

Tracing the refusal shows the real sequence. From Bugs' trap history and the
instructions before it returns:

```text
    $A81F  Get1Resource      @ +0x064c
    $A994  CurResFile        @ +0x0680
    $A260  HFSDispatch       @ +0x071a     <- the File Manager
    $A02E  BlockMove
    $A9A3  ReleaseResource
    ...
    +0x0098  tst.l ($750,A4)   ; a module global, still zero
    +0x009c  beq  $80a8        ; taken
    +0x00a8  rts               ; -> "requires After Dark 3.0 or later"
```

So the module asks the Resource Manager which file it came from, then goes to
the **file system** to look around that file's folder, and gives up when it
finds nothing. This is the same idiom Slide Show uses, whose own MacsBug symbol
names the routine `GetAfterDarkFilesFolderID`. The modules are not checking a
number; they are **looking for the After Dark 3.0 engine on disk** — the
`After Dark 3.0` application and its `AD 3.0 Code` / `AD 3.0 Sound` files, all of
which are in `reference/compressed-originals`' source payload.

This runtime has no file system. Every File Manager call answers `nsvErr` (no
such volume), by design: modules run from a resource fork handed to them, and
nothing has needed a directory until now.

**Making these run therefore needs two things this project does not have:**

1. **A File Manager with a synthetic volume** — enough of `PBGetFCBInfo`,
   `PBGetCatInfo` and friends, backed by a fabricated "After Dark Files" folder
   containing the engine files, for the lookup to succeed.
2. **The 3.0 engine actually hosted.** Finding the file is only the first step;
   the module then loads and calls into `AD 3.0 Code`, which is a large 68K
   application expecting a full Macintosh around it. Running it means this
   project stops being a Toolbox HLE for single modules and starts being a Mac.

That is a larger undertaking than everything in this file combined, and it is a
different *kind* of project. Worth stating plainly rather than filing as a
to-do: for the 3.0-era modules, a real emulator (Mini vMac, Basilisk II) running
a real After Dark 3.0 install is the appropriate tool, and this runtime's reach
is the 2.0-era ABI it was built for — where it now runs 62 modules.
