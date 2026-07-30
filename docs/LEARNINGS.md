# Learnings

Everything found the hard way while getting the original 1991 After Dark
resource forks executing on an emulated 68000 against a Rust-implemented
Macintosh Toolbox. This is the project's institutional memory: root causes,
false starts, and the reasoning behind decisions that look arbitrary from the
code alone. It replaces the running planning logs (`PLAN.md`,
`audit_gameplan.md`) that accumulated this during development — those are
gone; this is the distillation.

Read this before re-deriving something from the disassembly or the spec. Most
of the entries below exist because someone already did that and got it wrong
once.

For current, tool-maintained status (not history), see `docs/compatibility-matrix.md`
and `docs/module-findings.md` instead — those shrink/update as work happens;
this file does not.

## Contents

- [Emulator / CPU (68000, Musashi)](#emulator--cpu-68000-musashi)
- [Resource fork / HFS / After Dark file format](#resource-fork--hfs--after-dark-file-format)
- [Toolbox / QuickDraw / Mac OS emulation](#toolbox--quickdraw--mac-os-emulation)
- [Module-specific gotchas](#module-specific-gotchas)
- [Tooling / workflow](#tooling--workflow)
- [Build / packaging](#build--packaging)
- [Testing / audit methodology](#testing--audit-methodology)

## Emulator / CPU (68000, Musashi)

- 6-byte 68000 exception frame; 24-bit address bus (`ADDRESS_MASK =
  0x00FF_FFFF`). Any host address computed above that line silently truncates
  — this caused a real corruption bug (a storage handle written at
  `0x0104_922C` read back as `0x0004_922C`). All host allocations are now
  asserted below that line.
- Musashi's provenance was a mixed tree at import: `LICENSE-musashi.txt` said
  4.10, `m68k.h` said 3.32, `m68kcpu.c` said 4.60. Resolved as one coherent
  4.x tree with stale banners, proven via a `-Wmissing-prototypes` census, not
  assumed. Recorded in `crates/ad-m68k/vendor/PROVENANCE.md`. The licence is
  MIT with no non-commercial clause — the text lives in `readme.txt`, not a
  `LICENSE` file (404 on GitHub), so the readme is vendored since there's no
  standalone licence file to ship.
- `build.rs` originally used the Unix-only `-o` flag (fails under MSVC
  `cl.exe`, which needs `/Fe:`) and used the **target** compiler to build a
  **host**-run code generator, breaking cross-compilation. Fixed: generated
  opcode tables are committed, `build.rs` compiles target sources only, and
  `AD_M68K_REGENERATE=1` regenerates and diffs for drift in CI.
- Determinism verified across `-O0`/`-O2` and three runs — Musashi's cycle
  counts don't depend on host optimization level, which is why the `lab`
  Cargo profile (optimized + overflow-checks) is safe to measure with instead
  of a debug build.
- `Cpu` is a single global (Musashi's register file lives in C globals): it
  must be `!Sync`, not `!Send` — an earlier audit recommendation to make it
  `!Send` was wrong and broke the test harness's `static
  OnceLock<Mutex<Cpu>>` pattern; `Mutex` is exactly the exclusion `!Sync`
  needs. Use `Cpu::try_new()` plus an RAII bus guard (`ACTIVE_BUS`/
  `ACTIVE_VTABLE` thread-locals) so a panic mid-call can't leave a dangling
  bus pointer.
- Pascal calling-convention gotchas, each of which cost real debugging time:
  - The caller reserves the function-result slot (e.g. `CLR.W -(SP)` before
    `_Random`); the callee fills it in place and pops only its own arguments.
    Pushing a second slot leaks 2 bytes/call and eventually hangs the stack
    walk.
  - The A-line exception frame must be popped *before* dispatch — Toolbox
    args sit above the 6-byte frame; a handler reading from `SP` directly
    reads the SR word as its first argument and corrupts it on `RTE`.
  - Pascal `BOOLEAN` results live in the **high byte** of a 2-byte slot
    (caller reads with `MOVE.B (A7)+,Dn`, the even/high byte on 68000).
    Writing to the low byte made every predicate silently read `false` — this
    alone broke `EmptyRect`, `SectRect`, `PtInRect`, `EmptyRgn`, `PtInRgn`,
    `RectInRgn`, `Button`, `TestDeviceAttribute`.
  - Auto-pop trap variant: bit 10 set on an A-line word (e.g. `$AC2E` vs
    `$A82E`) means the glue's return address is pushed on top of the args,
    and the trap must resume execution at the *caller*, not fall through into
    the glue's own dispatch table (which loops forever). Handle this
    generically via `set_resume_pc` in the dispatcher, never per-trap. A
    linear scan for bit-10-set words finds ~66 hits, almost all false
    positives (data aliasing) — confirm only via an execution trace.
  - SANE/FP comparisons (`FOCMP`) return their answer only through the
    condition codes, not `D0`. Missing this caused an infinite loop
    (SunBurst) because the branch tested pre-trap flags. Also: `partial_cmp`'s
    `None` (NaN) must map to the "unordered" CC pattern (V=1 alone), not
    silently coerced to "greater" — that miscoding caused a hang on NaN.
- Higher-CPU-family emulation needs the exception-frame **format word**:
  68010+ appends a format word (frame layout in bits 15-12, vector offset in
  11-0) that `RTE` needs to size its pop correctly. Forgetting this when
  adding 68020 support regressed 54/66 modules to wild jumps.
- `gestaltProcessorType` is a 1-based enumeration, not a raw model number — an
  inline literal `3` intended to mean "68030" actually meant 68020.
- Advertised machine capabilities (`Gestalt`) must match actual CPU/FPU/sound
  behavior, or be set from measurement rather than principle. Concretely:
  `hasColorQD = false` sent Lunatic Fringe down its Mac Plus code path (1-bit,
  left-eighth-of-screen — wrong). But `fpu = false` (honest — no FPU is
  emulated) *fixed* a wild jump: Strange Attractors was executing native
  68881 opcodes the 68000 core doesn't implement; routed through SANE
  instead, it computed correctly. **Rule: each capability must be flipped and
  measured against all 66 modules — telling the truth can help or hurt
  unpredictably, and there's no way to know in advance which.**
- Mandelbrot needs a genuine 68020: its fixed-point inner loop uses
  `MULS.L`/`DIVS.L` (32×32→64 product), absent on 68000. This sat filed as a
  "240s timeout" for a long time purely because the lab was measuring a
  **debug** build (2,200× slower than release, not the assumed ~10×). Real
  fix time once measured correctly: 0.17s optimized. **Profile before
  diagnosing.**
- The emulated clock speed (`clock_hz`) is fundamentally unknowable from the
  binaries — After Dark shipped from Mac Plus to Quadra speeds, and the
  Programmer's Manual says modules are called only "at idle times," so there
  is no canonical frame rate to reproduce, only a machine speed. Left at 8
  MHz (where the regression baseline was measured), exposed as `AD_MHZ` for
  by-eye judgment.

## Resource fork / HFS / After Dark file format

- `CCOD` is a classic segmented `CODE` resource with a 4-byte header (`word
  jumpTableOffset, word entryCount`); segments chain via `offset(n+1) ==
  offset(n) + 8×count(n)`. Confirmed exact on every multi-segment module
  (Lunatic Fringe, Clock, Fish!, Boris). Only 10/66 modules are
  multi-segment.
- Module entry stub (56/66 modules): a 16-byte header — `BRA.S +14 / 'ADgm' /
  <own resource ID as word> / padding` — then `LEA -18(PC),A0 / NOP NOP /
  BRA.W main`. The declared ID matches the actual resource ID in 54/56
  cases. **Always locate the code resource by type `ADgm`, never by a fixed
  ID** (observed IDs: 0, 12, 63, 128, 129). The other 10 modules (Hard Rain,
  GeoBounce, etc.) have no header — code starts at offset 0.
  - Critical bug this exposed: the host used to decode the stub and jump
    straight to `main`, skipping the `LEA`. Think C's `SetUpA4()` then built
    the A4 globals register from garbage, so every `jsr (d16,A4)` leapt into
    zeroed low memory. Fix: enter the resource at **offset 0** and let the
    module run its own entry path. This single fix took full-lifecycle
    modules from 7→14 and wild jumps from 27→1, and retired "unresolved
    entry stub" as a bug category entirely.
- Module ABI, recovered from disassembly and then confirmed verbatim against
  Berkeley Systems' own SDK source (`GraphicsModule_main.c`): `pascal OSErr
  main(Handle *storage, RgnHandle blankRgn, short message, GMParamBlockPtr
  params)`. Selector order is `Initialize(0), Close(1), Blank(2),
  DrawFrame(3), ModuleSelected(4), DoAbout(5), ButtonMessage(8..11)` — **not**
  `Initialize/Blank/DrawFrame/Close` as first inferred; getting this wrong
  deallocates storage on the first frame. `0x0E(A6)` is `blankRgn`, not host
  globals — the module reads `(**RgnHandle).rgnBBox` for screen bounds.
- `GMParamBlock` layout (offset `0x08(A6)`): `controlValues[4]` (user slider
  values 0-100) at +0, `monitors` ptr at +8, `colorQDAvail` at +12,
  `systemConfig` bitmask at +14 (`SoundAvailable`=1<<15,
  `ExtensionsAvailable`=1<<14, `ModuleMayNotAnimate`=1<<9,
  `MultiModuleRunning`=1<<10), then `qdGlobalsCopy`, `brightness`, `demoRect`,
  `errorMessage`, `sndChannel`, `adVersion`, `extensions` table ptr.
  `controlValues[i]` corresponds to `sVal`/`xVal`/`mVal` resource ID
  `1000+i`.
- `params->errorMessage` is not reliable evidence of an actual fault: many
  modules (Bouncing Ball, per its own original source) arm a
  default/pessimistic message ("out of memory") at the top of `Initialize`
  before doing anything, so a captured error string may just be the module's
  guess.
- Settings resource vocabulary, from Berkeley Systems SDK
  `GraphicsModule_Types.r` (authoritative, not inferred): `sVal` (slider, IDs
  1000-1003, value 0-100), `sUnt` (slider unit labels), `bVal` (button — the
  value IS the message number sent on click), `mVal` (menu), `xVal`
  (checkbox), `tVal` (static text, 1-based STR# index), `µVal` (conditional
  control display — the type name is non-ASCII and needs hex-escaping on
  disk: `µVal` → `B5Val`), `sysz` (memory: id 0 = heap+desired, id 1 =
  absolute min), `Cals` (bitflags of understood messages; default = only the
  original four if absent), `Chnl` (sound channel type + volume, AD reads id
  0).
- `sysz` must never be used to size emulated RAM — it's unreliable/
  inconsistently used across modules. Lunatic Fringe's help text says it
  needs ~600K but `sysz 128` decodes to only 25,600 bytes (Hard Rain uses the
  field correctly, at id 0, so it isn't uniformly wrong — just not
  trustworthy in general).
- `Manm` is Lunatic-Fringe-only (all 62 instances live in that one module) —
  decoding it is useful for that module's sprites/debugging, not general
  infrastructure.
- `ADrk 0` (present in 40 modules) is a Pascal string, not a binary
  descriptor: `"Hard Rain 2.0\r©1989, 90 Berkeley Systems Inc."` — usable for
  library metadata/version. 26 modules carry no `ADrk 0` at all, and 5
  modules on the original disk share one copy-pasted descriptor ("Flying
  Toasters 2.0.") — don't use it as a unique display name; filenames are
  unique and match what Finder showed.
- High scores are a fidelity requirement, not a feature to add. Lunatic
  Fringe already has `READHIGH`/`FILEHIGH`/`SHOWHIGH`/`PLAYERCONV` MacsBug
  symbols and writes its score list back into its own resource fork via
  `_AddResource`/`_RmveResource`. The runtime needs a copy-on-write
  resource-fork overlay: the original fork stays immutable, module writes
  land in `overlay.rsrc`.
- A resource is written when the module *says* so
  (`_ChangedResource`/`_WriteResource`/`_AddResource`), never merely because
  its bytes differ from what was loaded. Lunatic Fringe's segment loader
  self-patches its own jump table in place, so its `CCOD` code segment reads
  back 31KB different after execution — syncing "bytes differ → write" would
  persist a corrupted, pre-patched code segment as "saved state." The sync
  (bytes get copied back for read-back correctness) still happens; the
  *dirty marking* must follow the module's explicit intent, not a diff.
- `overlay.rsrc`'s writer must be the exact structural inverse of the reader
  — deterministic ordering, shared name bytes, no timestamps/handles — and
  must refuse rather than truncate on 24-bit/16-bit offset overflow (a
  truncated fork parses successfully but returns wrong bytes, worse than a
  hard failure).
- A removal that leaves nothing behind is not persisted (the overlay format
  is additive-only) — acceptable because every observed module that removes
  a resource immediately re-adds a replacement (Lunatic Fringe's high-score
  save is `RmveResource` then `AddResource`).
- `StoredResource` originally dropped `attrs` and `name_bytes` that the
  parser preserved — this had to be fixed before building any durable-write
  path, or the first save would silently lose data the original had.
- Resource-fork bounds checking: the original `parse_resource_fork.py`
  validated only the two header offsets, then indexed everything else
  unchecked — an internal offset could land inside the map area and still
  "parse." Rewritten to be region-confined by construction (data/map areas
  are bounded sub-slices), verified with 4000 fixed-seed mutation tests plus
  9 malformed-input tests.
- System 7 compressed resources: the After Dark 3.0-era modules (the 3.0 disk
  set, Totally Twisted, After Dark Classic including Rat Race) all fail with
  "unhandled trap `$A89F`" — not because the trap is missing, but because
  their resources are **System 7 compressed resources**, which begin with
  `0xA89F6572` (whose first word IS the `$A89F` Unimplemented A-line trap).
  Handing packed bytes to the CPU as code faults immediately. Fixed offline:
  `tools/audit/decompress_modules.py` shells out to `resource_dasm` (which
  emulates enough of a Mac to run the module's own `dcmp 128` decompressor),
  producing plain uncompressed forks; originals are kept in
  `reference/compressed-originals/`. The runtime's own attempt to run `dcmp
  128` natively fails: that decompressor is self-decrypting and keys itself
  on the trap table (calls `_GetToolTrapAddress`/`_GetOSTrapAddress` for
  `$A89F`, then does `neg.l`/`not.l` self-modification) — since this runtime
  answers trap-address queries with synthetic addresses by design, the body
  decrypts to garbage that runs harmlessly and does nothing. Offline
  conversion via `resource_dasm` is the correct fix; fighting the
  self-decryption is not.
- 3.0-era modules that still decline after decompression are **not** checking
  a version number — tested directly (an `AD_VERSION=0300` override didn't
  change the outcome). Tracing shows they call the Resource Manager to find
  their own file, then hit the File Manager (`_HFSDispatch`) looking for a
  folder — they're searching the disk for the After Dark 3.0 **engine files**
  (`AD 3.0 Code`/`AD 3.0 Sound`), via a routine whose own MacsBug name is
  `GetAfterDarkFilesFolderID`. This runtime has no File Manager/filesystem by
  design (modules run from a bare resource fork), so this lookup always
  fails. Making them run needs a real File Manager with a synthetic volume
  *and* hosting the 3.0 engine binary — a fundamentally larger, different
  kind of project (more like Basilisk II/Mini vMac), explicitly out of
  scope.
- HFS/AppleDouble extraction gotchas, from auditing the original Python
  tools: B-tree header parsing that assumes exactly 3 records in node 0 is
  fragile; extents-overflow handling for the extents file itself was
  silently skipped (fine on the disk image actually used, silent-corruption
  risk on a fragmented one); non-ASCII resource type names (`µVal`) must be
  filesystem-safe encoded (hex-escape) — naive `isalnum()`-based sanitization
  keeps Unicode chars literally and collides cross-platform with
  case-insensitive filesystems.
- `.sit` StuffIt archives from SDK downloads are MacBinary-wrapped: strip the
  128-byte MacBinary header (data-fork length at offset 83, big-endian)
  before unpacking with `unar`.
- Rat Race is **not** Windows-only — an earlier conclusion generalized from
  the discs that were *searched* rather than the discs that *exist*. The
  "After Dark Classic" compilation CD is a hybrid image: the ISO9660 side is
  the Windows installer, but there's a separate HFS volume ("After Dark
  Classic") holding a StuffIt-archive-as-data-fork that `unar` reads
  directly, yielding 21 Mac (68K, not PPC) modules.

## Toolbox / QuickDraw / Mac OS emulation

- The trap surface cannot be enumerated statically (an important
  methodological finding, not just a fact about this codebase): a naive
  linear A-line scan gives 653 distinct words — wildly inflated, because it
  reads sprite/data tables as instructions (e.g. reports 1,864 F-line/FPU
  words in an all-integer game). Recursive-descent disassembly from real
  entry points reaches only 0.6%-10% of code, because most is reached via
  host callbacks / jump-table indirection with data-driven targets.
  **Conclusion: the trap logger has to be Phase-1 infrastructure — hard-fail
  loudly on unknown traps from day one, and let running modules discover the
  real surface.**
- Trap-table correctness is fragile and has bitten this project three
  separate times, always via trusting a wrong/hand-maintained table over the
  module's own call-site evidence:
  1. `$A023`/`$A01F` mislabeled (should be `_DisposHandle`/`_DisposPtr`).
  2. A hand-written table had `CountMItems` at `$A950` mislabeled
     "InitControls," `PlotIcon` three entries early, `GetItem`/`SetItem`
     twelve entries low — cost a full debugging session before being checked
     against a call site.
  3. The Event Manager block was shifted by 2: `$A973` was labeled
     `TickCount` but is actually `StillDown`; the real `TickCount` is `$A975`
     (was mislabeled `WaitMouseUp`). This one had massive impact —
     `TickCount` returning constant 0 meant every module pacing itself on
     tick count skipped every `DrawFrame`, i.e. every visible module looked
     static. `$A975` alone is called from 260 sites across 48/66 modules.
     Fixing this block: Fractal Forest hang→lifecycle, Strange Attractors
     hang→lifecycle, Mountains wild-jump→named-trap, 5 more modules started
     drawing.
  4. `$A82E` was assumed "Colour Utilities selector 0" but was actually the
     auto-pop variant `$AC2E` (bit 10 set) — the return-address-on-stack
     shifted what looked like the selector.
  5. `$A9C3` was labeled `SystemTask` (the real `_SystemTask` is `$A9B4`);
     `$A9C3` is actually `_KeyTranslate`. This broke `KCHR`-based key-name
     lookup, so every Lunatic Fringe control showed as "N" in the UI.
  - **General lesson: a trap table is a claim about someone else's software,
    and the modules are the primary source — identify traps from call-site
    shape/usage, never from a hand-copied reference table.** There is now
    exactly one trap table, in the Rust runtime (`crates/ad-toolbox/src/traps.rs`);
    the Python audit tools read it via `tools/audit/traptable.py` so they can
    never drift from it again.
- `AFTERDARKEXISTS` detection is **not** via `Gestalt`. It's a handshake
  through `_GetOSEvent`: stuff `'aYmm'` into the fake `EventRecord`'s
  `message` field, call with mask 0 (a real Mac leaves it untouched), then
  check if it became `'ADr.'` — if so, `EventRecord.where` points at After
  Dark's info record and `+12` is the AD version. Implementing this single
  handshake cleared the majority of "requires After Dark 2.0" refusals across
  43/66 modules at once.
- The After Dark host ABI is smaller than it first looks. `Sounds.o`/
  `EntryPoints.o` are module-side statically-linked libraries, not host
  callbacks — the runtime doesn't implement `OpenSound`/`PlaySound`/etc.
  directly, it implements the underlying **Sound Manager traps**
  (`_SndNewChannel`, `_SndPlay`, `_SndDoCommand`, etc.) those static libs
  call.
- The extension/callback ABI (`LookUpEntryPoints`) checks `systemConfig` bit
  14, walks `params->extensions` as `{u16 count; {OSType sel; u32
  entryPoints}[]}`, and returns `entryPoints` raw in D0 as a direct pointer
  to a method table, not a handle. An earlier attempt invented an
  indirection layer and immediately corrupted control flow — modules
  executed off their own stack because the fake methods used bare `RTS` but
  the real methods are callee-pops, so the caller's later `RTS` popped
  garbage. **Lesson, worth repeating verbatim: guessed ABI shapes corrupt
  control flow far from the cause.** `'CCOD'` extension slots were never
  actually called — modules load their own segments via `FindMemory` + the
  Resource Manager and execute from the heap; the Segment Loader "mostly
  built itself" once memory services worked.
- `CopyBits` argument-shape disambiguation: discriminated by the word at +4 —
  `0xC000` (both top bits) = `CGrafPort.portBits` (a `PixMapHandle` to
  dereference at +0), `0x8000` alone = a `PixMap`, neither = a 1-bit
  `BitMap`. A naive reader that assumes one shape reads `portVersion` as
  `rowBytes` and gets a stride of 16384 / empty bounds, drawing nothing. Must
  support depths 1/2/4/8 (high-bit-first packed) and transfer modes
  `srcCopy/srcOr/srcXor/srcBic` (+ `not` forms) — XOR erase-and-redraw is the
  era's core animation idiom and must round-trip exactly (blit twice =
  restore background).
- The `PICT` opcode size table matters more than the drawing code — an
  unknown-length opcode must be a hard error, never a guess, because
  desyncing turns the rest of the picture into noise. Version 1 vs 2 have
  different `Version` opcode byte-widths (1 byte vs 2); mishandling this
  desynchronized a title card into a phantom opcode.
- `ctFlags` bit 15 ("indexed by position"): PixMap color tables set this bit
  and leave `value` fields at zero; honoring `value` naively piles every
  entry onto index 0 ("last one wins"), decoding 16-color sprites as one flat
  color.
- Ports must not share a `PixMap`/regions. `OpenCPort` originally handed
  every port the *screen's* handle — redirecting one port moved them all, so
  `CopyBits` read and wrote the same buffer. Similarly, `blankRgn` was
  literally aliased (same handle, not a copy) as the screen port's
  `visRgn`/`clipRgn`, and also installed by default into every new port's
  regions by `OpenPort`/`OpenCPort`. This was a live landmine: as soon as
  clip/visRgn ops went from no-ops to real, Flying Toasters (which opens 5
  sprite ports clipped to their own sprites) got a `blankRgn` read back 32px
  wide, computed `Random() % (spriteWidth - blankWidth)`, and divided by
  zero. **General lesson: a shared mutable handle is not made safe by every
  writer happening to be a no-op. It is made safe by not being shared.**
- QuickDraw default port colors matter: `InitPort` defaults to
  black-on-white (correct for documents, wrong for screen savers) —
  inverting the default to white-on-black (empirically, not per-spec) took
  modules-that-draw from 14→16. Stated as a deliberate *default*, not a hard
  rule; a module setting its own colors is unaffected.
- QuickDraw patterns were originally discarded entirely — modules blank to
  black then draw with a white/grey `Pattern`; ignoring the pattern and
  always using foreground color painted black-on-black. Fixing 8×8
  pattern-bitmap resolution for `PenPat`/`BackPat`/fill/paint/erase ops is
  what made early modules (Snake etc.) visibly work.
- Metric-hiding-the-truth bugs — a recurring pattern that bit the project
  multiple times, in different forms:
  - Counting non-zero pixels: index 0 is white, 255 is black on classic Mac —
    a screen blanked to solid black scored as "fully rendered." Replaced with
    an **ink** metric: pixels differing from the dominant color (a uniform
    screen of any color scores 0).
  - Reading the framebuffer *after* `Close` — modules erase on their way out,
    so working modules scored as blank. Fixed: measure ink live, read before
    `Close`.
  - "Renders" ≠ "animates": a module can draw a correct-looking single frame
    and never update it again. Verified concretely for Flying Toasters:
    byte-identical frames at 2/20/60 `DrawFrame` calls with identical trap
    counts — the visible toasters actually came from `Initialize`, and
    `DrawFrame` returned `noErr` while doing nothing. An earlier report
    ("Flying Toasters flies") was wrong and caught only by an explicit
    `animates` column comparing frame hashes across calls.
  - The determinism column initially reported 16 modules as
    "non-deterministic" — they simply never wrote a frame at all (declined
    at `Initialize`), so "absence of evidence" was being read as "failed the
    test." Fixed by excluding modules with no frame hash from the column —
    the third instance of this exact class of measurement error in the
    project.
  - Regression-gate blind spot: an ink-drift heuristic ("ink wobbles with
    timing, so a smaller nonzero isn't a regression") silently swallowed a
    real behavioral change in 10 modules when the emulated clock speed
    changed (Mountains +106%, Strange Attractors −99%, etc.), because
    determinism is actually 100% reproducible — any diff is real. **Lesson: a
    regression gate that cannot see a change is worse than no gate, because
    it answers the question you asked with a confidence it hasn't earned.**
- Exception-vector defaults: unpopulated vectors point at address 0, so every
  uncaught exception (e.g. divide-by-zero) originally surfaced identically
  as an undiagnosable "wild jump to 0x000000." Vectoring each exception type
  to a named handler that decodes the faulting PC from the exception frame
  turned three separate divide-by-zero bugs into one-line diagnoses instead
  of a shared mystery.
- `_GetClip` as a no-op is dangerous, not neutral: if it hands back an
  *empty* region instead of the caller's actual clip region, a module
  computing a width from that region divides by zero. This single no-op
  caused two modules' "wild jump" classification (ProtoToasters, Major
  Metaphysical Appliances) — fixing the no-op (no new trap implementation
  needed) fixed both.
- Colour Utilities `_ColorUtilities` selector 7 = HSV→RGB — confirmed only by
  tracing real call sites (NightLines builds `{hue, $FFFF, $FFFF}` and feeds
  it to `_RGBForeColor`; under HSL a lightness of `$FFFF` is pure white,
  inconsistent with a colour-cycling saver), not assumed from any spec.
- `SetOrigin` should stay deliberately partial: offsetting `portRect` in
  addition to `visRgn` (which Inside Macintosh technically specifies)
  regressed Boris from 1600 ink to 0, because Boris is multi-monitor-aware
  and reads `portRect` back to place an off-screen entering sprite. Full
  correctness here is blocked on `MachineProfile` supporting multiple
  displays.
- `ColorTable` must not be nil for `clutType` devices — Supernova divided by
  a value read out of the exception vector table (interpreted as
  `ColorTable.ctSize`) because `pmTable` was nil. **A nil `pmTable` on a
  `clutType` device is not a simplification — it is a lie that reads as
  data.**
- SANE (`$A9EB`/`$A9EC`) is computed in host `f64` — 80-bit memory layout
  preserved exactly, but iterated calculations lose precision (53 mantissa
  bits vs 80-bit extended's 64). This is explicitly the one place the
  runtime does not claim bit-exactness.
- Arena/allocator undersizing has caused memory corruption twice, both times
  by the same mechanism: `alloc_host` returned guest address **0** on
  failure, and the caller wrote through it anyway, laying hundreds of KB
  across the 68000 exception vector table (once for the screen framebuffer,
  once for the `PICT` staging buffer) — both times costing ~10 working
  modules and going undetected until a full survey re-run. Fixed by changing
  the return type to `Option<NonZeroU32>` plus `reserve_host(size, what)`,
  which panics by name on undersizing, making the failure unrepresentable
  rather than merely documented.
- Allocator conservation is two separate invariants, not one blanket claim:
  the guest heap (Mac Memory Manager handles/pointers) must return exactly
  to its starting free space after every alloc/dispose cycle; the host arena
  (bump allocator backing fixed host structures) never shrinks by design —
  assert its *capacity* instead.
- A `TickCount` accumulator using `u32` + `saturating_add` created a false
  "hang" at exactly ~9 minutes of continuous play (8 MHz × `u32::MAX` cycles
  ≈ 9 min), compounded by the watchdog treating "budget = `u32::MAX`" (meant
  as "no limit") as equal to the saturated accumulator value. Fixed with
  `u64` and a named `NO_CYCLE_LIMIT` sentinel instead of an implicit large
  number.
- A game module can "own the machine": Lunatic Fringe's `DrawFrame` never
  returns while play is active — by design, that's literally how the
  original screen-saver-as-arcade-game worked. The host runs it in bounded
  cycle-budget chunks and uses two seams to interact mid-call:
  `Host::set_present_hook` (fires from inside the emulator's tick loop to
  publish frames / pump the window) and `Host::set_key_source`/
  `set_mouse_source` (polled each tick, edge-driven). Closing the window sets
  a quit flag that unwinds the call from *outside* — the only way to stop a
  module that never returns; its mid-frame state is abandoned deliberately,
  then `Close` is called.
- Interrupt-style delivery (VBL tasks, patched `_PostEvent`) needs full
  register save/restore and an executable "old vector" to chain to —
  modules patch traps themselves (`_SetTrapAddress`) and the runtime has to
  track and honor those patches (`_GetTrapAddress` must report them), with
  the synthetic trap table becoming real RTS-stub slots so hooks can
  chain-jump to the previous vector.

## Module-specific gotchas

- Lunatic Fringe's key bindings live in resource `LFky 128` (two 7-entry,
  8-byte-record tables — primary and alternate) and were extracted by
  *reading the loader code*, never guessed or borrowed from another
  reimplementation. The loader (`CCOD -2043 +0x878`) does `BlockMove(h,
  primary, 0x38); BlockMove(h+0x38, alternate, 0x38)`, and both scanners
  index with `asl.l #3` over 7 entries. Record layout: `[0]` kind — 0 matched
  by keycode through the patched `_PostEvent`, nonzero matched by mask
  against the low-memory `KeyMap`; `[1]` character, `[2]` keycode, `[4..8]`
  `KeyMap` mask. Primary bindings: rotate = keypad 4/6 (alt `l`/`'`), fire =
  Command (`KeyMap` mask `0x8000`), thrust = keypad 5 (alt `;`), super-thrust
  = keypad 8 (alt `p`), shield = keypad 0 (alt space), abort = `a`, pause =
  Caps Lock, read straight from low-memory global `$178` — the game never
  polls the event queue for this, it's a raw `KeyMap` bit test.
- Two distinct input mechanisms in one module: Caps Lock (a modifier) is
  polled directly from the `KeyMap` long at `$178`; ordinary keys arrive as
  events, but the game patches `_PostEvent` itself at startup and reads
  keyDown/keyUp at interrupt time rather than polling — meaning the host
  must support trap-patching and interrupt-style delivery, not just an event
  queue.
- Caps Lock cannot be delivered on macOS at all: produces no `keyDown:`, only
  `flagsChanged:`, and common windowing libs (`minifb`) don't surface it.
  Also, on a real Mac, Caps Lock is a *lock* (state persists while the light
  is on), not a momentary key — modeling it as "held while pressed" is
  simply wrong on every platform, not just macOS. Solution: model it as a
  latch toggled by ordinary ASCII keys as substitutes, never rely on any
  single key (an early single-substitute choice, `Tab`, turned out to be
  silently eaten by Cocoa's focus-traversal system — the lesson is not "use a
  different key," it is "do not depend on one key").
- macOS eats `keyUp:` for ordinary keys while Command is held — holding a
  turn key, pressing Command (Fire) then releasing the turn key produces no
  release event at all, causing the ship to turn forever. No event-level fix
  exists (the window server never sends the event); worked around via an
  auto-repeat heartbeat (a genuinely-held key keeps producing down-edges; a
  key whose release was swallowed goes silent) with a 450ms grace period,
  longer than the OS's 250ms default autorepeat delay.
- Sound loop-point idiom: a one-sample loop window on the *last* sample
  (`loopStart = len-2, loopEnd = len-1`) is the format's idiom for "this is a
  one-shot, not a looping note" (loop fields can't be literally zero for
  note-playback semantics, so they're parked at the end). A mixer that
  honors these loop points literally holds that sample forever, producing a
  continuous DC tone instead of discrete one-shot sounds — this exact bug
  turned Flying Toasters' flap sounds into a steady tone.
- Play sessions must dedupe sound decode: naive re-decoding on every
  `_SndPlay` call decoded the same resource 59 times for 59 shots in one
  Lunatic Fringe session and never freed any of them. Fixed with a cache
  keyed on resource bytes, returning a shared `Arc`.
- PICS Player was misdiagnosed as a hang for 3 sessions — it actually
  returns `ModuleError` from `Blank` cleanly ("Couldn't find selected PICS
  file"), but the host kept calling `DrawFrame` on a module that had already
  failed init, spinning 50M cycles against unbuilt state. A guard already
  existed for failed `Initialize` but not failed `Blank`.
- Say What? draws its own foreground color as black over its own black blank
  — traced to `ForeColor(329)` at a specific call site that doesn't decode as
  any of the 8 classic QuickDraw color constants (the classic set encodes as
  `(hue << 6) | (4·hue + 1)`, and 329 decodes to hue 5's high bits mixed with
  hue 2's low bits). Root cause not yet found — candidates: an unstubbed
  color-table trap, `InvertColor`, or a Palette Manager call. Open item, not
  solved.
- Blackboard "draws nothing" is a clip-region limitation, not a missing
  trap: it correctly builds region-based letter shapes
  (`SetRect`/`RectRgn`/`UnionRgn`, `SetClip`, `DiffRgn`), but the region
  model only tracks bounding boxes, so fills aren't actually clipped to
  complex regions and the intended chalk paint never lands correctly.
  Diagnosed as a QuickDraw milestone (real region algebra), not a patch.
- A font's `owTLoc` field is the actual structural checksum for validating
  `FONT`/`NFNT` strike parsing (it counts words from its own address to the
  offset/width table, so it must resolve exactly). Real fonts from a live
  System file corrected two assumptions from the format spec: Geneva 9/12
  both omit the documented `$FFFF` terminator in the offset/width table
  (each 2 bytes short — requiring it rejects legitimate fonts), and
  `fRectHeight` is **not** the point size (Geneva 9 is 12 rows tall with
  ascent 10). **No font may ever be bundled** — these are Apple's, read live
  from the user's own System file; absence must degrade to a text listing,
  never a substitute typeface.

## Tooling / workflow

- QEMU oracle capture (`tools/oracle/qemu_capture.py`): boots
  `qemu-system-m68k -M q800` with a genuine dumped Quadra ROM, connects over
  **QMP**, does `screendump` captures at scheduled times, converts PPM→PNG
  with a dependency-free encoder, emits a manifest with per-frame CRC32 +
  non-black pixel counts. **The QMP Unix socket must live in a short
  directory** — macOS caps `AF_UNIX` paths near 104 bytes.
- A CD boots without needing an on-disk SCSI driver — this unblocked oracle
  bring-up. A bootable hard-disk image needs an `Apple_Driver43` partition
  (driver loaded from a Driver Descriptor Map in block 0) for a real ROM to
  see it at all; a bare HFS volume was invisible to a real ROM (only worked
  under emulators like Basilisk II that fake the driver). But a CD boots the
  driver straight from ROM, so a freely-downloadable Apple System 7.5.3
  release boots to Finder with zero extra work.
- Driver grafting across block sizes doesn't work: copying a CD's
  (2048-byte-block) SCSI driver into a hard-disk (512-byte-block) partition
  map produces a volume that neither mounts nor boots — confirmed via
  byte-identical screen CRC with/without the graft. The right approach (not
  yet implemented) is having the guest OS write its own driver via QMP
  `input-send-event` GUI automation.
- `tools/lab/survey.py`/`matrix.py` run every module headlessly and grade
  with an evidence-based, per-claim matrix
  (`docs/compatibility-matrix.md`) rather than a single pass/fail number —
  directly motivated by the metric-hiding bugs above. Must run against the
  `[profile.lab]` Cargo profile (release-derived but keeps
  `overflow-checks`/`debug-assertions`): debug alone was ~2200× slower on
  the worst case (falsely flagged Mandelbrot as hung), and *also* the only
  build that caught a real `i32` overflow in the oval rasterizer (Pearls) —
  release alone silently wrapped and drew a wrong shape instead of
  panicking. Neither plain debug nor plain release is the right instrument;
  the hybrid profile is required.
- Instrumentation environment variables for reverse-engineering a module
  live (not guessing from static disassembly alone): `AD_QD_LOG` (drawing
  destinations + resource load addresses), `AD_WATCH_ADDR=<hex>[+len]`
  (writes into a byte range with PC), `AD_WATCH_PC=<hex>` (what a specific
  instruction stores), `AD_WATCH_SCREEN` (attribute direct screen writes to
  code), `AD_TRACE_EVENT` (trace one patched-trap delivery through
  glue/segment loader), `AD_KEYS`/`AD_BUDGET` (scripted input by
  frame/tick, per-message cycle ceiling), `AD_PNG_DIR`+`AD_PNG_EVERY` (frame
  dump from inside the tick loop — necessary for games that never return
  from `DrawFrame`), `AD_WAV_DIR` (every played sound as WAV), `AD_MIX_WAV`
  (whole-session mixed audio through the real mixer at real tick offsets —
  catches bugs the per-sound WAV dump can't, e.g. the loop-point/DC-tone
  bug). These are typed `RuntimeOptions` read in exactly one place
  (`ad-runtime`), not scattered `std::env::var` calls inside trap handlers —
  a shipped library must not have its rendering silently altered by an
  unrelated env var.
- Two disassembler pitfalls that cost real debugging time: (1) `current_pc()`
  read during a bus *write* returns the **next** instruction on Musashi (PC
  is advanced before the operand write completes) — a PC-keyed watch
  expecting the storing instruction's own address finds nothing and looks
  like a dead code path when it isn't; (2) a hand-written Python
  disassembler's PC-relative base was 2 bytes early for `MOVEM` specifically
  (it has two extension words), making a legitimate handler pointer look
  like garbage.
- Think C / MPW MacsBug name extraction (`tools/audit/thinkc_names.py`,
  `macsbug.py`) recovers the original developers' own function names for
  free from the compiled binary (space-padded 8-char names, or a
  high-bit-length-byte + string variant, emitted after each function's
  terminating RTS/JMP) — this is how Lunatic Fringe's internal routine names
  (`MOVEPLAYER`, `CREATESHOT`, `XPOSTEVENT`, `WRITEHIGHSCORES`) were
  recovered without any guessing.
- Two independent implementations of the same parsing/extraction logic (e.g.
  `hfs_audit.py` vs `dump_all_forks.py`) are kept deliberately, on purpose —
  **two implementations disagreeing is how parser bugs get caught.**
- Original enemy order / RNG-driven behavior should never be assumed fixed
  from a first read: trace the actual RNG calls and prove whether a sequence
  is fixed, seeded, weighted, or conditional before treating it as a literal
  table. This generalizes: don't assume, trace and prove.

## Build / packaging

- Windows `.scr` model (`/s`, `/c`, `/p <HWND>`) verified current on Windows
  11.
- XScreenSaver passes the target window ID via the `$XSCREENSAVER_WINDOW`
  environment variable — a saver that creates its own window instead will
  not work.
- Tauri 2 `externalBin` sidecars have known open issues: macOS
  codesigning/notarization gaps, and the NSIS installer not replacing a
  sidecar on upgrade/reinstall — both need solving deliberately in the
  packaging phase, not discovered at release.
- **macOS third-party screen savers cannot receive keyboard events at all
  since Catalina** (see Apple Developer Forums thread 120901 / FB6916019,
  unanswered since 2019, and the Aerial project's issue 1267 hitting the
  same wall) — this is a hard platform limitation, not a
  permissions/entitlement issue. Consequence: interactive modules on macOS
  are arcade-mode-only, and — for consistency across platforms even though
  Windows/Linux don't have this restriction — interactive modules are kept
  to arcade mode everywhere. There will not be a `.saver` plug-in for
  playable modules.
- macOS `.saver` bundles run inside a sandboxed `legacyScreenSaver.appex`
  host since Catalina: they can read most of disk but can only *write*
  inside a specific Containers path; `stopAnimation` is never called in
  normal operation (Sonoma+); a new `ScreenSaverView` is created on each
  start without releasing the old one, so two instances can run
  concurrently. Any design assuming writable state or single-instance from a
  `.saver` is wrong on modern macOS.
- Nothing is signed with a paid certificate (no Apple Developer ID, no
  Windows Authenticode cert) — a deliberate choice. On macOS the app is
  still ad-hoc signed (`codesign -s -`, free, no account needed) since a
  completely unsigned binary is killed by the kernel on Apple Silicon rather
  than merely warned about; it just isn't *notarized*, so first launch needs
  right-click → Open. The Gatekeeper/SmartScreen click-through steps are
  documented for users rather than paid away.

## Testing / audit methodology

These are meta-lessons about how to verify claims on this kind of project,
proven the hard way and worth following on anything similar.

- **"Replicated" is stricter than "runs."** A module completing its
  lifecycle without an unhandled trap is not evidence it's correct. Use a
  multi-column evidence matrix (imported / initializes / renders / animates
  / determinism / sound / mixed / audible / settings / persist / stable /
  fidelity) where each column reports `--` (not evidenced) rather than
  pass/fail, and never conflate absence of evidence with failure.
- **Every real bug fix here came from running real modules against real
  disassembly, not from unit tests or spec-reading alone.** Several of the
  most consequential fixes (Event Manager block, extension ABI, CopyBits
  argument shapes) were preceded by *failed* attempts that guessed a
  structure from a spec/intuition and corrupted control flow in ways that
  were hard to localize. The method that actually works: read the real call
  site in the real module, then implement exactly that.
- **A regression baseline, and negative-testing that baseline, is required
  infrastructure — build it first, not as an afterthought.** Verify it by
  forging a false-positive baseline and confirming the tool reports
  `WORSE:`/exits nonzero.
- **Every bug-fixing commit's test should be verified to fail with the fix
  reverted.** A test that passes before and after a fix proves nothing about
  the fix.
- Metric design is itself a source of bugs, repeatedly (see the "metric-
  hiding-the-truth" entries above): a metric that can't distinguish
  "rendered" from "blanked," "animates" from "drew once," or "no evidence"
  from "failed," will confidently report the wrong thing. Design metrics to
  make the failure mode impossible to represent, not just unlikely.
