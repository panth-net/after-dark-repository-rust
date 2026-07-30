# Musashi provenance

This tree is a vendored copy of Musashi, the portable M680x0 emulation engine by
Karl Stenerud. It arrived here with **no upstream URL, tag, or commit recorded**
(the commit that added it, `9defd0c`, says only "Musashi (MIT, vendored with its
readme as the licence)"), and its per-file version banners disagree with each
other. This file records what is actually here, established by reading the
source rather than by trusting the banners.

Everything below was verified against the files at the hashes listed in
§Manifest. Re-verify with `AD_M68K_REGENERATE=1 cargo build -p ad-m68k` (which
re-derives `generated/` and fails on drift) and by re-running `shasum -a 256`.

## Manifest

sha256, as vendored. `generated/` is produced by `m68kmake` from `m68k_in.c` and
is committed so the normal build compiles target C only — see `../build.rs`.

| sha256 | file |
|---|---|
| `ecff54ee43a229d39aaa6def046c95d5133f8f936ba83a6e62af50ac01832a4f` | `LICENSE-musashi.txt` |
| `7fb10f51ee36d45f6e2be38d72aa0c57853810a816f1cf19e0f0c600bd1afa4a` | `m68k.h` |
| `7a5836fa60ac5c59f77adbcfe086a288d5356c3d373ab15d337986a4e51e0132` | `m68k_in.c` |
| `f87e48179ace8b1a7f85c660378e83357a29e2682acbe046909d0afc63110a26` | `m68kconf.h` |
| `0d98c1bfd104929ca393ab6020c05adaa3de5433961cbf924d0ab78a82778405` | `m68kcpu.c` |
| `372d2086a9c39aadf87d9e0fa42104c5849bd4972ce74b191c0d458208193635` | `m68kcpu.h` |
| `c31b1d3a9863c4271fd0ddcddf3cde8fadac691909ca994db6ec41941696dca2` | `m68kdasm.c` |
| `e86c9eaaa51993c2d0fac1f6d489bafbe3d4209ff74163e97b7720011fe4327a` | `m68kfpu.c` |
| `c6da040b9f9b4a2a30aabc7b26f7228796ef6090934002d9c8a788d754904788` | `m68kmake.c` |
| `66bdbef424dc672c79d316cad6060d487a9cd574b0c12d008e82505a4a27df89` | `m68kmmu.h` |
| `f36a23ff55ba012081b0b28815c23c975782dbca5eb1e10dbaf8825d14bb117e` | `generated/m68kops.c` |
| `f7a4709892d071901c677afafbab575fd99945c76cf1e9869789b54fac13199a` | `generated/m68kops.h` |
| `c71e555b87858bcbd6629ca2a1b181babaf8fe129172c15489a9eeeb56a6dc1f` | `softfloat/README.txt` |
| `0060a07e2a2292bb98a79917a8ddd02ae3225173ce37d919eb7e76a8cc661aac` | `softfloat/mamesf.h` |
| `48da7850cbd2481c3d87f7bde42815d0e64d08a2772afb847b5f91d90afcf9be` | `softfloat/milieu.h` |
| `003fd1da7dac65e74c3129af0a45a2fb238140d281ccf456b2f3d18ac633e840` | `softfloat/softfloat-macros` |
| `ec8df128b1ebd711215e39d35ac62c8bde59f8bcd0cf88718d8fa5d18a475d72` | `softfloat/softfloat-specialize` |
| `a9249098c1be64f346a20d72250be47e779764819afed2c6ecce7f8687476db6` | `softfloat/softfloat.c` |
| `5cf048506ee233be66e1ec0ce7f15e0184e3758c14c41829a7f3ab0195f7b509` | `softfloat/softfloat.h` |

## What the banners claim

| file | banner |
|---|---|
| `LICENSE-musashi.txt` (upstream `readme.txt`) | Version **4.10** |
| `m68kcpu.c` | Version **4.60** |
| `m68kmake.c` | Version **4.60** (and `g_version[] = "4.60"`, printed at run time) |
| `m68kcpu.h` | Version **4.5** |
| `m68k.h` | Version **3.32** |
| `m68k_in.c` | Version **3.32** |
| `m68kconf.h` | Version **3.32** |
| `m68kdasm.c` | Version **3.32** |
| `m68kfpu.c` | none — no banner, no copyright header |
| `m68kmmu.h` | none — "By R. Belmont", © Nicola Salmoria and the MAME Team |
| `softfloat/` | SoftFloat release **2b** (John R. Hauser, 2002), repackaged for MAME |

