//! Macintosh Toolbox high-level emulation.
//!
//! Traps are implemented **on demand**, discovered by running real modules. That
//! is not laziness — the trap surface cannot be enumerated
//! statically (see `docs/LEARNINGS.md`): a naive scan of the module binaries reports 653 candidate A-line
//! words (inflated by sprite data that happens to look like opcodes), while
//! recursive-descent disassembly reaches under 10% of the code because almost
//! everything is called through host callbacks and jump tables.
//!
//! So the dispatcher's most important property is that **an unimplemented trap
//! stops the run and says exactly what it was**. A trap that silently returned 0
//! would produce a subtly wrong render, which is far more expensive to debug than
//! a hard failure.
//!
//! # Pascal calling convention
//!
//! Toolbox traps take arguments on the stack, pushed left to right, and leave any
//! result there too; the trap pops its own arguments. OS traps take arguments in
//! registers (usually `A0`/`D0`) and return a result code in `D0`. [`Stack`]
//! handles the former.

use std::collections::BTreeMap;

use ad_m68k::{Bus, Registers, TrapError};
use ad_memory::{globals, Memory};

/// Diagnostic switches, passed in rather than read from the environment.
///
/// Every one of these used to be a `std::env::var` call inside a trap handler.
/// That is convenient in a lab and wrong in a library: this code will be loaded
/// into a screen-saver host process, where an environment variable set for some
/// unrelated reason must not change how a module renders, and where a caller has
/// no way to ask for logging without mutating global state. `Default` is all
/// off. `ad_runtime::RuntimeOptions::from_env` is the one place that maps
/// environment variables onto this.
///
/// `qd_log` in particular exists because "the module drew and the screen stayed
/// empty" is a recurring failure class here, and the answer is always in *where*
/// the drawing landed rather than *whether* it happened.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Diagnostics {
    /// Trace QuickDraw and Resource Manager activity: current port, resolved
    /// surface, and each primitive's rect.
    pub qd_log: bool,
    /// Log the first write to screen memory from each distinct PC.
    pub watch_screen: bool,
    /// Log writes into an address range, as `(from, to)` exclusive.
    pub watch_addr: Option<(u32, u32)>,
    /// Log what the instruction at this PC stores, and where.
    pub watch_pc: Option<u32>,
    /// Trace the first event-path callback the host delivers.
    pub trace_event: bool,
}

impl Diagnostics {
    /// Whether any per-write watch is armed. The store path branches on this
    /// once instead of entering two watch functions per emulated write.
    #[must_use]
    pub fn watching(&self) -> bool {
        self.watch_screen || self.watch_addr.is_some() || self.watch_pc.is_some()
    }
}

pub mod blit;
pub mod fonts;
pub mod pict;
pub mod port;
pub mod profile;
pub mod quickdraw;
pub mod resources;
pub mod sane;
pub mod snd;
pub mod random;
pub mod traps;

pub use blit::Surface;
pub use port::Screen;
pub use quickdraw::{Framebuffer, QuickDraw, Rect};
pub use resources::ResourceStore;
pub use traps::{Family, Trap};

/// The After Dark presence handshake.
///
/// `AFTERDARKEXISTS` — the glue linked into 43 of the 66 modules —
/// does not use `Gestalt`. After Dark **patched `_GetOSEvent`**, and the module
/// detects it by handshake:
///
/// 1. The module builds a 16-byte `EventRecord` on its stack and stores the magic
///    [`MAGIC_REQUEST`] in the `message` field (offset 2).
/// 2. It calls `GetOSEvent` with an event mask of **0** — a real Mac returns no
///    event and leaves the record untouched.
/// 3. After Dark's patch spots the cookie, replaces `message` with
///    [`MAGIC_REPLY`], and stores a pointer to its own info record in the `where`
///    field (offset 10).
/// 4. The module reads a version word at **+12** of that record and compares it
///    against the minimum it needs.
///
/// Without this, 38 modules refused to start with "This module requires After
/// Dark 2.0" — a message that looks nothing like a missing trap.
pub mod ad_detect {
    /// `'aYmm'` — the cookie a module writes into `EventRecord.message`.
    pub const MAGIC_REQUEST: u32 = 0x6159_6D6D;
    /// `'ADrk'` — what After Dark writes back. Modules compare only the top three
    /// bytes (`& 0xFFFFFF00` against `'ADr\0'`), so the last byte is free.
    pub const MAGIC_REPLY: u32 = 0x4144_726B;
    /// Offset of `message` within an `EventRecord`.
    pub const EVT_MESSAGE: u32 = 2;
    /// Offset of `where` within an `EventRecord`.
    pub const EVT_WHERE: u32 = 10;
    /// Offset of the version word within After Dark's info record.
    pub const INFO_VERSION: u32 = 12;
    /// BCD version we report. The control panel on the source disk is 2.0x.
    ///
    /// `AD_VERSION` in the environment overrides it. There are **two** places a
    /// module can read the version — this info record and `params->adVersion` —
    /// and a 3.0-era module that finds 2.0 in either one declines, so probing
    /// what they need means moving both together.
    pub const AD_VERSION: u16 = 0x0200;

    /// The version to report, honouring the `AD_VERSION` override.
    #[must_use]
    pub fn ad_version() -> u16 {
        std::env::var("AD_VERSION")
            .ok()
            .and_then(|v| u16::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok())
            .unwrap_or(AD_VERSION)
    }
    /// Size of the info record handed back. Generous and zeroed, so a module
    /// reading a field we have not identified sees 0 rather than garbage.
    pub const INFO_SIZE: u32 = 256;
}

/// Host-callout gate.
///
/// After Dark's ABI is not only data: the info record and the extension table
/// carry **function pointers the module JSRs through** (see `FINDMEMORY`'s
/// `movea.l $6(a3),a0 ; jsr (a0)` — see `docs/LEARNINGS.md`). The host therefore needs
/// addresses that are *callable from 68K code* but implemented in Rust.
///
/// Each slot is four bytes of emulated memory: an A-line word `$AB<slot>`
/// followed by `RTS`. Calling the slot raises the A-line exception, the trap
/// gate dispatches here, the handler runs in Rust, and the resumed `RTS`
/// returns to the module. Unknown slots hard-fail with the slot's registered
/// provenance — the same discovery discipline as unimplemented traps.
pub mod callout {
    /// First A-line word reserved for callouts.
    pub const WORD_BASE: u16 = 0xAB00;
    /// Number of available slots.
    pub const SLOTS: u16 = 0x80;
    /// Bytes per slot: the A-line word plus an RTS.
    pub const SLOT_BYTES: u32 = 4;
}

/// What a host-callout slot does when a module calls it.
///
/// The method signatures and stack conventions come from disassembling the
/// After Dark glue in the modules themselves (see `docs/LEARNINGS.md`): all are Pascal —
/// arguments pushed left to right, **callee pops**, result in a caller-reserved
/// stack slot. Getting the pop wrong corrupts the caller's control flow far
/// from the call site, which is how the first guessed implementation sent
/// modules off executing their own stacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Callout {
    /// Unimplemented: hard-fail with provenance and live stack args.
    Discover,
    /// `'ADmf'+2`: `FindMemory(params, size) : Handle`
    MemFind,
    /// `'ADmf'+6`: `ReleaseMemory(h)`
    MemRelease,
    /// `'ADmf'+10`: `UseMemory(h)`
    MemUse,
    /// `'ADmf'+14`: `RestoreMemory(h)`
    MemRestore,
    /// `'ADSd'+2`: `OpenSound(params) : SoundInfoHandle`
    SndOpen,
    /// `'ADSd'+6`: `CloseSound(info, chan)`
    SndClose,
    /// `'ADSd'+10`: `PlaySound(info, chan, snd)`
    SndPlay,
    /// `'ADSd'+14`: `QuietSound(info, chan)` — stop the playing sound.
    SndQuiet,
    /// `'ADSd'+18`: `FlushSound(info, chan)` — drop queued commands.
    SndFlush,
    /// `'ADSd'+22`: `GetSoundLength(info, snd) : long`
    SndLength,
    /// `'ADSd'+26`: `SoundBusy(info, chan) : BOOLEAN`
    ///
    /// The slot order comes from the SDK's `Sounds.h` declaration order, which
    /// the four glue-disassembled anchors (+2, +6, +10, +22) confirm.
    SndBusy,
    /// A **68000 exception vector**: report which exception, and where.
    ///
    /// Every unpopulated vector on a 68000 is address 0, so any exception a
    /// module took presented as "wild jump: executing low memory at 0x000000" —
    /// the single most misleading error in this runtime. ProtoToasters looked
    /// like a nil function pointer for as long as that was the message; it is
    /// actually `divs.w D6,D0` with `D6` zero.
    ///
    /// `short_frame` distinguishes the 6-byte group-1/2 frame (`SR`, then `PC`)
    /// from the long frame a 68000 pushes for a bus or address error, whose
    /// layout is different enough that guessing a PC out of it would be worse
    /// than naming the exception alone.
    CpuException { short_frame: bool },
}

/// Number of entries in the synthetic trap-address table (all canonical words).
const TRAP_TABLE_ENTRIES: u32 = 0x1000;
/// Staging size for decoded `PICT` bitmaps: one full-screen 8-bit frame.
const PICT_SCRATCH_SIZE: u32 = 640 * 480;
/// Size of the `SndChannel` record handed to modules.
const SND_CHANNEL_SIZE: u32 = 1024;

/// Answer a `Gestalt` selector, or `None` for "I do not know this one".
///
/// Returning an error for unknown selectors is deliberate: inventing a value is
/// how a module ends up taking a code path for hardware that is not there.
fn gestalt(selector: u32, p: &profile::MachineProfile) -> Option<u32> {
    Some(match &selector.to_be_bytes() {
        b"sysv" => u32::from(p.system_version),
        // 0x0200 is 32-bit Color QuickDraw; 0x0100 would be the 8-bit original.
        b"qd  " => {
            if p.color_quickdraw {
                0x0200
            } else {
                0
            }
        }
        b"snd " => p.sound.gestalt_bits(),
        b"proc" => p.cpu.gestalt_processor(),
        b"fpu " => u32::from(p.fpu),
        // gestaltMMUType: 0 = none, 1 = 68851, 2 = 68030 PMMU, 3 = 68040.
        b"mmu " => u32::from(p.mmu) * 2,
        b"ram " => p.ram_bytes,
        b"vm  " => 0, // no virtual memory
        b"a/ux" => 0,
        _ => return None,
    })
}

/// Classic Mac `OSErr` values this runtime returns.
pub mod oserr {
    pub const NO_ERR: i16 = 0;
    pub const MEM_FULL_ERR: i16 = -108;
    pub const NIL_HANDLE_ERR: i16 = -109;
    pub const RES_NOT_FOUND: i16 = -192;
    /// `vTypErr`: the queue element handed to `_VInstall` isn't a VBL task.
    pub const V_TYP_ERR: i16 = -2;
    /// `memWZErr`: attempt to operate on a free or nonexistent block.
    pub const MEM_WZ_ERR: i16 = -111;
    /// `nsvErr`: no such volume. What every File Manager call gets here, since
    /// this runtime mounts none.
    pub const NSV_ERR: i16 = -35;
    /// `ioErr`: a durable write was attempted and failed. Never returned for a
    /// host with no save location configured — that is `noErr` and no write.
    pub const IO_ERR: i16 = -36;
}

/// File Manager parameter-block offsets and `_HFSDispatch` selectors.
///
/// `ioResult` sits at the same place in every variant of the block, which is why
/// one constant serves `HParamBlockRec` and `FCBPBRec` alike.
pub mod filemgr {
    pub const IO_RESULT: u32 = 16;
    /// `PBGetFCBInfo`.
    pub const SEL_GET_FCB_INFO: u16 = 8;
    /// `PBGetCatInfo`.
    pub const SEL_GET_CAT_INFO: u16 = 9;
}

/// `VBLTask` field offsets: `qLink(4) qType(2) vblAddr(4) vblCount(2) vblPhase(2)`.
pub mod vbl {
    /// Address of the task's handler routine.
    pub const VBL_ADDR: u32 = 6;
    /// Ticks until the handler runs; the handler re-arms it or goes dormant.
    pub const VBL_COUNT: u32 = 10;
}

/// `CntrlParam` field offsets, shared by every Device Manager parameter block:
/// `qLink(4) qType(2) ioTrap(2) ioCmdAddr(4) ioCompletion(4) ioResult(2)
/// ioNamePtr(4) ioVRefNum(2) ioCRefNum(2) csCode(2) csParam[11]`.
pub mod cntrl {
    pub const IO_RESULT: u32 = 16;
    pub const IO_CREF_NUM: u32 = 24;
    pub const CS_CODE: u32 = 26;
    pub const CS_PARAM: u32 = 28;
    /// Video driver control code for "load these colour table entries".
    pub const CSC_SET_ENTRIES: i16 = 3;
}

/// `MenuInfo` header size: `menuID(2) menuWidth(2) menuHeight(2) menuProc(4)
/// enableFlags(4)`. `menuData` follows, starting with the Str255 title.
pub const MENU_HEADER: u32 = 14;
/// Bytes of attributes after each item's text: icon, key, mark, style.
pub const MENU_ITEM_ATTRS: u32 = 4;
/// Index of the style byte within those attributes.
pub const MENU_ITEM_STYLE: u32 = 3;

/// Address and length of each item's text in a menu, in order.
///
/// Walking stops at the zero length byte that terminates `menuData`, and is
/// bounded by the handle's own size so a malformed menu cannot run away.
fn menu_items(mem: &mut Memory, menu: u32) -> Vec<(u32, u8)> {
    let mut out = Vec::new();
    let (Some(block), Some(size)) = (mem.deref_handle(menu), mem.handle_size(menu)) else {
        return out;
    };
    let end = block.wrapping_add(size);
    let title_len = u32::from(mem.read_u8(block.wrapping_add(MENU_HEADER)));
    let mut p = block
        .wrapping_add(MENU_HEADER)
        .wrapping_add(1)
        .wrapping_add(title_len);
    while p < end {
        let len = mem.read_u8(p);
        if len == 0 {
            break;
        }
        out.push((p, len));
        p = p
            .wrapping_add(1)
            .wrapping_add(u32::from(len))
            .wrapping_add(MENU_ITEM_ATTRS);
    }
    out
}

/// A record of one serviced trap, for the compatibility lab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrapCall {
    pub word: u16,
    pub name: Option<&'static str>,
    pub pc: u32,
}

/// Counts and ordering of every trap a run made.
#[derive(Debug, Default)]
pub struct TrapLog {
    /// How many times each canonical trap was called.
    pub counts: BTreeMap<u16, u32>,
    /// The first `history_cap` calls, in order.
    pub history: Vec<TrapCall>,
    history_cap: usize,
    /// Traps that were asked for but are not implemented.
    pub unimplemented: BTreeMap<u16, u32>,
}

impl TrapLog {
    #[must_use]
    pub fn new() -> Self {
        Self {
            history_cap: 4096,
            ..Default::default()
        }
    }