## Determination

**This is a single coherent 4.x-generation tree, not a mixture of releases. The
"3.32" banners are stale headers on files that were modified without a version
bump; they are provably not 3.32 code. The header and the implementation are
compatible.**

Musashi 3.32 emulated 68000/68010/68EC020/68020 and had no FPU, no MMU, and no
SoftFloat. The 4.x line added 68EC030/68030/68EC040/68040, `m68kfpu.c`,
`m68kmmu.h`, and the SoftFloat import. Every file banner-marked 3.32 here
contains 4.x-only content:

- **`m68k.h` is not 3.32.** Its `M68K_CPU_TYPE_*` enum (lines 96–106) lists
  `68EC030`, `68030`, `68EC040`, `68LC040`, `68040` and `SCC68070` — none of
  which exist in 3.32. Its own prose two hundred lines later (lines 311–312)
  still says "Currently supported types are: M68K_CPU_TYPE_68000,
  M68K_CPU_TYPE_68010, M68K_CPU_TYPE_EC020, and M68K_CPU_TYPE_68020", so the
  file demonstrably outran its own documentation *and* its banner. It also
  declares post-3.32 API: `m68k_get_virq`/`m68k_set_virq`, `m68k_set_fc_callback`,
  `m68k_set_illg_instr_callback`, `m68k_set_bkpt_ack_callback`,
  `m68k_state_register`, `m68k_disassemble_raw`. It is in fact newer than the
  4.10 readme too: that readme enumerates "68000, 68010, 68EC020, 68020, 68EC030,
  68030, 68EC040 and 68040" and knows nothing of `68LC040` or `SCC68070`.
- **`m68k_in.c` is not 3.32.** It declares the 68040 FPU and PMMU entry points
  `m68040_fpu_op0`, `m68040_fpu_op1`, `m68881_mmu_ops` (lines 286–288) and
  dispatches to them (lines 922, 933, 8383), and it carries `M68KMAKE_OP(pmmu, …)`
  and `M68KMAKE_OP(move16, …)`. 3.32 has none of this. Decisively, its table
  format is the 5-CPU format: `#define NUM_CPU_TYPES 5` (line 137) and every
  opcode row carries five availability columns and five cycle columns
  (`1010 0 . . 1010............ .......... U U U U U 4 4 4 4 4`). 3.32's table is
  three CPUs wide.
- **`m68kconf.h` is not 3.32.** It defines `M68K_EMULATE_030`,
  `M68K_EMULATE_040` and `M68K_EMULATE_PMMU`, switches that do not exist in 3.32.
- **`m68kmake.c` (4.60) parses `m68k_in.c` cleanly**, which is only possible if
  the two agree on that table format: `NUM_CPUS = 5` (lines 137–142). It emits
  1967 opcode handlers from 518 primitives with no diagnostics.

### Header/implementation compatibility — evidence

The build compiles `m68kcpu.c`, `m68kdasm.c` and `generated/m68kops.c`, each of
which reaches `m68k.h` through `m68kcpu.h`, so any prototype disagreement would
be a hard C error rather than a subtle mismatch. Compiling all three with
warnings deliberately turned **on** (`-Wall -Wextra -Wmissing-prototypes
-Wstrict-prototypes`, Apple clang 16) produces **zero** `conflicting types`,
`incompatible pointer types`, `implicit declaration` or `redefinition`
diagnostics. The complete diagnostic census is 26 warnings in three classes:

| count | class | note |
|---|---|---|
| 23 | `-Wunused-variable` | 22 in generated handlers, 1 in `m68kcpu.h:1869` |
| 3 | `-Wmissing-prototypes` | see the two asymmetries below, plus `m68ki_disassemble_quick` (`m68kdasm.c:3775`), an internal helper that should be `static` |

`-Wmissing-prototypes` firing only three times is the positive result: it means
every other externally-visible function defined in the core has a matching
visible prototype from the headers. Cross-checking the ABI the other way, the
only `m68k_*` symbols `libmusashi.a` leaves unresolved are the six bus callbacks
(`m68k_read_memory_8/16/32`, `m68k_write_memory_8/16/32`) and two disassembler
reads (`m68k_read_disassembler_16/32`) — all of which `ad-m68k` defines in
`src/lib.rs` (lines 548–573), along with three aliases
(`m68k_read_immediate_16/32`, `m68k_read_disassembler_8`) kept deliberately
against a future `M68K_SEPARATE_READS` change. Nothing is missing and nothing is
stray.

Two harmless declaration asymmetries do exist, and they are upstream's, not a
mixed-tree symptom:

1. `m68kcpu.c:751` and `:756` define `m68k_set_cmpild_instr_callback` and
   `m68k_set_rte_instr_callback`, which neither `m68k.h` nor `m68kcpu.h`
   declares. The feature is otherwise fully wired — the callback slots are in
   `m68kcpu.h:1008-1009` and the dispatch macros at `m68kcpu.h:508-513` — so this
   is a missing public declaration, not a version skew. The Rust FFI does not use
   either callback.
2. `m68k.h:375` declares `m68k_state_register` unconditionally, but
   `m68kcpu.c:1210` defines it only inside `#if M68K_COMPILE_FOR_MAME ==
   M68K_OPT_ON` (`m68kcpu.c:1187-1234`), and `m68kconf.h:55` sets that switch
   **off**. Calling it from this build would be a link error, not a miscompile.
   It is MAME savestate plumbing (`state_save_register_item*`) and nothing here
   calls it.

### What it is *not* — no pinnable upstream release

The tree cannot be pinned to a pristine upstream release from its contents, and
this is not a case of "we lost the URL":

- `m68kfpu.c` has **no banner and no copyright header at all**, and carries
  eighteen third-party edit markers initialled "JFF" — bug fixes and added
  addressing modes (`m68kfpu.c:11`, `:673`, `:708`, `:1097`, `:1354`, `:1382`,
  `:1391`, `:1399`, `:1427`, `:1432`, `:1447`, `:1464`, `:1478`, `:1652`, `:1674`,
  `:1754`, `:1785`, `:1865`), including one that changes cycle accounting with
  the comment "unsure of the number of cycles!!". This is a fork's working copy,
  not a release artefact.
- `m68kmmu.h` and `softfloat/` are MAME-lineage imports carrying **different
  copyright notices** from the MIT text in `LICENSE-musashi.txt`: `m68kmmu.h`
  says "Copyright Nicola Salmoria and the MAME Team. Visit http://mamedev.org for
  licensing and usage restrictions", and `softfloat/README.txt` carries John R.
  Hauser's own SoftFloat 2b notice. The vendored `LICENSE-musashi.txt` covers
  Karl Stenerud's code only. Anyone auditing this repo's licensing needs to read
  all three, and the crate's `license = "MIT OR Apache-2.0"` describes the Rust
  code, not the vendored C.

**Best determination:** a 4.x Musashi, most plausibly around the **4.60** mark —
that is the newest self-consistent banner and the version the generator prints —
carrying MAME's PMMU and SoftFloat, plus uncredited third-party FPU fixes. **The
exact upstream release is indeterminate and should not be asserted.** It cannot
be recovered from the tree, because the local FPU edits mean no upstream release
will hash-match anyway. Pinning it properly requires an external act: diff this
tree against candidate upstreams and record the base commit here. Until then,
the hashes in §Manifest *are* the version — they identify these bytes exactly,
which is what the build actually depends on.

## Configuration

`m68kconf.h` is the one file here intended to be edited, and it enables
`M68K_EMULATE_010`, `_EC020`, `_020`, `_030`, `_040` and `_PMMU` (lines 67–84,
254) even though the host currently selects `M68K_CPU_TYPE_68000`. **Leave them
on.** A Quadra machine profile is a 68040, and `M68K_EMULATE_040` also gates the
`m68kmmu.h` paths; switching them off now buys a smaller binary and costs the
profile work a revert.