    fn record(&mut self, t: &Trap, pc: u32) {
        let c = self.counts.entry(t.canonical()).or_insert(0);
        *c = c.saturating_add(1);
        if self.history.len() < self.history_cap {
            self.history.push(TrapCall {
                word: t.word,
                name: t.name(),
                pc,
            });
        }
    }

    fn record_missing(&mut self, t: &Trap) {
        let c = self.unimplemented.entry(t.canonical()).or_insert(0);
        *c = c.saturating_add(1);
    }

    /// Total trap calls made.
    #[must_use]
    pub fn summary_len(&self) -> u32 {
        self.counts.values().sum()
    }

    /// Distinct traps used, for the per-module compatibility report.
    #[must_use]
    pub fn distinct(&self) -> usize {
        self.counts.len()
    }

    /// A human-readable summary, most-called first.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut v: Vec<(&u16, &u32)> = self.counts.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        v.iter()
            .map(|(w, n)| {
                let name = traps::name_of(**w).unwrap_or("?");
                format!("_{name}(${w:04X})x{n}")
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Stack access for Pascal-convention Toolbox traps.
///
/// Arguments were pushed left to right before the trap, so the *last* argument
/// is nearest the stack pointer. `pop_*` walks outward from there.
///
/// # Where the result goes
///
/// For a Pascal **function**, the caller reserves the result slot *before*
/// pushing arguments and pops it afterwards:
///
/// ```text
/// CLR.W   -(SP)        ; caller reserves 2 bytes for the result
/// _Random
/// MOVE.W  (SP)+,D3     ; caller pops it
/// ```
///
/// So the routine writes into that existing slot and pops only its arguments —
/// it must **not** push a second slot. Doing so leaks bytes on every call and
/// the stack walks downward until the module hangs, which is exactly what it
/// looks like: a module that runs and then never returns.
#[allow(missing_debug_implementations, reason = "holds a &mut dyn Registers")]
pub struct Stack<'a> {
    regs: &'a mut dyn Registers,
    /// Bytes consumed so far.
    offset: u32,
}

impl<'a> Stack<'a> {
    fn new(regs: &'a mut dyn Registers) -> Self {
        Self { regs, offset: 0 }
    }

    /// Pop a 16-bit argument.
    pub fn pop_u16(&mut self, mem: &mut Memory) -> u16 {
        let v = mem.read_u16(self.regs.sp().wrapping_add(self.offset));
        self.offset = self.offset.wrapping_add(2);
        v
    }

    /// Pop a 16-bit signed argument.
    pub fn pop_i16(&mut self, mem: &mut Memory) -> i16 {
        self.pop_u16(mem) as i16
    }

    /// Pop a 32-bit argument (pointer, handle or long).
    pub fn pop_u32(&mut self, mem: &mut Memory) -> u32 {
        let v = mem.read_u32(self.regs.sp().wrapping_add(self.offset));
        self.offset = self.offset.wrapping_add(4);
        v
    }

    /// Discard `n` bytes of arguments without reading them.
    pub fn skip(&mut self, n: u32) {
        self.offset = self.offset.wrapping_add(n);
    }

    /// Remove the argument block, leaving no result.
    pub fn finish(self) {
        let sp = self.regs.sp().wrapping_add(self.offset);
        self.regs.set_sp(sp);
    }

    /// Pop the arguments and write a 16-bit result into the caller's slot.
    pub fn finish_u16(self, mem: &mut Memory, value: u16) {
        let sp = self.regs.sp().wrapping_add(self.offset);
        mem.write_u16(sp, value);
        self.regs.set_sp(sp);
    }

    /// Pop the arguments and write a Pascal `BOOLEAN` into the caller's slot.
    ///
    /// A `BOOLEAN` result occupies two bytes on the stack but lives in the
    /// **high-order** byte, because the caller reads it with `MOVE.B (A7)+,Dn` —
    /// and on the 68000 a byte access through `(A7)+` reads the even address and
    /// still advances the stack by two.
    ///
    /// Writing the value in the low byte instead makes every predicate read as
    /// `false`. Lunatic Fringe declined with "This module does not run under Demo
    /// mode" purely because `EmptyRect` looked like it had said "not empty".
    pub fn finish_bool(self, mem: &mut Memory, value: bool) {
        let sp = self.regs.sp().wrapping_add(self.offset);
        mem.write_u16(sp, u16::from(value) << 8);
        self.regs.set_sp(sp);
    }

    /// Pop the arguments and write a 32-bit result into the caller's slot.
    pub fn finish_u32(self, mem: &mut Memory, value: u32) {
        let sp = self.regs.sp().wrapping_add(self.offset);
        mem.write_u32(sp, value);
        self.regs.set_sp(sp);
    }
}

/// The Toolbox: memory, QuickDraw state, and the trap dispatcher.
///
/// Implements [`Bus`], so it *is* the machine the CPU runs against.
#[derive(Debug)]
pub struct Toolbox {
    pub mem: Memory,
    pub qd: QuickDraw,
    pub log: TrapLog,
    /// The screen's port, pixel map and graphics device. Modules read screen
    /// bounds and depth out of these before drawing anything.
    pub screen: Screen,
    /// The current `GrafPort`, as `_SetPort` / `_GetPort` see it.
    pub cur_port: u32,
    /// Current `ctSeed` of the screen's `ColorTable`; bumped on every `SetEntries`.
    ct_seed: u32,
    /// The module's own resources, for the Resource Manager traps.
    pub resources: ResourceStore,
    /// Where `_WriteResource` and `_UpdateResFile` put bytes that must survive
    /// the process. `None` in the lab and in tests: nothing is written, and a
    /// module must not fail because the host gave it nowhere to save.
    pub sink: Option<Box<dyn resources::ResourceSink>>,
    /// After Dark's info record, handed back by the `_GetOSEvent` handshake.
    pub ad_info: u32,
    /// Staging area for decoded `PICT` bitmaps, outside the module's heap.
    pub pict_scratch: u32,
    /// A stand-in heap zone header, for `_GetZone` and friends.
    pub zone: u32,
    /// Base address of the host-callout slots in emulated memory.
    pub callout_base: u32,
    /// What each callout slot stands for, by index — provenance for diagnostics.
    pub callout_names: Vec<String>,
    /// The behaviour of each slot.
    pub callout_kinds: Vec<Callout>,
    /// Incremented every frame; drives `Ticks` and `TickCount`.
    pub ticks: u32,
    /// Cursor position in global coordinates, as `(h, v)`.
    ///
    /// `_GetMouse` and every `EventRecord.where` read this. Headless runs need a
    /// *fixed* value or determinism goes, and it must not be `(0, 0)`: a corner
    /// is a position modules treat specially (several avoid drawing under the
    /// cursor), so the screen centre is the least loaded choice. `ad-player`
    /// overwrites it from the real window each frame.
    pub mouse: (i16, i16),
    /// The machine every capability answer derives from. See [`profile`].
    pub profile: profile::MachineProfile,
    /// Current `_SwapMMUMode` addressing mode.
    mmu_mode: u8,
    /// Trap patches installed by the module via `_SetTrapAddress`, keyed by
    /// canonical trap word. The host calls these to deliver interrupt-driven
    /// events (keyboard) to modules that hook the event path.
    pub trap_patches: BTreeMap<u16, u32>,
    /// Base of the trap-address table `_GetTrapAddress` reports from.
    ///
    /// Every entry is a real `RTS` in emulated memory: modules compare these
    /// addresses to feature-detect, but a module that *patches* a trap also
    /// **jumps to the old vector** to chain — Lunatic Fringe's `_PostEvent`
    /// hook ends that way — so the reported address must be executable and
    /// harmless, not merely distinct.
    trap_table_base: u32,
    /// Diagnostic switches. The host sets these; nothing here reads the
    /// environment. `watch_pc` complements `watch_addr` for the common case
    /// where the *code* is known from a disassembly but the runtime address of
    /// the variable is not — the logged address reveals the register base.
    pub diag: Diagnostics,
    /// Distinct PCs seen writing to the screen, so each logs once.
    screen_writers: std::collections::BTreeSet<u32>,
    /// Sounds played, decoded once and cached. See [`snd::SoundBank`].
    ///
    /// A host's audio layer drains what is new; the lab reads the whole log as
    /// evidence.
    pub sounds: snd::SoundBank,
    /// Installed Vertical Retrace Manager tasks (`VBLTask` addresses).
    ///
    /// Games use VBL tasks as their heartbeat: Lunatic Fringe installs one and
    /// then paces every gameplay frame off it. Accepting `_VInstall` without
    /// ever running the task leaves such a module spinning forever on a flag
    /// its own interrupt handler was supposed to set.
    pub vbl_tasks: Vec<u32>,
    /// Tasks whose `vblCount` just reached zero; the host runs and clears
    /// these. Split from `vbl_tasks` because firing needs a valid CPU context,
    /// which the Toolbox doesn't own — ticks may advance between calls too.
    pub vbl_due: Vec<u32>,
}

impl Default for Toolbox {
    fn default() -> Self {
        Self::new()
    }
}

impl Toolbox {
    #[must_use]
    pub fn new() -> Self {
        let mut mem = Memory::new();
        let qd = QuickDraw::new(&mut mem);
        // After Dark handed modules a ready-made full-screen port; reproduce that
        // rather than making every module call OpenPort itself.
        //
        // The port's visRgn and clipRgn are its **own** regions, not the module's
        // `blankRgn`. They used to be the same handle, which was a landmine: the
        // only reason nothing broke is that `_SetClip` and `_ClipRect` were
        // no-ops, and the moment either wrote through, every module's opening
        // `if (EmptyRgn(blankRgn)) return;` would have started lying. Flying
        // Toasters' `DoDrawFrame` is exactly that shape, so the failure would have
        // been silent and fleet-wide.
        let vis = QuickDraw::full_screen_region(&mut mem);
        let clip = QuickDraw::full_screen_region(&mut mem);
        let screen = Screen::build(&mut mem, qd.screen_base, vis, clip, &qd.fb.palette);
        let zone = mem.reserve_host(64, "heap zone header");
        // Callout slots: A-line word then RTS, one per slot.
        let callout_base =
            mem.reserve_host(callout::SLOTS as u32 * callout::SLOT_BYTES, "callout slots");
        for i in 0..callout::SLOTS {
            let at = callout_base + u32::from(i) * callout::SLOT_BYTES;
            mem.write_u16(at, callout::WORD_BASE | i);
            mem.write_u16(at.wrapping_add(2), 0x4E75); // RTS
        }
        // Trap-address table: one RTS per canonical trap word, plus a final
        // shared slot for "unimplemented". See the field docs.
        let trap_table_base =
            mem.reserve_host((TRAP_TABLE_ENTRIES + 1) * 2, "trap address table");
        for i in 0..=TRAP_TABLE_ENTRIES {
            mem.write_u16(trap_table_base.wrapping_add(i * 2), 0x4E75); // RTS
        }
        // A full-screen 8-bit bitmap is the largest a picture can reasonably
        // stage; sized once so decoding never allocates mid-frame.
        // `reserve_host` carries the assertion this used to make by hand, and
        // names the fixture in the panic instead of relying on a nearby comment.
        let pict_scratch = mem.reserve_host(PICT_SCRATCH_SIZE, "PICT staging buffer");
        let ad_info = mem.reserve_host(ad_detect::INFO_SIZE, "After Dark info record");
        for i in 0..ad_detect::INFO_SIZE {
            mem.write_u8(ad_info.wrapping_add(i), 0);
        }
        mem.write_u16(
            ad_info.wrapping_add(ad_detect::INFO_VERSION),
            ad_detect::ad_version(),
        );
        let mut me = Self {
            pict_scratch,
            zone,
            callout_base,
            callout_names: Vec::new(),
            callout_kinds: Vec::new(),
            ad_info,
            mem,
            qd,
            log: TrapLog::new(),
            cur_port: screen.port,
            screen,
            ct_seed: port::INITIAL_CT_SEED,
            resources: ResourceStore::default(),
            sink: None,
            ticks: 0,
            mouse: (
                (quickdraw::SCREEN_WIDTH / 2) as i16,
                (quickdraw::SCREEN_HEIGHT / 2) as i16,
            ),
            profile: profile::MachineProfile::honest(),
            // 24-bit addressing, matching `MachineProfile::honest()`.
            mmu_mode: 0,
            trap_patches: BTreeMap::new(),
            trap_table_base,
            sounds: snd::SoundBank::default(),
            diag: Diagnostics::default(),
            screen_writers: std::collections::BTreeSet::new(),
            vbl_tasks: Vec::new(),
            vbl_due: Vec::new(),
        };
        me.set_port(me.screen.port);
        me
    }

    /// Make `port` current, for both the Toolbox and QuickDraw.
    ///
    /// Kept in one place because drawing resolves its destination from
    /// `qd.cur_port`: if the two drift apart, output silently goes to the wrong
    /// surface.
    pub fn set_port(&mut self, port: u32) {
        // Colours are per-port state on a real Mac, and a module that composes
        // offscreen depends on it: Logo opens its own CGrafPort — which InitPort
        // gives standard black-ink-on-white — draws the After Dark logo there,
        // and blits to the screen. With one global pair, the offscreen port
        // inherited the *screen* port's black-on-black and the logo was drawn
        // in invisible ink before the blit ever ran.
        self.qd.switch_port_colours(self.cur_port, port);
        self.cur_port = port;
        self.qd.cur_port = port;
        if self.diag.qd_log {
            let bits = port.wrapping_add(port::port::PORT_BITS);
            match blit::Surface::resolve(&mut self.mem, bits) {
                Some(s) => eprintln!(
                    "[qd] SetPort {port:#x} -> base={:#x} rb={} bounds={:?} depth={}",
                    s.base, s.row_bytes, s.bounds, s.pixel_size
                ),
                None => eprintln!("[qd] SetPort {port:#x} -> unresolvable bits"),
            }
        }
    }

    /// Reserve a callout slot and return its callable address.
    ///
    /// `name` is the slot's provenance — e.g. `"info+16"` — and is what the
    /// hard failure reports when a module calls a slot nothing implements yet.
    pub fn callout_slot(&mut self, name: &str) -> u32 {
        self.callout_slot_kind(name, Callout::Discover)
    }

    /// Reserve a callout slot with an implementation.
    pub fn callout_slot_kind(&mut self, name: &str, kind: Callout) -> u32 {
        let idx = self.callout_names.len();
        self.callout_names.push(name.to_owned());
        self.callout_kinds.push(kind);
        self.callout_base + (idx as u32) * callout::SLOT_BYTES
    }

    /// Complete a Pascal-convention callout: pop `arg_bytes` of arguments,
    /// write any 32-bit result into the caller's reserved slot, and leave SP
    /// so the slot's `RTS` returns cleanly.
    ///
    /// Stack at handler time: `SP → [ret][args…][result slot]`. Callee-pops
    /// means the return address moves up over the arguments.
    fn pascal_callout_return(
        &mut self,
        regs: &mut dyn Registers,
        arg_bytes: u32,
        result: Option<u32>,
    ) {
        let sp = regs.sp();
        let ret = self.mem.read_u32(sp);
        let new_sp = sp.wrapping_add(arg_bytes);
        self.mem.write_u32(new_sp, ret);
        if let Some(v) = result {
            self.mem.write_u32(new_sp.wrapping_add(4), v);
        }
        regs.set_sp(new_sp);
    }

    /// Like [`Self::pascal_callout_return`], for a `BOOLEAN` result.
    ///
    /// The caller reserved **two** bytes, not four, and reads the value from
    /// the high byte (`MOVE.B (A7)+`). Writing a long here would trample the
    /// caller's own stack data two bytes past the slot.
    fn pascal_callout_return_bool(
        &mut self,
        regs: &mut dyn Registers,
        arg_bytes: u32,
        value: bool,
    ) {
        let sp = regs.sp();
        let ret = self.mem.read_u32(sp);
        let new_sp = sp.wrapping_add(arg_bytes);
        self.mem.write_u32(new_sp, ret);
        self.mem
            .write_u16(new_sp.wrapping_add(4), u16::from(value) << 8);
        regs.set_sp(new_sp);
    }

    /// Fill a `QDGlobals` record the way `_InitGraf` does.
    pub fn init_graf(&mut self, global_ptr: u32) {
        let seed = globals::LowMem::rnd_seed(&mut self.mem);
        let base = self.qd.screen_base;
        let screen = self.screen;
        screen.init_graf(&mut self.mem, global_ptr, base, seed);
    }

    /// Blank the display to black, as After Dark did on taking over the screen.
    ///
    /// Modules are sent a `Blank` message and many do paint the screen themselves,
    /// but several assume the host has already darkened it — and a sprite blitted
    /// with `srcCopy` carries its own black background, which is invisible against
    /// black and an ugly box against white.
    pub fn blank_screen(&mut self) {
        let base = self.qd.screen_base;
        self.qd.fb.clear(&mut self.mem, base, quickdraw::BLACK_INDEX);
    }

    /// Refresh the framebuffer cache from emulated screen memory.
    ///
    /// Must be called after any module code runs: modules may write straight to
    /// the screen bitmap without issuing a single drawing trap.
    pub fn sync_screen(&mut self) {
        let base = self.qd.screen_base;
        self.qd.fb.sync_from(&mut self.mem, base);
    }

    /// Advance the emulated clock by one tick (1/60 s).
    /// Decode the `snd ` resource behind `handle`, naming it when possible.
    fn decode_snd_handle(&mut self, handle: u32) -> Option<(String, snd::DecodedSound)> {
        let block = self.mem.deref_handle(handle)?;
        let size = self.mem.handle_size(handle)?;
        let bytes = self.mem.read_bytes(block, size.min(1 << 20) as usize);
        let sound = snd::decode(&bytes).ok()?;
        let name = self.snd_name(handle);
        Some((name, sound))
    }

    /// The `snd ` resource's own name, when the handle is a live resource.
    fn snd_name(&mut self, handle: u32) -> String {
        self.resources
            .info_for(handle)
            .and_then(|(e, _, _)| e.name.clone())
            .unwrap_or_else(|| format!("snd@{handle:#x}"))
    }

    /// Decode and queue one played sound; silently ignores non-sounds.
    ///
    /// `channel` is the `SndChannel` the module played on. The Sound Manager
    /// plays one sound at a time per channel, so an output device needs it to
    /// know whether a new effect replaces a playing one or mixes with it.
    fn play_snd_handle(&mut self, handle: u32, channel: u32) {
        let Some(block) = self.mem.deref_handle(handle) else {
            return;
        };
        let Some(size) = self.mem.handle_size(handle) else {
            return;
        };
        let bytes = self.mem.read_bytes(block, size.min(1 << 20) as usize);
        let name = self.snd_name(handle);
        let tick = self.ticks;
        let log = self.diag.qd_log;
        if let Some(sound) = self.sounds.play(name.clone(), channel, tick, &bytes) {
            if log {
                eprintln!(
                    "[snd] play {:?}: {} samples @ {} Hz ({} ticks)",
                    name,
                    sound.samples.len(),
                    sound.rate_hz,
                    sound.ticks()
                );
            }
        }
    }

    /// `AD_WATCH_ADDR=<hex>[+<len>]`: log every write into that byte range,
    /// with the PC that made it.
    ///
    /// This is how a module's own state variables get read out. A module's
    /// globals are `A4`-relative, so the workflow is: disassemble to find the
    /// offset, log the code resource's block address (see the `[res]` line),
    /// add, and watch. It found Lunatic Fringe's key-pressed array.
    fn watch_addr_write(&mut self, addr: u32, len: u32, value: u32) {
        if let Some(want) = self.diag.watch_pc {
            let pc = ad_m68k::current_pc();
            if pc == want {
                eprintln!("[watch] PC {pc:#x} writes {len} bytes at {addr:#x}");
            }
        }
        let Some((from, to)) = self.diag.watch_addr else {
            return;
        };
        if addr.wrapping_add(len) <= from || addr >= to {
            return;
        }
        // `current_pc` is the NEXT instruction: Musashi advances PC before the
        // operand write. Subtracting is not safe (variable instruction length),
        // so the logged PC reads a little past the storing instruction.
        let pc = ad_m68k::current_pc();
        eprintln!("[watch] {addr:#x} = {value:#x} ({len}b) pc<{pc:#x}");
    }

    /// `AD_WATCH_SCREEN=1`: log the PC of each distinct instruction that writes
    /// into the screen buffer directly (not through a trap). This is how a
    /// custom game blitter is located for disassembly — Lunatic Fringe draws
    /// its whole playfield with its own code, and the write sites are the only
    /// ground truth for what geometry it believes the screen has.
    fn watch_screen_write(&mut self, addr: u32, len: u32) {
        if !self.diag.watch_screen {
            return;
        }
        let base = self.qd.screen_base;
        let size = quickdraw::SCREEN_ROW_BYTES * u32::from(quickdraw::SCREEN_HEIGHT);
        if addr.wrapping_add(len) <= base || addr >= base.wrapping_add(size) {
            return;
        }
        let pc = ad_m68k::current_pc();
        if self.screen_writers.insert(pc) {
            eprintln!(
                "[watch] screen write from PC {pc:#x} (+{:#x}) at offset {:#x}",
                pc.wrapping_sub(0x8000),
                addr.wrapping_sub(base)
            );
        }
    }

    /// Copy a live handle's bytes into the resource it belongs to.
    ///
    /// A module that writes through a resource handle and then calls
    /// `_WriteResource` never releases it, so the release-time sync never runs
    /// and the store would persist the bytes as they were when loaded.
    fn sync_handle_bytes(&mut self, handle: u32) {
        if handle == 0 {
            return;
        }
        let Some(block) = self.mem.deref_handle(handle) else {
            return;
        };
        let Some(size) = self.mem.handle_size(handle) else {
            return;
        };
        let bytes = self.mem.read_bytes(block, size as usize);
        self.resources.set_bytes_of_handle(handle, bytes);
    }

    /// Write every changed resource through the host's sink, returning an OSErr.
    ///
    /// With no sink installed this is `noErr` and nothing is written: the lab and
    /// the tests run without a writable save location, and a module must not fail
    /// because of that. What the runtime must never do is report success on a
    /// write that *was* attempted and failed — hence the distinct `ioErr`.
    fn flush_resources(&mut self) -> i16 {
        if !self.resources.has_changes() {
            return 0;
        }
        let Some(sink) = self.sink.as_mut() else {
            return 0;
        };
        let changed = self.resources.changed();
        match sink.persist(&changed) {
            Ok(()) => {
                self.resources.mark_saved();
                0
            }
            Err(msg) => {
                eprintln!("[res] durable write failed: {msg}");
                oserr::IO_ERR
            }
        }
    }

    /// Write out anything still pending — host shutdown, i.e. `_CloseResFile`.
    ///
    /// # Errors
    /// The sink's own message, so the caller can report it rather than losing a
    /// save silently.
    pub fn flush_resources_on_close(&mut self) -> Result<(), String> {
        if !self.resources.has_changes() {
            return Ok(());
        }
        let Some(sink) = self.sink.as_mut() else {
            return Ok(());
        };
        let changed = self.resources.changed();
        sink.persist(&changed)?;
        self.resources.mark_saved();
        Ok(())
    }

    /// Fill an `EventRecord` with a null event, or answer After Dark's presence
    /// handshake if the module left its cookie there.
    ///
    /// Shared by `_GetOSEvent`/`_OSEventAvail` and `_GetNextEvent`/`_EventAvail`:
    /// all four report "no event" in a screen saver, and all four are asked the
    /// same question by the same modules. The cookie is checked **first** — the
    /// module has already written it into `message`, so clearing the record
    /// before looking would destroy the request and the module would conclude
    /// After Dark is not running.
    fn answer_no_event(&mut self, evt: u32) {
        if evt == 0 {
            return;
        }
        let msg = self.mem.read_u32(evt.wrapping_add(ad_detect::EVT_MESSAGE));
        if msg == ad_detect::MAGIC_REQUEST {
            let info = self.ad_info;
            self.mem
                .write_u32(evt.wrapping_add(ad_detect::EVT_MESSAGE), ad_detect::MAGIC_REPLY);
            self.mem.write_u32(evt.wrapping_add(ad_detect::EVT_WHERE), info);
            return;
        }
        // EventRecord { what:2, message:4, when:4, where:Point 4, modifiers:2 }.
        // `when` and `where` are filled in even for a null event, which is what
        // a real Mac does and what a module polling for elapsed time reads.
        let (h, v) = self.mouse;
        self.mem.write_u16(evt, 0); // nullEvt
        self.mem.write_u32(evt.wrapping_add(2), 0);
        self.mem.write_u32(evt.wrapping_add(6), self.ticks);
        self.mem.write_u16(evt.wrapping_add(10), v as u16);
        self.mem.write_u16(evt.wrapping_add(12), h as u16);
        self.mem.write_u16(evt.wrapping_add(14), 0);
    }

    /// Fresh full-screen `visRgn` and `clipRgn` for a port the module opens.
    ///
    /// Every port gets its **own** pair. Handing out `blankRgn` — which is what
    /// this used to do, for the screen port and for every `OpenPort`/`OpenCPort`
    /// alike — makes a `ClipRect` on any port a write through After Dark's blank
    /// region. That is not hypothetical: the moment `_ClipRect` stopped being a
    /// no-op, Flying Toasters opened five sprite ports, clipped each to its
    /// sprite, and `blankRgn` came back 32 pixels wide. Its `RandomRect` then
    /// divided by (spriteWidth − blankWidth) and took a divide-by-zero
    /// exception. The compatibility baseline caught it on the first run.
    fn new_port_regions(&mut self) -> (u32, u32) {
        let vis = quickdraw::QuickDraw::full_screen_region(&mut self.mem);
        let clip = quickdraw::QuickDraw::full_screen_region(&mut self.mem);
        (vis, clip)
    }

    // The cost, recorded rather than hidden, per the gameplan's W2 protocol:
    // **Globe** drew 170 pixels in two colours when its ports aliased `blankRgn`
    // and draws none now. Bisected to region *identity*, not to clipping going
    // live and not to the allocation shift — four throwaway handles at startup
    // leave its 170 pixels intact, so it genuinely depended on writing through
    // one alias and reading the other.
    //
    // Accepted anyway, and Globe's own log is why: it opens a 324x162 buffer,
    // decodes its 19,682-byte world map into it, opens a 180x180 globe buffer,
    // fills it white, and CopyBits it to the screen twenty times. 170 of 307,200
    // pixels in two colours is not a rendered globe — the sphere never worked,
    // and the matrix's `renders` column was flattering it. Trading a flattering
    // metric for a port that cannot corrupt the module's blank region is the
    // right way round.


    /// Release a closed port's own regions.
    ///
    /// Modules open and close ports per sprite, so leaking two regions each time
    /// would grow the heap for the life of the session. Never disposes `blankRgn`
    /// or the screen port's regions, however a module got hold of the handle.
    fn dispose_port_regions(&mut self, port_addr: u32) {
        if port_addr == 0 || port_addr == self.screen.port {
            return;
        }
        let keep = [
            self.qd.blank_rgn,
            self.mem.read_u32(self.screen.port.wrapping_add(port::port::VIS_RGN)),
            self.mem.read_u32(self.screen.port.wrapping_add(port::port::CLIP_RGN)),
        ];
        for off in [port::port::VIS_RGN, port::port::CLIP_RGN] {
            let h = self.mem.read_u32(port_addr.wrapping_add(off));
            if h != 0 && !keep.contains(&h) {
                self.mem.dispose_handle(h);
                self.mem.write_u32(port_addr.wrapping_add(off), 0);
            }
        }
    }

    /// Install the host's diagnostic switches.
    ///
    /// One entry point rather than a public field, because `qd.log` has to stay
    /// in step with `diag.qd_log` — the rasteriser keeps its own copy so it never
    /// reaches back through the Toolbox, and two places to set one switch is how
    /// a flag ends up silently half-on.
    pub fn set_diagnostics(&mut self, diag: Diagnostics) {
        self.diag = diag;
        self.qd.log = diag.qd_log;
    }

    pub fn tick(&mut self) {
        self.ticks = self.ticks.wrapping_add(1);
        globals::LowMem::set_ticks(&mut self.mem, self.ticks);
        // The wall clock advances with the tick counter: 60 ticks to the second.
        // `Time` starts at a fixed value so runs stay reproducible, but leaving
        // it *frozen* meant Clock drew a correct face whose hands never moved —
        // a still life, not a replica. Deriving seconds from ticks keeps both
        // properties: identical input still gives an identical run.
        if self.ticks % 60 == 0 {
            let now = self.mem.read_u32(globals::TIME);
            self.mem.write_u32(globals::TIME, now.wrapping_add(1));
        }
        // The Vertical Retrace Manager's countdown: each tick decrements every
        // installed task's vblCount; a task whose count reaches zero is due.
        // A count already at zero stays dormant — the task re-arms itself by
        // writing vblCount from inside its own handler.
        for i in 0..self.vbl_tasks.len() {
            let task = self.vbl_tasks[i];
            let count_at = task.wrapping_add(vbl::VBL_COUNT);
            let count = self.mem.read_u16(count_at);
            if count == 0 {
                continue;
            }
            let count = count - 1;
            self.mem.write_u16(count_at, count);
            if count == 0 && !self.vbl_due.contains(&task) {
                self.vbl_due.push(task);
            }
        }
    }

    /// Service one decoded trap.
    /// Load `count + 1` `ColorSpec`s from `table` into the palette.
    ///
    /// `count` is the entry count MINUS ONE, per Color QuickDraw, and a `start`
    /// of -1 means each spec carries its own index in its `value` field. Shared
    /// by `_SetEntries` and the video driver's `cscSetEntries` control call,
    /// which are the same operation reached two ways.
    fn set_entries(&mut self, start: i16, count: i16, table: u32) {
        for i in 0..=count.max(0) {
            let spec = table.wrapping_add(u32::from(i.unsigned_abs()) * 8);
            let value = self.mem.read_u16(spec) as i16;
            let r = (self.mem.read_u16(spec.wrapping_add(2)) >> 8) as u8;
            let g = (self.mem.read_u16(spec.wrapping_add(4)) >> 8) as u8;
            let b = (self.mem.read_u16(spec.wrapping_add(6)) >> 8) as u8;
            let idx = if start == -1 { value } else { start.saturating_add(i) };
            if let Some(slot) = self
                .qd
                .fb
                .palette
                .get_mut(usize::from(idx.unsigned_abs()).min(255))
            {
                *slot = [r, g, b];
            }
        }
        // The screen's `ColorTable` is the same palette seen from the emulated
        // side, so it has to move with it. A module that calls `SetEntries` and
        // then reads `pmTable` back — to match a colour, or to size a loop — must
        // not see the table it replaced. The seed advances because that is how
        // code caching against a `ColorTable` learns it changed.
        self.ct_seed = self.ct_seed.wrapping_add(1);
        let (ct, seed) = (self.screen.color_table, self.ct_seed);
        port::write_color_table(&mut self.mem, ct, &self.qd.fb.palette, seed);
    }

    fn dispatch(&mut self, t: Trap, regs: &mut dyn Registers) -> Result<(), TrapError> {
        match t.family {
            Family::Os => self.dispatch_os(t, regs),
            Family::Toolbox => self.dispatch_toolbox(t, regs),
        }
    }

    /// OS traps: arguments in registers, result code in `D0`.
    ///
    /// Dispatch keys on the **raw** trap word, not the canonical form: for most OS
    /// traps the flag bits are part of the identity rather than a modifier — see
    /// [`Trap::flags_are_modifiers`]. The allocator family is the exception and is
    /// folded first.
    fn dispatch_os(&mut self, t: Trap, regs: &mut dyn Registers) -> Result<(), TrapError> {
        let key = if t.flags_are_modifiers() {
            t.canonical()
        } else {
            t.word
        };
        match key {
            // _NewPtr: D0 = size in, A0 = pointer out, D0 = OSErr out.
            0xA01E => {
                let size = regs.data(0);
                let p = self.mem.new_ptr(size, t.flag_clear);
                regs.set_addr(0, p);
                let err = if p == 0 { oserr::MEM_FULL_ERR } else { 0 };
                regs.set_data(0, err as u32);
                globals::LowMem::set_mem_err(&mut self.mem, err);
            }
            // _NewHandle: D0 = size in, A0 = handle out, D0 = OSErr out.
            0xA022 => {
                let size = regs.data(0);
                let h = self.mem.new_handle(size, t.flag_clear);
                regs.set_addr(0, h);
                let err = if h == 0 { oserr::MEM_FULL_ERR } else { 0 };
                regs.set_data(0, err as u32);
                globals::LowMem::set_mem_err(&mut self.mem, err);
            }
            // _DisposPtr / _DisposHandle: A0 = the block.
            0xA01F => {
                let p = regs.addr(0);
                self.mem.dispose_ptr(p);
                regs.set_data(0, 0);
            }
            0xA023 => {
                let h = regs.addr(0);
                self.mem.dispose_handle(h);
                regs.set_data(0, 0);
            }
            // _GetHandleSize: A0 = handle, D0 = size (or negative OSErr).
            0xA025 => {
                let h = regs.addr(0);
                let size = self.mem.handle_size(h).unwrap_or(0);
                regs.set_data(0, size);
            }
            // _HLock / _HUnlock / _HPurge / _HNoPurge: A0 = handle.
            0xA029 => {
                let h = regs.addr(0);
                self.mem.set_handle_locked(h, true);
                regs.set_data(0, 0);
            }
            0xA02A => {
                let h = regs.addr(0);
                self.mem.set_handle_locked(h, false);
                regs.set_data(0, 0);
            }
            0xA049 => {
                let h = regs.addr(0);
                self.mem.set_handle_purgeable(h, true);
                regs.set_data(0, 0);
            }
            0xA04A => {
                let h = regs.addr(0);
                self.mem.set_handle_purgeable(h, false);
                regs.set_data(0, 0);
            }
            // _HGetState / _HSetState.
            0xA069 => {
                let h = regs.addr(0);
                let s = self.mem.handle_info(h).map_or(0, |b| b.state_byte());
                regs.set_data(0, u32::from(s));
            }
            0xA06A => {
                let h = regs.addr(0);
                let s = regs.data(0) as u8;
                self.mem
                    .set_handle_locked(h, s & ad_memory::handle::state::LOCK != 0);
                self.mem
                    .set_handle_purgeable(h, s & ad_memory::handle::state::PURGE != 0);
            }
            // _BlockMove: A0 = src, A1 = dst, D0 = count.
            // File Manager: A0 = a parameter block, D0 = OSErr out, and the
            // result is also stored in the block's `ioResult`.
            //
            // **No volume is mounted.** This runtime hands modules a resource
            // store, not a file system: there is no catalogue, no directory
            // hierarchy and no open-file table, so every one of these calls is
            // answered `nsvErr` — the accurate reply, and the only one that is
            // not a fabricated file system. It is deliberately not `fnfErr`:
            // PICS Player reads that specific code as "the file is absent, go
            // create it" (`cmpi.w #$ffd5` right before its `_PBHCreate` path),
            // and inviting a module down a write path this runtime cannot
            // service would be worse than a clean refusal.
            //
            // The three traps are the ones modules actually reach, each through
            // Think C's MacTraps glue:
            //
            // * `$A200` `_PBHOpen` — Picture Frame, whose calling routine the
            //   module's own MacsBug symbol names `HOPEN`.
            // * `$A20A` `_PBHOpenRF` — PICS Player. Its glue fills `ioNamePtr`
            //   (+18), `ioVRefNum` (+22), `ioPermssn` (+27), `ioMisc` (+28) and
            //   `ioDirID` (+48), then reads `ioRefNum` (+24) back: the
            //   `HParamBlockRec` layout exactly.
            // * `$A260` `_HFSDispatch`, selector in D0. Selector 8 is
            //   `PBGetFCBInfo`, used by MultiModule, Randomizer and Slide Show
            //   in the same idiom — Slide Show's routine is called
            //   `SlideShowModule.GetAfterDarkFilesFolderID` and reads
            //   `ioFCBParID` at +58 out of a 62-byte `FCBPBRec`, sizing its
            //   stack frame to exactly that plus a 256-byte name buffer.
            //   Selector 9 is `PBGetCatInfo`, which Picture Frame reaches next
            //   once the open has failed. Other selectors fail and name
            //   themselves, so the next module to need one says which.
            0xA200 | 0xA20A => {
                let pb = regs.addr(0);
                self.mem
                    .write_u16(pb.wrapping_add(filemgr::IO_RESULT), oserr::NSV_ERR as u16);
                regs.set_data(0, oserr::NSV_ERR as u32);
            }
            0xA260 => {
                let selector = regs.data(0) as u16;
                if !matches!(
                    selector,
                    filemgr::SEL_GET_FCB_INFO | filemgr::SEL_GET_CAT_INFO
                ) {
                    return Err(TrapError {
                        trap: t.word,
                        detail: format!("_HFSDispatch selector {selector} is not implemented"),
                    });
                }
                let pb = regs.addr(0);
                self.mem
                    .write_u16(pb.wrapping_add(filemgr::IO_RESULT), oserr::NSV_ERR as u16);
                regs.set_data(0, oserr::NSV_ERR as u32);
            }
            // _Control: A0 = a `CntrlParam` block, D0 = OSErr out.
            //
            // Satori's own `ROTATECO`(LORS) reaches this through MacTraps glue
            // that fills `ioCRefNum` (+24) from `(**GetGDevice()).gdRefNum` and
            // `csCode` (+26) from its second argument, then `_BlockMove`s 22
            // bytes of `csParam` (+28). So it is talking straight to the screen's
            // video driver, bypassing `_SetEntries`.
            //
            // csCode 3 is `cscSetEntries`, whose `csParam` is
            // `csTable: ColorSpecPtr; csStart, csCount: INTEGER` — confirmed by
            // the caller, which points `csTable` at a 2048-byte array it indexes
            // at +2 and +$7fa (entries 0 and 255 of a 256-entry `ColorSpec[]`)
            // and passes csStart 0, csCount 255. That is `_SetEntries(0, 255,
            // table)`, so it runs the same code.
            //
            // Any other control code fails and names itself: a video driver call
            // this runtime silently accepted would change the screen's state
            // invisibly.
            0xA004 => {
                let pb = regs.addr(0);
                let cs_code = self.mem.read_u16(pb.wrapping_add(cntrl::CS_CODE)) as i16;
                let ref_num = self.mem.read_u16(pb.wrapping_add(cntrl::IO_CREF_NUM)) as i16;
                if cs_code != cntrl::CSC_SET_ENTRIES {
                    return Err(TrapError {
                        trap: t.word,
                        detail: format!(
                            "_Control csCode {cs_code} on driver refNum {ref_num} \
                             is not implemented"
                        ),
                    });
                }
                let param = pb.wrapping_add(cntrl::CS_PARAM);
                let table = self.mem.read_u32(param);
                let start = self.mem.read_u16(param.wrapping_add(4)) as i16;
                let count = self.mem.read_u16(param.wrapping_add(6)) as i16;
                self.set_entries(start, count, table);
                self.mem.write_u16(pb.wrapping_add(cntrl::IO_RESULT), 0);
                regs.set_data(0, 0);
            }
            0xA02E => {
                let (src, dst, len) = (regs.addr(0), regs.addr(1), regs.data(0));
                self.mem.block_move(src, dst, len);
                regs.set_data(0, 0);
            }
            // _VInstall ($A033) and _SlotVInstall ($A06F): A0 = VBLTask. Queue
            // it; `tick` counts it down and the host runs it when due — see the
            // field docs on `vbl_tasks`. The slot variant additionally takes a
            // slot number in D0; on this single-screen host every slot's
            // retrace is the same 60Hz tick, so they share one queue. Lunatic
            // Fringe uses _VInstall on its B&W path and _SlotVInstall on its
            // colour path.
            0xA033 | 0xA06F => {
                let task = regs.addr(0);
                if task == 0 {
                    regs.set_data(0, oserr::V_TYP_ERR as u32);
                } else {
                    if !self.vbl_tasks.contains(&task) {
                        self.vbl_tasks.push(task);
                    }
                    regs.set_data(0, 0);
                }
            }
            // _VRemove ($A034) / _SlotVRemove ($A070): A0 = VBLTask. Also drop
            // any pending fire: running a task after its owner removed it
            // would call through a freed block.
            0xA034 | 0xA070 => {
                let task = regs.addr(0);
                self.vbl_tasks.retain(|&t| t != task);
                self.vbl_due.retain(|&t| t != task);
                regs.set_data(0, 0);
            }
            // No-ops that are safe because this heap never compacts or purges.
            // _MoveHHi, _CompactMem, _PurgeMem, _MaxApplZone, _SetApplLimit,
            // _InitApplZone, _EmptyHandle-as-noop, _StripAddress.
            0xA064 | 0xA04C | 0xA04D | 0xA063 | 0xA02D | 0xA02C => {
                regs.set_data(0, 0);
            }
            // _SwapMMUMode: D0.B = wanted addressing mode (0 = 24-bit,
            // 1 = 32-bit), previous mode returned in D0.B. Colour code swaps
            // to 32-bit around direct framebuffer access. Addressing here
            // never actually changes, so this just books the mode.
            0xA05D => {
                let wanted = regs.data(0) & 0xFF;
                let prev = u32::from(self.mmu_mode);
                self.mmu_mode = wanted as u8;
                regs.set_data(0, (regs.data(0) & 0xFFFF_FF00) | prev);
            }
            // _StripAddress: 24-bit mode is not emulated, so this is identity.
            0xA055 => {
                let a = regs.data(0);
                regs.set_data(0, a);
            }
            // _FreeMem / _MaxBlock / _MaxMem / _PurgeSpace: D0 = bytes free.
            // A module consults these before deciding it cannot run, so reporting
            // a real figure is what stops "Sorry, there is not enough memory."
            0xA01C | 0xA11D => {
                let free = self.mem.free_bytes();
                regs.set_data(0, free);
            }
            0xA061 | 0xA161 => {
                let big = self.mem.max_block();
                regs.set_data(0, big);
            }
            0xA162 => {
                // _PurgeSpace: total in D0, largest contiguous in A0.
                let (free, big) = (self.mem.free_bytes(), self.mem.max_block());
                regs.set_data(0, free);
                regs.set_addr(0, big);
            }
            // _ReserveMem: make room for a block. Nothing to do in a bump
            // allocator; report success.
            0xA01D | 0xA040 | 0xA240 | 0xA440 => {
                regs.set_data(0, 0);
            }
            // _SetHandleSize: A0 = handle, D0 = new size.
            0xA024 => {
                let (h, want) = (regs.addr(0), regs.data(0));
                let err = if self.mem.resize_handle(h, want) { 0 } else { oserr::MEM_FULL_ERR };
                regs.set_data(0, err as u32);
                globals::LowMem::set_mem_err(&mut self.mem, err);
            }
            // _GetPtrSize: not tracked for bump-allocated pointers.
            // _GetPtrSize: A0 = ptr, D0 = size (or a negative OSErr).
            0xA021 => {
                let p = regs.addr(0);
                match self.mem.ptr_size(p) {
                    Some(size) => {
                        regs.set_data(0, size);
                        globals::LowMem::set_mem_err(&mut self.mem, oserr::NO_ERR);
                    }
                    None => {
                        // memWZErr: asked about a block that is not there.
                        regs.set_data(0, oserr::MEM_WZ_ERR as u32);
                        globals::LowMem::set_mem_err(&mut self.mem, oserr::MEM_WZ_ERR);
                    }
                }
            }
            // _RecoverHandle: A0 = ptr in, A0 = handle out. Modules that stash a
            // dereferenced pointer use this to get back to the handle.
            0xA028 | 0xA128 => {
                let ptr = regs.addr(0);
                let h = self.mem.recover_handle(ptr);
                regs.set_addr(0, h);
                regs.set_data(0, 0);
            }
            0xA065 => {
                let sp = regs.sp();
                regs.set_data(0, sp.saturating_sub(ad_memory::HEAP_BASE));
            }
            // _MemError: D0 = last Memory Manager result.
            0xA093 => {
                let e = self.mem.read_u16(globals::MEM_ERR);
                regs.set_data(0, u32::from(e));
            }
            // _GetOSEvent / _OSEventAvail.
            //
            // A0 = EventRecord*, D0.W = event mask. There is never any user input
            // in a screen saver, so the answer is always "no event" — but this is
            // also where After Dark answered the presence handshake, so check for
            // the cookie before doing anything else. Note we must *not* clear the
            // record first: the cookie is already in it.
            0xA030 | 0xA031 => {
                let evt = regs.addr(0);
                self.answer_no_event(evt);
                // Boolean false: no event was returned.
                regs.set_data(0, 0);
            }
            // _FlushEvents: nothing queued.
            0xA032 => regs.set_data(0, 0),
            // _Delay: A0 = ticks to wait on entry, D0 = final tick count on
            // exit (IM II-384). We advance the clock instead of sleeping, so a
            // soak run stays fast and deterministic.
            //
            // Reading the count from D0 instead of A0 was a frozen player, not
            // a wrong pause: D0 holds whatever the module last computed — for
            // Lunatic Fringe's death beat, a pointer-sized 1.6 million — and a
            // paced host then owes seven hours of wall clock before the next
            // frame is due.
            0xA03B => {
                let n = regs.addr(0);
                self.ticks = self.ticks.wrapping_add(n);
                globals::LowMem::set_ticks(&mut self.mem, self.ticks);
                regs.set_data(0, self.ticks);
            }
            // _GetTrapAddress / _GetToolTrapAddress / _GetOSTrapAddress.
            //
            // Modules use these to feature-detect: they fetch a trap's address and
            // compare it against `_Unimplemented`'s, calling the trap only if they
            // differ. So the addresses need to be *distinguishable*, not real. A
            // trap we know about gets a unique synthetic address; anything else
            // gets the single "unimplemented" address.
            //
            // Caveat, documented deliberately: a module that *calls* the returned
            // address rather than comparing it will fault. None on this disk does.
            0xA046 | 0xA146 | 0xA346 | 0xA746 => {
                let num = regs.data(0) as u16;
                let is_tool = matches!(t.word, 0xA746 | 0xA146);
                let canonical = if is_tool { 0xA800 | (num & 0x03FF) } else { 0xA000 | (num & 0x00FF) };
                // A patched trap reports the patch — that's how patch chains
                // fetch the "old" vector before installing their own.
                let addr = if let Some(&patched) = self.trap_patches.get(&canonical) {
                    patched
                } else if canonical == 0xA89F || traps::name_of(canonical).is_none() {
                    // The shared "unimplemented" slot, one past the table.
                    self.trap_table_base
                        .wrapping_add(TRAP_TABLE_ENTRIES * 2)
                } else {
                    self.trap_table_base
                        .wrapping_add(u32::from(canonical & 0x0FFF).wrapping_mul(2))
                };
                regs.set_addr(0, addr);
                regs.set_data(0, 0);
            }
            // _SetTrapAddress: record the patch. The runtime's own dispatch does
            // not route through it, but the host delivers interrupt-driven work
            // (keyboard events) by *calling* recorded patches — Lunatic Fringe
            // hooks the event path at game start and reads keys nowhere else.
            0xA047 | 0xA247 | 0xA647 => {
                let addr = regs.addr(0);
                let num = regs.data(0) as u16;
                let is_tool = matches!(t.word, 0xA647);
                let canonical = if is_tool { 0xA800 | (num & 0x03FF) } else { 0xA000 | (num & 0x00FF) };
                if self.diag.qd_log {
                    eprintln!("[trap] SetTrapAddress {canonical:#06x} -> {addr:#x}");
                }
                self.trap_patches.insert(canonical, addr);
                regs.set_data(0, 0);
            }
            // _GetZone / _SetZone / _HandleZone / _PtrZone / _SystemZone /
            // _ApplicZone: one heap, so all of these name the same zone.
            0xA11A | 0xA01B | 0xA126 | 0xA148 | 0xA150 => {
                regs.set_addr(0, self.zone);
                regs.set_data(0, 0);
            }
            // _Gestalt: D0 = selector, A0 = response out, D0 = OSErr out.
            0xA1AD => {
                let selector = regs.data(0);
                match gestalt(selector, &self.profile) {
                    Some(v) => {
                        regs.set_addr(0, v);
                        regs.set_data(0, 0);
                    }
                    None => {
                        // gestaltUndefSelectorErr; modules must handle this, and
                        // answering "unknown" is safer than inventing a value.
                        regs.set_addr(0, 0);
                        regs.set_data(0, (-5551i32) as u32);
                    }
                }
            }
            // _SysEnvirons: D0 = requested version, A0 = SysEnvRec out.
            0xA090 => {
                let rec = regs.addr(0);
                // machineType Quadra-ish, System 7.5.2, Color QuickDraw present.
                // hasFPU is byte 8 and hasColorQD byte 9 of one word. Writing
                // 0x0100 told every caller "no Color QuickDraw", and Lunatic
                // Fringe answered by switching to its Mac Plus path: 1-bit
                // sprites and x/8 pixel maths, painting the whole game into the
                // left eighth of the screen. Both bytes now come from the
                // profile so they cannot drift from what the runtime does.
                let p = &self.profile;
                let flags = (u16::from(p.fpu) << 8) | u16::from(p.color_quickdraw);
                for (off, v) in [
                    (0u32, 2i16),      // environsVersion
                    (2, 20),           // machineType (gestaltQuadra800-ish)
                    (4, p.system_version as i16),
                    (6, p.cpu.sysenvirons_processor()),
                    (8, flags as i16),
                    (10, 0),           // keyBoardType
                    (12, 0),           // atDrvrVersNum
                    (14, 0),           // sysVRefNum
                ] {
                    self.mem.write_u16(rec.wrapping_add(off), v as u16);
                }
                regs.set_data(0, 0);
            }
            _ => {
                self.log.record_missing(&t);
                return Err(TrapError {
                    trap: t.word,
                    detail: format!("{} is not implemented", t.label()),
                });
            }
        }
        Ok(())
    }

    /// Toolbox traps: Pascal convention on the stack, plus the **auto-pop** bit.
    ///
    /// A Toolbox trap word with bit 10 set is reached from glue that has pushed
    /// the caller's return address *on top of* the arguments, and expects the trap
    /// to return straight to the caller. Both halves of that are handled here,
    /// once, rather than in each trap:
    ///
    /// * the return address is lifted off and `SP` advanced past it, so every
    ///   handler below sees its arguments at `SP+0` exactly as it always did;
    /// * the resume PC is redirected to the caller, because the instruction after
    ///   the trap word is the glue's own `bsr` dispatch table, and falling through
    ///   re-enters the stub forever.
    ///
    /// Mountains is the module that proves the convention is in use: its Think C
    /// glue reaches `_ColorUtilities` as `$AC2E`, and reading `SP+0` as the
    /// selector gave the high half of a return address — zero, because module code
    /// sits below 64 K of its base — so eight requests for selector 7 were
    /// reported as "selector 0 is not implemented".
    fn dispatch_toolbox(&mut self, t: Trap, regs: &mut dyn Registers) -> Result<(), TrapError> {
        if !t.auto_pop {
            return self.dispatch_toolbox_args(t, regs);
        }
        let sp = regs.sp();
        let caller = self.mem.read_u32(sp);
        regs.set_sp(sp.wrapping_add(4));
        let result = self.dispatch_toolbox_args(t, regs);
        // On failure the error is what matters and the frame is discarded, but
        // redirecting anyway keeps the two paths identical for anything that
        // recovers.
        regs.set_resume_pc(caller);
        result
    }

    /// The trap handlers themselves, with arguments at `SP+0`.
    fn dispatch_toolbox_args(&mut self, t: Trap, regs: &mut dyn Registers) -> Result<(), TrapError> {
        // Host callouts arrive as A-line words in the reserved range. None are
        // implemented yet: each failure names the slot's provenance, which is the
        // discovery mechanism — implement what real modules actually call.
        if (callout::WORD_BASE..callout::WORD_BASE + callout::SLOTS).contains(&t.word) {
            let idx = usize::from(t.word - callout::WORD_BASE);
            let kind = self
                .callout_kinds
                .get(idx)
                .copied()
                .unwrap_or(Callout::Discover);
            let sp = regs.sp();
            // Pascal, pushed left to right: the LAST argument is nearest the
            // return address at SP.
            let arg = |mem: &mut Memory, n: u32| mem.read_u32(sp.wrapping_add(4 + n * 4));
            match kind {
                Callout::MemFind => {
                    // FindMemory(params, size): last-pushed (arg 0) is the size.
                    let size = arg(&mut self.mem, 0).clamp(16, 4 * 1024 * 1024);
                    let h = self.mem.new_handle(size.saturating_add(16), true);
                    self.pascal_callout_return(regs, 8, Some(h));
                }
                Callout::MemRelease | Callout::MemRestore | Callout::MemUse => {
                    // Lock-state bookkeeping; this heap never moves blocks.
                    let h = arg(&mut self.mem, 0);
                    self.mem
                        .set_handle_locked(h, matches!(kind, Callout::MemUse));
                    self.pascal_callout_return(regs, 4, None);
                }
                Callout::SndOpen => {
                    // struct SoundInfo { long privateData; Boolean soundDisabled;
                    //                    Boolean hasSoundIOManager; ... }
                    // The glue writes privateData itself; we say sound works.
                    let h = self.mem.new_handle(16, true);
                    if let Some(block) = self.mem.deref_handle(h) {
                        self.mem.write_u8(block.wrapping_add(4), 0); // enabled
                        self.mem.write_u8(block.wrapping_add(5), 1); // has snd IO
                    }
                    self.pascal_callout_return(regs, 4, Some(h));
                }
                Callout::SndClose => {
                    self.pascal_callout_return(regs, 8, None);
                }
                Callout::SndPlay => {
                    // PlaySound(info, chan, snd): last-pushed (arg 0) is the
                    // sound handle. Decode it to PCM and queue it for the
                    // host's audio layer.
                    let h = arg(&mut self.mem, 0);
                    let chan = arg(&mut self.mem, 1);
                    self.play_snd_handle(h, chan);
                    self.pascal_callout_return(regs, 12, None);
                }
                Callout::SndLength => {
                    // GetSoundLength(info, snd) -> ticks, from the real header.
                    let h = arg(&mut self.mem, 0);
                    let ticks = self
                        .decode_snd_handle(h)
                        .map_or(30, |(_, s)| s.ticks());
                    self.pascal_callout_return(regs, 8, Some(ticks));
                }
                Callout::SndQuiet | Callout::SndFlush => {
                    // QuietSound(info, chan) / FlushSound(info, chan): the last
                    // pushed argument is the channel. Recorded as a stop event so
                    // an output device silences the right voice; a game that
                    // fires, quiets, and fires again depends on the *order* of
                    // these against the plays, which is why they share one stream.
                    let chan = arg(&mut self.mem, 0);
                    let tick = self.ticks;
                    self.sounds.stop(chan, tick);
                    self.pascal_callout_return(regs, 8, None);
                }
                Callout::SndBusy => {
                    // A silent backend is never busy — and saying so keeps
                    // game loops that gate effects on it moving.
                    self.pascal_callout_return_bool(regs, 8, false);
                }
                Callout::CpuException { short_frame } => {
                    let name = self
                        .callout_names
                        .get(idx)
                        .cloned()
                        .unwrap_or_else(|| format!("68000 exception, slot {idx}"));
                    // The exception frame is at SP: SR, then the PC of (or after)
                    // the faulting instruction.
                    let detail = if short_frame {
                        let sr = self.mem.read_u16(sp);
                        let pc = self.mem.read_u32(sp.wrapping_add(2));
                        format!("{name} at PC {pc:#x} (SR {sr:#06x})")
                    } else {
                        format!("{name} (long exception frame; PC not decoded)")
                    };
                    return Err(TrapError {
                        trap: t.word,
                        detail,
                    });
                }
                Callout::Discover => {
                    let name = self
                        .callout_names
                        .get(idx)
                        .cloned()
                        .unwrap_or_else(|| format!("unregistered slot {idx}"));
                    let args: Vec<String> = (0..4)
                        .map(|i| format!("{:#x}", arg(&mut self.mem, i)))
                        .collect();
                    return Err(TrapError {
                        trap: t.word,
                        detail: format!(
                            "host callout [{name}] invoked; stack: {}",
                            args.join(" ")
                        ),
                    });
                }
            }
            return Ok(());
        }
        match t.canonical() {
            // _Secs2Date: register-based despite living in the Toolbox number
            // space — D0 = seconds since 1904, A0 = DateTimeRec out
            // {year, month, day, hour, minute, second, dayOfWeek} as words.
            0xA9C6 => {
                let secs = regs.data(0);
                let rec = regs.addr(0);
                let (mut days, rem) = (secs / 86_400, secs % 86_400);
                let (hour, min, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);
                // 1 Jan 1904 was a Friday; dayOfWeek runs 1=Sunday..7=Saturday.
                let dow = ((days + 5) % 7 + 1) as u16;
                let mut year = 1904u32;
                loop {
                    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
                    let len = if leap { 366 } else { 365 };
                    if days < len {
                        break;
                    }
                    days -= len;
                    year += 1;
                }
                let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
                let month_lens = [
                    31,
                    if leap { 29 } else { 28 },
                    31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
                ];
                let mut month = 1u16;
                for len in month_lens {
                    if days < len {
                        break;
                    }
                    days -= len;
                    month += 1;
                }
                for (i, v) in [
                    year as u16,
                    month,
                    days as u16 + 1,
                    hour as u16,
                    min as u16,
                    sec as u16,
                    dow,
                ]
                .iter()
                .enumerate()
                {
                    self.mem.write_u16(rec.wrapping_add(i as u32 * 2), *v);
                }
                regs.set_data(0, 0);
            }
            // FUNCTION Random: INTEGER;
            0xA861 => {
                let seed = globals::LowMem::rnd_seed(&mut self.mem);
                let (next, result) = random::next(seed);
                globals::LowMem::set_rnd_seed(&mut self.mem, next);
                let s = Stack::new(regs);
                s.finish_u16(&mut self.mem, result as u16);
            }
            // FUNCTION GetNextEvent / EventAvail(mask: INTEGER;
            //                                    VAR e: EventRecord): BOOLEAN;
            //
            // A screen saver owns the machine, so there is never a real event to
            // report: both answer FALSE. The record still has to be filled in,
            // and the After Dark presence cookie has to be honoured *before*
            // anything is written over it — see `_GetOSEvent`, which shares
            // `answer_no_event` so the two paths cannot drift apart.
            0xA970 | 0xA971 => {
                let mut s = Stack::new(regs);
                let evt = s.pop_u32(&mut self.mem);
                let _mask = s.pop_i16(&mut self.mem);
                self.answer_no_event(evt);
                s.finish_bool(&mut self.mem, false);
            }
            // PROCEDURE GetMouse(VAR mouseLoc: Point);
            0xA972 => {
                let mut s = Stack::new(regs);
                let out = s.pop_u32(&mut self.mem);
                let (h, v) = self.mouse;
                self.mem.write_u16(out, v as u16); // Point is { v; h }
                self.mem.write_u16(out.wrapping_add(2), h as u16);
                s.finish();
            }
            // FUNCTION StillDown / Button / WaitMouseUp: BOOLEAN;
            //
            // The button is never down: After Dark blanked on idle, and the
            // first click dismissed the saver rather than reaching the module.
            0xA973 | 0xA974 | 0xA977 => {
                let s = Stack::new(regs);
                s.finish_bool(&mut self.mem, false);
            }
            // FUNCTION TickCount: LONGINT;
            //
            // Called from 260 sites across 48 of the 66 modules — the most-used
            // trap on the disk after QuickDraw's — and answered at $A973 until
            // the block was re-derived from the call sites (see `traps.rs`).
            0xA975 => {
                let ticks = self.ticks;
                let s = Stack::new(regs);
                s.finish_u32(&mut self.mem, ticks);
            }
            // PROCEDURE GetKeys(VAR k: KeyMap); — copy the low-memory KeyMap
            // at $174, which is where the host's input layer writes key state.
            // Lunatic Fringe polls Caps Lock straight from $178; the two views
            // must agree. Strange Attractors is the caller that pins the trap
            // number down: it passes a 16-byte local and tests bit 1 of the
            // long at +4, which is a `KeyMap` and nothing else.
            0xA976 => {
                let mut s = Stack::new(regs);
                let out = s.pop_u32(&mut self.mem);
                for i in 0..16 {
                    let b = self.mem.read_u8(globals::KEY_MAP + i);
                    self.mem.write_u8(out.wrapping_add(i), b);
                }
                s.finish();
            }
            // PROCEDURE SysBeep(duration: INTEGER);
            0xA9C8 => {
                let mut s = Stack::new(regs);
                let _duration = s.pop_i16(&mut self.mem);
                s.finish();
            }
            // PROCEDURE InitGraf(globalPtr: Ptr);
            0xA86E => {
                let mut s = Stack::new(regs);
                let ptr = s.pop_u32(&mut self.mem);
                s.finish();
                self.init_graf(ptr);
            }
            // PROCEDURE OpenPort / InitPort(port: GrafPtr);
            //
            // Each port gets its own PixMap, initialised to the screen's
            // characteristics. Modules then redirect it to an offscreen buffer,
            // and that must not disturb any other port.
            0xA86F | 0xA86D => {
                let mut s = Stack::new(regs);
                let addr = s.pop_u32(&mut self.mem);
                s.finish();
                let base = self.qd.screen_base;
                let pm = Screen::new_screen_pixmap(&mut self.mem, base);
                let (vis, clip) = self.new_port_regions();
                Screen::init_port(&mut self.mem, addr, base, pm, vis, clip);
                self.set_port(addr);
            }
            // PROCEDURE ClosePort(port: GrafPtr);
            //
            // Closing the current port must not leave it current: drawing would
            // keep going to a dead offscreen buffer. Flying Toasters opens five
            // sprite ports during Initialize and closes them, and its `Blank`
            // then painted into the last one instead of the screen.
            0xA87D => {
                let mut s = Stack::new(regs);
                let addr = s.pop_u32(&mut self.mem);
                s.finish();
                self.dispose_port_regions(addr);
                if addr == self.cur_port {
                    let screen = self.screen.port;
                    self.set_port(screen);
                }
            }
            // FUNCTION GetMainDevice / GetGDevice: GDHandle;
            0xAA2A | 0xAA32 => {
                let gd = self.screen.device;
                let s = Stack::new(regs);
                s.finish_u32(&mut self.mem, gd);
            }
            // FUNCTION GetDeviceList: GDHandle;
            0xAA29 => {
                let gd = self.screen.device;
                let s = Stack::new(regs);
                s.finish_u32(&mut self.mem, gd);
            }
            // FUNCTION GetNextDevice(cur: GDHandle): GDHandle;  single monitor.
            0xAA2B => {
                let mut s = Stack::new(regs);
                let _cur = s.pop_u32(&mut self.mem);
                s.finish_u32(&mut self.mem, 0);
            }
            // FUNCTION GetMaxDevice(globalRect: Rect): GDHandle;
            0xAA27 => {
                let mut s = Stack::new(regs);
                let _r = s.pop_u32(&mut self.mem);
                let gd = self.screen.device;
                s.finish_u32(&mut self.mem, gd);
            }
            // PROCEDURE SetGDevice(gd: GDHandle);
            0xAA31 => {
                let mut s = Stack::new(regs);
                let gd = s.pop_u32(&mut self.mem);
                if gd != 0 {
                    self.mem.write_u32(ad_memory::globals::THE_GDEVICE, gd);
                }
                s.finish();
            }
            // FUNCTION TestDeviceAttribute(gd: GDHandle; attr: INTEGER): BOOLEAN;
            0xAA2C => {
                let mut s = Stack::new(regs);
                let attr = s.pop_i16(&mut self.mem);
                let _gd = s.pop_u32(&mut self.mem);
                use port::device_attr as a;
                // One active main colour screen.
                let yes = matches!(
                    attr,
                    a::GD_DEV_TYPE | a::MAIN_SCREEN | a::SCREEN_DEVICE | a::SCREEN_ACTIVE
                );
                s.finish_bool(&mut self.mem, yes);
            }
            // FUNCTION QDError: INTEGER;
            0xAA40 => {
                let s = Stack::new(regs);
                s.finish_u16(&mut self.mem, 0);
            }
            // Resource Manager.
            0xA9A0 | 0xA81F => {
                // FUNCTION GetResource(theType: ResType; theID: INTEGER): Handle;
                let mut s = Stack::new(regs);
                let id = s.pop_i16(&mut self.mem);
                let ty = s.pop_u32(&mut self.mem).to_be_bytes();
                let h = self.resources.get(&mut self.mem, &ty, id);
                if self.diag.qd_log {
                    // The block address matters for code resources: it is the
                    // base that turns a disassembly offset into a live PC.
                    let block = self.mem.deref_handle(h).unwrap_or(0);
                    eprintln!(
                        "[res] GetResource '{}' {id} -> handle {h:#x} block {block:#x}",
                        String::from_utf8_lossy(&ty)
                    );
                }
                s.finish_u32(&mut self.mem, h);
            }
            0xA9A1 | 0xA820 => {
                // FUNCTION GetNamedResource(theType: ResType; name: Str255): Handle;
                let mut s = Stack::new(regs);
                let name_ptr = s.pop_u32(&mut self.mem);
                let ty = s.pop_u32(&mut self.mem).to_be_bytes();
                let len = usize::from(self.mem.read_u8(name_ptr));
                let name_bytes = self.mem.read_bytes(name_ptr.wrapping_add(1), len);
                let name = ad_resource::macroman::decode(&name_bytes);
                let h = self.resources.get_named(&mut self.mem, &ty, &name);
                s.finish_u32(&mut self.mem, h);
            }
            // FUNCTION GetPicture/GetCursor/GetPattern/GetString/GetIcon(id): Handle;
            // Sugar over GetResource with a fixed type.
            0xA9B8..=0xA9BC => {
                let ty: &[u8; 4] = match t.canonical() {
                    0xA9BC => b"PICT",
                    0xA9B9 => b"CURS",
                    0xA9B8 => b"PAT ",
                    0xA9BA => b"STR ",
                    _ => b"ICON",
                };
                let mut s = Stack::new(regs);
                let id = s.pop_i16(&mut self.mem);
                let h = self.resources.get(&mut self.mem, ty, id);
                s.finish_u32(&mut self.mem, h);
            }
            // FUNCTION GetMenu(menuID: INTEGER): MenuHandle;
            //
            // Randomizer reserves four bytes, pushes `#128`, and treats a nil
            // result as fatal ("could not load the module list"); it then calls
            // $A950 with the handle for a word count and $A946 for each item's
            // text, and finishes with `_ReleaseResource` on that same handle.
            // The release is why this hands back the resource handle rather than
            // a detached copy.
            //
            // A 'MENU' resource *is* a `MenuInfo`: `menuID(2) menuWidth(2)
            // menuHeight(2) menuProc(4) enableFlags(4)` then `menuData`, a
            // Str255 title followed by one record per item — text, then icon,
            // key, mark and style bytes — ending at a zero length byte.
            // Confirmed against Randomizer's MENU 128, whose 30 items are the
            // module names it randomises between.
            0xA9BF => {
                let mut s = Stack::new(regs);
                let id = s.pop_i16(&mut self.mem);
                let h = self.resources.get(&mut self.mem, b"MENU", id);
                s.finish_u32(&mut self.mem, h);
            }
            // FUNCTION CountMItems(theMenu: MenuHandle): INTEGER;
            0xA950 => {
                let mut s = Stack::new(regs);
                let menu = s.pop_u32(&mut self.mem);
                let n = menu_items(&mut self.mem, menu).len();
                s.finish_u16(&mut self.mem, u16::try_from(n).unwrap_or(u16::MAX));
            }
            // PROCEDURE GetItem(theMenu: MenuHandle; item: INTEGER;
            //                   VAR itemString: Str255);
            //
            // MultiModule proves the last argument is a Str255 and not a single
            // character: it passes a 258-byte stack local, then `_BlockMove`s
            // `buffer[0] + 1` bytes out of it. An out-of-range item yields the
            // empty string, which is what the Toolbox does.
            0xA946 => {
                let mut s = Stack::new(regs);
                let out = s.pop_u32(&mut self.mem);
                let item = s.pop_i16(&mut self.mem);
                let menu = s.pop_u32(&mut self.mem);
                let items = menu_items(&mut self.mem, menu);
                let text = usize::try_from(item)
                    .ok()
                    .and_then(|i| i.checked_sub(1))
                    .and_then(|i| items.get(i))
                    .copied();
                match text {
                    Some((at, len)) => {
                        let bytes = self.mem.read_bytes(at, usize::from(len) + 1);
                        self.mem.write_bytes(out, &bytes);
                    }
                    None => self.mem.write_u8(out, 0),
                }
                s.finish();
            }
            // PROCEDURE GetItmStyle(theMenu: MenuHandle; item: INTEGER;
            //                       VAR chStyle: Style);
            //
            // MultiModule reads each playlist entry's style and skips the entry
            // when it is 2 — italic, the Mac idiom for "this module is missing".
            //
            // The value is written as a **zero-extended word**, not a byte. That
            // is not a liberty: MultiModule reaches the trap through Think C
            // glue that reserves two bytes, passes their address as the VAR, and
            // afterwards does `MOVE.W (A7)+,D0 ; MOVE.B D0,(A1)` — it takes the
            // *low* half. Writing only the byte at the pointer would leave that
            // half undefined and make the module's comparison depend on stack
            // residue.
            0xA941 => {
                let mut s = Stack::new(regs);
                let out = s.pop_u32(&mut self.mem);
                let item = s.pop_i16(&mut self.mem);
                let menu = s.pop_u32(&mut self.mem);
                let items = menu_items(&mut self.mem, menu);
                let style = usize::try_from(item)
                    .ok()
                    .and_then(|i| i.checked_sub(1))
                    .and_then(|i| items.get(i))
                    .map(|(at, len)| {
                        at.wrapping_add(u32::from(*len))
                            .wrapping_add(1 + MENU_ITEM_STYLE)
                    })
                    .map_or(0, |a| self.mem.read_u8(a));
                self.mem.write_u16(out, u16::from(style));
                s.finish();
            }
            // PROCEDURE GetResInfo(h; VAR id; VAR type; VAR name: Str255);
            0xA9A8 => {
                let mut s = Stack::new(regs);
                let name_out = s.pop_u32(&mut self.mem);
                let type_out = s.pop_u32(&mut self.mem);
                let id_out = s.pop_u32(&mut self.mem);
                let h = s.pop_u32(&mut self.mem);
                if let Some((e, ty, id)) = self.resources.info_for(h) {
                    let name = e.name.clone();
                    self.mem.write_u16(id_out, id as u16);
                    self.mem.write_u32(type_out, u32::from_be_bytes(ty));
                    let bytes = name.unwrap_or_default().into_bytes();
                    let n = bytes.len().min(255);
                    self.mem.write_u8(name_out, n as u8);
                    for (i, b) in bytes.iter().take(n).enumerate() {
                        self.mem
                            .write_u8(name_out.wrapping_add(1 + i as u32), *b);
                    }
                    globals::LowMem::set_res_err(&mut self.mem, 0);
                } else {
                    globals::LowMem::set_res_err(&mut self.mem, oserr::RES_NOT_FOUND);
                }
                s.finish();
            }
            // PROCEDURE SetEntries(start, count: INTEGER; aTable: cSpecArrayPtr);
            // `count` is the entry count MINUS ONE, per Color QuickDraw. This is
            // where a module's own palette becomes authoritative — a fidelity
            // requirement, not an optimisation.
            0xAA3F => {
                let mut s = Stack::new(regs);
                let table = s.pop_u32(&mut self.mem);
                let count = s.pop_i16(&mut self.mem);
                let start = s.pop_i16(&mut self.mem);
                self.set_entries(start, count, table);
                s.finish();
            }
            0xA9A3 => {
                // PROCEDURE ReleaseResource(theResource: Handle);
                let mut s = Stack::new(regs);
                let h = s.pop_u32(&mut self.mem);
                self.resources.release(&mut self.mem, h);
                s.finish();
            }
            // PROCEDURE AddResource(theData: Handle; theType: ResType;
            //                       theID: INTEGER; name: Str255);
            // Lunatic Fringe's high-score save: RmveResource the old LFhs 128,
            // AddResource the new one, ReleaseResource. The store adopts the
            // bytes; the host persists changed resources after Close.
            0xA9AB => {
                let mut s = Stack::new(regs);
                let name_ptr = s.pop_u32(&mut self.mem);
                let id = s.pop_i16(&mut self.mem);
                let ty = s.pop_u32(&mut self.mem).to_be_bytes();
                let h = s.pop_u32(&mut self.mem);
                // Keep the module's own Str255 bytes: they are what a durable
                // write must put back, and MacRoman decode-then-encode is not
                // guaranteed to be the identity.
                let name = (name_ptr != 0)
                    .then(|| {
                        let len = usize::from(self.mem.read_u8(name_ptr));
                        self.mem.read_bytes(name_ptr.wrapping_add(1), len)
                    })
                    .filter(|b| !b.is_empty());
                if self.diag.qd_log {
                    eprintln!(
                        "[res] AddResource '{}' {id} ({} bytes)",
                        String::from_utf8_lossy(&ty),
                        self.mem.handle_size(h).unwrap_or(0)
                    );
                }
                self.resources.add(&mut self.mem, ty, id, name, h);
                s.finish();
            }
            // FUNCTION GetResAttrs(theResource: Handle): INTEGER;
            0xA9A6 => {
                let mut s = Stack::new(regs);
                let h = s.pop_u32(&mut self.mem);
                match self.resources.attrs_of(h) {
                    Some(a) => {
                        globals::LowMem::set_res_err(&mut self.mem, 0);
                        s.finish_u16(&mut self.mem, u16::from(a));
                    }
                    None => {
                        globals::LowMem::set_res_err(&mut self.mem, oserr::RES_NOT_FOUND);
                        s.finish_u16(&mut self.mem, 0);
                    }
                }
            }
            // PROCEDURE SetResAttrs(theResource: Handle; attrs: INTEGER);
            //
            // Fish! is the only caller on the disk. Recording the byte is what
            // lets a durable write put the resource back as the module left it.
            0xA9A7 => {
                let mut s = Stack::new(regs);
                let attrs = s.pop_i16(&mut self.mem);
                let h = s.pop_u32(&mut self.mem);
                let ok = self.resources.set_attrs(h, attrs as u8);
                globals::LowMem::set_res_err(
                    &mut self.mem,
                    if ok { 0 } else { oserr::RES_NOT_FOUND },
                );
                s.finish();
            }
            // PROCEDURE RmveResource(theResource: Handle);
            0xA9AD => {
                let mut s = Stack::new(regs);
                let h = s.pop_u32(&mut self.mem);
                self.resources.remove_by_handle(&mut self.mem, h);
                s.finish();
            }
            // PROCEDURE ChangedResource(theResource: Handle);
            //
            // Marks only. The Resource Manager writes changed resources when the
            // module asks — `_WriteResource` or `_UpdateResFile` — or when the
            // file closes, which for us is host shutdown.
            0xA9AA => {
                let mut s = Stack::new(regs);
                let h = s.pop_u32(&mut self.mem);
                self.resources.mark_changed(h);
                s.finish();
                globals::LowMem::set_res_err(&mut self.mem, 0);
            }
            // PROCEDURE WriteResource(theResource: Handle);
            //
            // A real durable write. `_ReleaseResource` has already synced the
            // handle's bytes into the store, and so has `_AddResource`; this is
            // the point at which they reach the disk. Lunatic Fringe's high score
            // arrives here.
            0xA9B0 => {
                let mut s = Stack::new(regs);
                let h = s.pop_u32(&mut self.mem);
                self.sync_handle_bytes(h);
                self.resources.mark_changed(h);
                s.finish();
                let err = self.flush_resources();
                globals::LowMem::set_res_err(&mut self.mem, err);
            }
            // PROCEDURE UpdateResFile(refNum: INTEGER); — write every change.
            0xA999 => {
                let mut s = Stack::new(regs);
                s.skip(2);
                s.finish();
                let err = self.flush_resources();
                globals::LowMem::set_res_err(&mut self.mem, err);
            }
            0xA9A2 => {
                // PROCEDURE LoadResource(theResource: Handle);  already resident.
                let mut s = Stack::new(regs);
                let _h = s.pop_u32(&mut self.mem);
                s.finish();
            }
            0xA9A5 => {
                // FUNCTION SizeRsrc(theResource: Handle): LONGINT;
                let mut s = Stack::new(regs);
                let h = s.pop_u32(&mut self.mem);
                let size = self.mem.handle_size(h).unwrap_or(0);
                s.finish_u32(&mut self.mem, size);
            }
            0xA9AF => {
                // FUNCTION ResError: INTEGER;
                let e = self.mem.read_u16(globals::RES_ERR);
                let s = Stack::new(regs);
                s.finish_u16(&mut self.mem, e);
            }
            0xA994 | 0xA9A4 => {
                // FUNCTION CurResFile / HomeResFile: INTEGER;  one open file.
                let mut s = Stack::new(regs);
                if t.canonical() == 0xA9A4 {
                    let _h = s.pop_u32(&mut self.mem);
                }
                s.finish_u16(&mut self.mem, 1);
            }
            0xA998 => {
                // PROCEDURE UseResFile(refNum: INTEGER);
                let mut s = Stack::new(regs);
                let _r = s.pop_i16(&mut self.mem);
                s.finish();
            }
            0xA99B => {
                // PROCEDURE SetResLoad(load: BOOLEAN);
                let mut s = Stack::new(regs);
                let _l = s.pop_u16(&mut self.mem);
                s.finish();
            }
            0xA99C | 0xA80D => {
                // FUNCTION CountResources(theType: ResType): INTEGER;
                let mut s = Stack::new(regs);
                let ty = s.pop_u32(&mut self.mem).to_be_bytes();
                let n = self.resources.count_of(&ty);
                s.finish_u16(&mut self.mem, n);
            }
            0xA99D | 0xA80E => {
                // FUNCTION GetIndResource(theType: ResType; index: INTEGER): Handle;
                let mut s = Stack::new(regs);
                let index = s.pop_i16(&mut self.mem);
                let ty = s.pop_u32(&mut self.mem).to_be_bytes();
                let h = self.resources.get_indexed(&mut self.mem, &ty, index);
                s.finish_u32(&mut self.mem, h);
            }
            // PROCEDURE SetPort(port: GrafPtr);
            0xA873 => {
                let mut s = Stack::new(regs);
                let p = s.pop_u32(&mut self.mem);
                s.finish();
                if p != 0 {
                    self.set_port(p);
                }
            }
            // PROCEDURE GetPort(VAR port: GrafPtr);
            0xA874 => {
                let mut s = Stack::new(regs);
                let out = s.pop_u32(&mut self.mem);
                let cur = self.cur_port;
                self.mem.write_u32(out, cur);
                s.finish();
            }
            0xA992 => {
                // PROCEDURE DetachResource(theResource: Handle);
                // The handle survives but stops being a resource, so the Resource
                // Manager must forget it without disposing the block.
                let mut s = Stack::new(regs);
                let h = s.pop_u32(&mut self.mem);
                self.resources.detach(h);
                s.finish();
            }
            // Sound Manager. Silent for now, but the calls must succeed and
            // balance: modules check the OSErr and give up on failure, and
            // `Sounds.o` is linked into 24 of the 66 modules.
            0xA809 => {
                // FUNCTION SndNewChannel(VAR chan: SndChannelPtr; synth: INTEGER;
                //                        init: LONGINT; proc: ProcPtr): OSErr;
                let mut s = Stack::new(regs);
                let _proc = s.pop_u32(&mut self.mem);
                let _init = s.pop_u32(&mut self.mem);
                let _synth = s.pop_i16(&mut self.mem);
                let out = s.pop_u32(&mut self.mem);
                // The only *runtime* arena allocation, so it is the only one
                // that reports failure to the guest rather than panicking. It
                // previously wrote whatever `alloc_host` returned — including 0
                // — and still answered `noErr`, handing the module a nil
                // channel it had every reason to trust.
                let (chan, err) = match self.mem.alloc_host(SND_CHANNEL_SIZE) {
                    Some(a) => (a.get(), oserr::NO_ERR),
                    None => (0, oserr::MEM_FULL_ERR),
                };
                self.mem.write_u32(out, chan);
                globals::LowMem::set_mem_err(&mut self.mem, err);
                s.finish_u16(&mut self.mem, err as u16);
            }
            0xA803 | 0xA801 => {
                // FUNCTION SndDisposeChannel(chan; quietNow: BOOLEAN): OSErr;
                let mut s = Stack::new(regs);
                let _quiet = s.pop_u16(&mut self.mem);
                let chan = s.pop_u32(&mut self.mem);
                let tick = self.ticks;
                self.sounds.stop(chan, tick);
                s.finish_u16(&mut self.mem, 0);
            }
            0xA807 => {
                // FUNCTION SndPlay(chan; sndHdl: Handle; async: BOOLEAN): OSErr;
                let mut s = Stack::new(regs);
                let _async = s.pop_u16(&mut self.mem);
                let snd_h = s.pop_u32(&mut self.mem);
                let chan = s.pop_u32(&mut self.mem);
                self.play_snd_handle(snd_h, chan);
                s.finish_u16(&mut self.mem, 0);
            }
            0xA805 | 0xA806 => {
                // FUNCTION SndDoCommand / SndDoImmediate(chan; cmd; noWait): OSErr;
                let mut s = Stack::new(regs);
                let _no_wait = s.pop_u16(&mut self.mem);
                let _cmd = s.pop_u32(&mut self.mem);
                let _chan = s.pop_u32(&mut self.mem);
                s.finish_u16(&mut self.mem, 0);
            }
            0xA808 => {
                // FUNCTION SndControl(id: INTEGER; VAR cmd: SndCommand): OSErr;
                let mut s = Stack::new(regs);
                let _cmd = s.pop_u32(&mut self.mem);
                let _id = s.pop_i16(&mut self.mem);
                s.finish_u16(&mut self.mem, 0);
            }
            // FP68K ($A9EB) and Elems68K ($A9EC) — SANE software floating point.
            //
            // Not the Pascal convention: operand *addresses* are pushed, then a
            // 16-bit opword. See `sane` for the encoding.
            0xA9EB | 0xA9EC => {
                let mut s = Stack::new(regs);
                let opword = s.pop_u16(&mut self.mem);
                let is_elems = t.canonical() == 0xA9EC;
                let operation = if is_elems { opword & 0x003F } else { opword & sane::op::MASK };
                let nargs = if is_elems {
                    match operation {
                        sane::elem::XPWRI | sane::elem::XPWRY
                        | sane::elem::COMPOUND | sane::elem::ANNUITY => 2,
                        _ => 1,
                    }
                } else {
                    sane::operand_count(operation)
                };
                // The destination was pushed last, so it is nearest SP.
                let dst = s.pop_u32(&mut self.mem);
                let src = if nargs == 2 {
                    Some(s.pop_u32(&mut self.mem))
                } else {
                    None
                };
                let outcome = if is_elems {
                    sane::elems68k(&mut self.mem, opword, dst, src)
                } else {
                    sane::fp68k(&mut self.mem, opword, dst, src)
                };
                match outcome {
                    Some(sane::SaneResult::Done) => s.finish(),
                    Some(cmp @ (sane::SaneResult::Compared(_) | sane::SaneResult::Unordered)) => {
                        // A SANE comparison answers **through the condition
                        // codes** — its caller's next instruction is a `bgt` or a
                        // `blt`. Setting only `D0`, as this did, left the branch
                        // reading the flags from before the trap, so SunBurst's
                        // `while (angle > limit) angle -= step` never terminated
                        // and surfaced as a hang half a million calls deep.
                        //
                        // `D0` is set as well, for the glue that tests a register
                        // instead. Unordered has no signed integer to report, so
                        // it takes `-1`, matching how the flags read it.
                        use std::cmp::Ordering;
                        let v: i16 = match cmp {
                            sane::SaneResult::Compared(Ordering::Greater) => 1,
                            sane::SaneResult::Compared(Ordering::Equal) => 0,
                            _ => -1,
                        };
                        s.finish();
                        regs.set_data(0, v as i32 as u32);
                        regs.set_condition_codes(sane::comparison_ccr(cmp));
                    }
                    None => {
                        self.log.record_missing(&t);
                        return Err(TrapError {
                            trap: t.word,
                            detail: format!(
                                "{} opword ${opword:04X} (op ${operation:02X}) is not implemented",
                                t.label()
                            ),
                        });
                    }
                }
            }
            // PROCEDURE OpenCPort / InitCPort(port: CGrafPtr);
            // The colour-port constructor; seven of the animation-heavy modules
            // build an offscreen or window port through this before drawing.
            0xAA00 | 0xAA01 => {
                let mut s = Stack::new(regs);
                let addr = s.pop_u32(&mut self.mem);
                s.finish();
                let base = self.qd.screen_base;
                let pm = port::Screen::new_screen_pixmap(&mut self.mem, base);
                let (vis, clip) = self.new_port_regions();
                port::Screen::init_port(&mut self.mem, addr, base, pm, vis, clip);
                self.set_port(addr);
            }
            // The Toolbox fixed-point maths pack. All exact integer arithmetic:
            // Fixed is 16.16, Fract is 2.30.
            0xA83F => {
                // FUNCTION Long2Fix(x: LONGINT): Fixed;
                let mut s = Stack::new(regs);
                let x = s.pop_u32(&mut self.mem) as i32;
                let v = i64::from(x) << 16;
                let v = v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
                s.finish_u32(&mut self.mem, v as u32);
            }
            0xA840 => {
                // FUNCTION Fix2Long(x: Fixed): LONGINT; rounds to nearest.
                let mut s = Stack::new(regs);
                let x = s.pop_u32(&mut self.mem) as i32;
                let v = (i64::from(x) + 0x8000) >> 16;
                s.finish_u32(&mut self.mem, v as u32);
            }
            0xA841 => {
                // FUNCTION Fix2Frac(x: Fixed): Fract;
                let mut s = Stack::new(regs);
                let x = s.pop_u32(&mut self.mem) as i32;
                let v = (i64::from(x) << 14).clamp(i64::from(i32::MIN), i64::from(i32::MAX));
                s.finish_u32(&mut self.mem, v as i32 as u32);
            }
            0xA842 => {
                // FUNCTION Frac2Fix(x: Fract): Fixed;
                let mut s = Stack::new(regs);
                let x = s.pop_u32(&mut self.mem) as i32;
                let v = (i64::from(x) + (1 << 13)) >> 14;
                s.finish_u32(&mut self.mem, v as u32);
            }
            0xA844 => {
                // FUNCTION X2Fix(x: extended): Fixed; the argument is pushed by
                // address, per the SANE-era convention for extended parameters.
                let mut s = Stack::new(regs);
                let p = s.pop_u32(&mut self.mem);
                let v = sane::read_ext(&mut self.mem, p);
                let fixed = (v * 65536.0).round().clamp(f64::from(i32::MIN), f64::from(i32::MAX));
                s.finish_u32(&mut self.mem, (fixed as i32) as u32);
            }
            0xA847 | 0xA848 => {
                // FUNCTION FracCos / FracSin(angle: Fixed): Fract; radians.
                let mut s = Stack::new(regs);
                let x = s.pop_u32(&mut self.mem) as i32;
                let a = f64::from(x) / 65536.0;
                let v = if t.canonical() == 0xA847 { a.cos() } else { a.sin() };
                let fr = (v * (1u64 << 30) as f64).round();
                s.finish_u32(&mut self.mem, (fr as i64 as i32) as u32);
            }
            0xA849 => {
                // FUNCTION FracSqrt(x: Fract): Fract; x treated as unsigned.
                let mut s = Stack::new(regs);
                let x = s.pop_u32(&mut self.mem);
                let v = (f64::from(x) / f64::from(1u32 << 30)).sqrt();
                let fr = (v * f64::from(1u32 << 30)).round();
                s.finish_u32(&mut self.mem, fr as u32);
            }
            0xA84A => {
                // FUNCTION FracMul(a, b: Fract): Fract;
                let mut s = Stack::new(regs);
                let b = s.pop_u32(&mut self.mem) as i32;
                let a = s.pop_u32(&mut self.mem) as i32;
                let v = (i64::from(a) * i64::from(b)) >> 30;
                s.finish_u32(&mut self.mem, v as i32 as u32);
            }
            0xA84B => {
                // FUNCTION FracDiv(a, b: Fract): Fract;
                let mut s = Stack::new(regs);
                let b = s.pop_u32(&mut self.mem) as i32;
                let a = s.pop_u32(&mut self.mem) as i32;
                let v = if b == 0 {
                    if a < 0 { i32::MIN } else { i32::MAX }
                } else {
                    ((i64::from(a) << 30) / i64::from(b))
                        .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
                };
                s.finish_u32(&mut self.mem, v as u32);
            }
            // PROCEDURE ShieldCursor(shieldRect: Rect; offsetPt: Point);
            0xA855 => {
                let mut s = Stack::new(regs);
                s.skip(8);
                s.finish();
            }
            // PROCEDURE OpColor(color: RGBColor); arithmetic-mode operand colour.
            0xAA21 => {
                let mut s = Stack::new(regs);
                s.skip(4);
                s.finish();
            }
            // PROCEDURE SetStdCProcs(VAR procs: CQDProcs); fill with zeros — the
            // runtime never dispatches through grafProcs, so defaults are moot.
            0xAA4E => {
                let mut s = Stack::new(regs);
                let p = s.pop_u32(&mut self.mem);
                for i in 0..52 {
                    self.mem.write_u8(p.wrapping_add(i), 0);
                }
                s.finish();
            }
            // PROCEDURE SetPortPix(pm: PixMapHandle);
            // Redirects the current port's pixels — this is how a module points a
            // port at an offscreen buffer, so it must actually take effect.
            0xAA06 => {
                let mut s = Stack::new(regs);
                let pm = s.pop_u32(&mut self.mem);
                s.finish();
                if pm != 0 {
                    let at = self.cur_port.wrapping_add(port::port::PORT_BITS);
                    self.mem.write_u32(at, pm);
                }
            }
            // PROCEDURE SetPortBits(bm: BitMap); the mono equivalent — copy the
            // 14-byte BitMap into the port's portBits.
            0xA875 => {
                let mut s = Stack::new(regs);
                let bm = s.pop_u32(&mut self.mem);
                s.finish();
                if bm != 0 {
                    let bytes = self.mem.read_bytes(bm, 14);
                    let at = self.cur_port.wrapping_add(port::port::PORT_BITS);
                    self.mem.write_bytes(at, &bytes);
                    // A BitMap in portBits means this is no longer a colour port.
                    self.mem.write_u16(self.cur_port.wrapping_add(6), 0);
                }
            }
            // PROCEDURE PortSize(width, height: INTEGER);
            // PROCEDURE MovePortTo(leftGlobal, topGlobal: INTEGER);
            //
            // Both reshape the current port's `portRect` without touching its
            // bits. Randomizer proves the pairing: `_GetPort`, then `$A877` with
            // a Point-sized long, then `_PortSize` with the next long, then
            // `_SetOrigin(0, 0)` — the standard port set-up sequence, two words
            // each.
            //
            // Accepted without effect, as `_PortSize` already was: drawing here
            // resolves through `portBits` and `_SetOrigin`, so a port's
            // `portRect` is not consulted. Writing it was tried against
            // ProtoToasters, whose own `RandomRect` helper divides by
            // `field.height - sprite.height` and dies on a zero — the equal
            // heights come from somewhere else, so the change bought nothing and
            // is left out rather than carried untested.
            0xA876 | 0xA877 => {
                let mut s = Stack::new(regs);
                s.skip(4); // two words
                s.finish();
            }
            // FUNCTION GetCIcon(id: INTEGER): CIconHandle;
            //
            // The result must be **independent of the resource**, which Confetti
            // Factory's own `SAFEGETCICON` proves: it calls `_GetCIcon`, then
            // `_GetResource('cicn', sameID)`, then `_ReleaseResource` on that,
            // and returns the icon — the idiom for "build the icon, then stop
            // paying for the resource copy". Handing back the resource handle
            // itself made that release free the icon out from under the module.
            0xAA1E => {
                let mut s = Stack::new(regs);
                let id = s.pop_i16(&mut self.mem);
                let bytes = self.resources.bytes_of(b"cicn", id).map(<[u8]>::to_vec);
                let h = match bytes {
                    Some(b) => {
                        let h = self.mem.new_handle(u32::try_from(b.len()).unwrap_or(0).max(1), false);
                        if let Some(block) = self.mem.deref_handle(h) {
                            self.mem.write_bytes(block, &b);
                        }
                        globals::LowMem::set_res_err(&mut self.mem, 0);
                        h
                    }
                    None => {
                        globals::LowMem::set_res_err(&mut self.mem, oserr::RES_NOT_FOUND);
                        0
                    }
                };
                s.finish_u32(&mut self.mem, h);
            }
            // PROCEDURE DisposCIcon(theIcon: CIconHandle);
            //
            // Plain disposal, because `_GetCIcon` allocated the block outright.
            // Confetti Factory's loop is GetCIcon / PlotCIcon / DisposCIcon per
            // confetti piece, 133 times in twenty frames.
            0xAA25 => {
                let mut s = Stack::new(regs);
                let h = s.pop_u32(&mut self.mem);
                self.mem.dispose_handle(h);
                s.finish();
            }
            // PROCEDURE DisposCTable(cTable: CTabHandle);
            //
            // GeoBounce pairs it with the `_GetCTable` above, which returns
            // either the module's own 'clut' or a synthesised table — a release
            // covers both, where a bare `_DisposHandle` would leave the store
            // holding a freed handle for the resource case.
            0xAA24 => {
                let mut s = Stack::new(regs);
                let h = s.pop_u32(&mut self.mem);
                self.resources.release(&mut self.mem, h);
                s.finish();
            }
            // FUNCTION GetCTable(id: INTEGER): CTabHandle;
            // Prefer the module's own 'clut'; otherwise synthesise one from the
            // current palette so the structure is always well-formed.
            0xAA18 => {
                let mut s = Stack::new(regs);
                let id = s.pop_i16(&mut self.mem);
                let mut h = self.resources.get(&mut self.mem, b"clut", id);
                if h == 0 {
                    h = self.mem.new_handle(8 + 256 * 8, true);
                    if let Some(block) = self.mem.deref_handle(h) {
                        self.mem.write_u32(block, u32::from(id as u16)); // ctSeed
                        self.mem.write_u16(block.wrapping_add(4), 0); // ctFlags
                        self.mem.write_u16(block.wrapping_add(6), 255); // ctSize
                        let palette = self.qd.fb.palette.clone();
                        for (i, c) in palette.iter().enumerate() {
                            let spec = block.wrapping_add(8 + (i as u32) * 8);
                            self.mem.write_u16(spec, i as u16);
                            for (j, v) in c.iter().enumerate() {
                                let w = (u16::from(*v) << 8) | u16::from(*v);
                                self.mem
                                    .write_u16(spec.wrapping_add(2 + (j as u32) * 2), w);
                            }
                        }
                    }
                    globals::LowMem::set_res_err(&mut self.mem, 0);
                }
                s.finish_u32(&mut self.mem, h);
            }
            // PROCEDURE DrawPicture(pic: PicHandle; dstRect: Rect);
            //
            // This is where the sprite modules' art lives: Flying Toasters renders
            // toasters through here into offscreen buffers before compositing.
            0xA8F6 => {
                let mut s = Stack::new(regs);
                let rect_ptr = s.pop_u32(&mut self.mem);
                let pic_handle = s.pop_u32(&mut self.mem);
                s.finish();

                let dst_rect = quickdraw::Rect::read(&mut self.mem, rect_ptr);
                let Some(pic) = self.mem.deref_handle(pic_handle) else {
                    return Ok(());
                };
                let pic_len = self.mem.handle_size(pic_handle).unwrap_or(0);
                // Draw into whatever the current port points at, which may be an
                // offscreen buffer rather than the screen.
                let bits_at = self.cur_port.wrapping_add(port::port::PORT_BITS);
                let Some(dst) = blit::Surface::resolve(&mut self.mem, bits_at) else {
                    return Ok(());
                };
                let palette = self.qd.fb.palette.clone();
                let to_index = move |_m: &mut Memory, rgb: [u8; 3]| -> u8 {
                    let mut best = 0u8;
                    let mut best_d = i32::MAX;
                    for (i, c) in palette.iter().enumerate() {
                        let d = (i32::from(c[0]) - i32::from(rgb[0])).pow(2)
                            + (i32::from(c[1]) - i32::from(rgb[1])).pow(2)
                            + (i32::from(c[2]) - i32::from(rgb[2])).pow(2);
                        if d < best_d {
                            best_d = d;
                            best = u8::try_from(i).unwrap_or(0);
                        }
                    }
                    best
                };
                let (fore, back) = (self.qd.fore, self.qd.back);
                let scratch = self.pict_scratch;
                if self.diag.qd_log {
                    eprintln!(
                        "[qd] DrawPicture pic={pic_handle:#x} len={pic_len} dst_rect={dst_rect:?} -> base={:#x} rb={} depth={}",
                        dst.base, dst.row_bytes, dst.pixel_size
                    );
                }
                if let Err(e) = pict::draw_picture(
                    &mut self.mem, pic, pic_len, &dst, &dst_rect, fore, back,
                    scratch, PICT_SCRATCH_SIZE, &to_index,
                ) {
                    // A malformed or unsupported picture must not be silently
                    // half-drawn: report it the way an unimplemented trap is.
                    return Err(TrapError {
                        trap: t.word,
                        detail: format!("DrawPicture failed: {e:?}"),
                    });
                }
            }
            // Cursor calls take no arguments and do nothing here. _SystemTask
            // ($A9B4) likewise: it gave time to desk accessories, none exist.
            0xA850 | 0xA852 | 0xA853 | 0xA856 | 0xA9B4 => {
                Stack::new(regs).finish();
            }
            // FUNCTION KeyTranslate(transData: Ptr; keycode: INTEGER;
            //                       VAR state: LONGINT): LONGINT;
            //
            // How a module turns a virtual key code into a character. Lunatic
            // Fringe calls it from a routine its own MacsBug symbol names
            // `CONVERTM`, to label each of its configurable controls — and this
            // trap used to be misidentified as `_SystemTask` and grouped with the
            // cursor no-ops, so it popped none of its ten bytes of arguments and
            // returned nothing. Every control in the game's key table read "N",
            // for none. See `traps.rs` for the derivation from the call site.
            //
            // The result is a long holding up to two characters, one per word.
            // A table lookup produces one, in the low byte.
            0xA9C3 => {
                let mut s = Stack::new(regs);
                let state_ptr = s.pop_u32(&mut self.mem);
                let keycode = s.pop_u16(&mut self.mem);
                let trans_data = s.pop_u32(&mut self.mem);
                // The modifier byte travels in the high half of the key code.
                let modifiers = (keycode >> 8) as u8;
                let code = keycode as u8;
                // Enough of the layout to reach any table a selector can name.
                let layout = self.mem.read_bytes(
                    trans_data,
                    (resources::kchr::TABLES + 256 * resources::kchr::TABLE_LEN) as usize,
                );
                let ch = resources::kchr_char(&layout, modifiers, code)
                    // Fall back to the built-in US mapping rather than answering
                    // "no character": a module labelling its controls would
                    // otherwise show a blank where a key certainly exists.
                    .filter(|&c| c != 0)
                    .unwrap_or_else(|| resources::us_char_for(code));
                // No dead keys in the synthesized layout, so no state carries.
                if state_ptr != 0 {
                    self.mem.write_u32(state_ptr, 0);
                }
                s.finish_u32(&mut self.mem, u32::from(ch));
            }
            // _Pack7 — Binary-Decimal Conversion. A selector word on the stack;
            // data moves through registers (Lunatic Fringe's Think C glue:
            // `clr.w -(a7); _Pack7`, with D0 = number, A0 = string). The
            // handler must pop exactly the selector — the glue removes its own
            // arguments after the trap returns.
            0xA9EE => {
                let mut s = Stack::new(regs);
                let selector = s.pop_u16(&mut self.mem);
                s.finish();
                match selector {
                    // NumToString: D0.L (signed) -> Str255 at A0.
                    0 => {
                        let num = regs.data(0) as i32;
                        let dst = regs.addr(0);
                        let text = num.to_string();
                        self.mem.write_u8(dst, u8::try_from(text.len()).unwrap_or(255));
                        for (i, b) in text.bytes().take(255).enumerate() {
                            self.mem.write_u8(dst.wrapping_add(1 + i as u32), b);
                        }
                    }
                    // StringToNum: Str255 at A0 -> D0.L. Sign then digits, with
                    // wrapping arithmetic, which is what the ROM did.
                    1 => {
                        let src = regs.addr(0);
                        let len = u32::from(self.mem.read_u8(src));
                        let mut value: i32 = 0;
                        let mut neg = false;
                        for i in 0..len {
                            let b = self.mem.read_u8(src.wrapping_add(1 + i));
                            match b {
                                b'-' if i == 0 => neg = true,
                                b'+' if i == 0 => {}
                                b'0'..=b'9' => {
                                    value = value
                                        .wrapping_mul(10)
                                        .wrapping_add(i32::from(b - b'0'));
                                }
                                _ => {}
                            }
                        }
                        if neg {
                            value = value.wrapping_neg();
                        }
                        regs.set_data(0, value as u32);
                    }
                    _ => {
                        return Err(TrapError {
                            trap: t.word,
                            detail: format!("Pack7 selector {selector} is not implemented"),
                        });
                    }
                }
            }
            _ => {
                if let Some(handled) = self.qd.dispatch(t, regs, &mut self.mem) {
                    return handled.map_err(|d| TrapError {
                        trap: t.word,
                        detail: d,
                    });
                }
                self.log.record_missing(&t);
                return Err(TrapError {
                    trap: t.word,
                    detail: format!("{} is not implemented", t.label()),
                });
            }
        }
        Ok(())
    }
}

impl Bus for Toolbox {
    fn read_u8(&mut self, addr: u32) -> u8 {
        self.mem.read_u8(addr)
    }
    fn read_u16(&mut self, addr: u32) -> u16 {
        self.mem.read_u16(addr)
    }
    fn read_u32(&mut self, addr: u32) -> u32 {
        self.mem.read_u32(addr)
    }
    fn write_u8(&mut self, addr: u32, value: u8) {
        // One branch when nothing is being watched, which is every product
        // run: this is the CPU's store path, and even two always-return-early
        // calls showed up in a profile of a busy game scene.
        if self.diag.watching() {
            self.watch_screen_write(addr, 1);
            self.watch_addr_write(addr, 1, u32::from(value));
        }
        self.mem.write_u8(addr, value);
    }
    fn write_u16(&mut self, addr: u32, value: u16) {
        if self.diag.watching() {
            self.watch_screen_write(addr, 2);
            self.watch_addr_write(addr, 2, u32::from(value));
        }
        self.mem.write_u16(addr, value);
    }
    fn write_u32(&mut self, addr: u32, value: u32) {
        if self.diag.watching() {
            self.watch_screen_write(addr, 4);
            self.watch_addr_write(addr, 4, value);
        }
        self.mem.write_u32(addr, value);
    }

    fn trap(&mut self, trap: u16, regs: &mut dyn Registers) -> Result<(), TrapError> {
        let t = Trap::decode(trap);
        self.log.record(&t, regs.trap_pc());
        self.dispatch(t, regs)
    }
}

#[cfg(test)]
mod tests;
