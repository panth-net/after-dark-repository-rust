//! After Dark 2.x host ABI.
//!
//! This is the layer that actually *runs* a module. It loads the code resource,
//! builds the `GMParamBlock`, and performs the Pascal call:
//!
//! ```c
//! pascal OSErr main(Handle *storage, RgnHandle blankRgn,
//!                   short message, GMParamBlockPtr params);
//! ```
//!
//! Arguments are pushed **left to right**, so `storage` goes on first and
//! `params` ends up nearest the stack pointer. 14 bytes total, a 16-bit result
//! left on the stack, and the callee pops — all verified against Berkeley
//! Systems' `GraphicsModule_main.c` (see `reference/sdk/` and `docs/LEARNINGS.md`).
//!
//! # Returning to the host
//!
//! There is no host code in emulated memory to return to, so the call pushes a
//! **sentinel return address**. When the module's final `RTS` jumps there,
//! execution has finished; the runtime detects the PC and stops. That avoids
//! needing a trampoline or guessing at cycle counts.

pub mod decompress;

use ad_m68k::{Cpu, RunError};
use ad_memory::globals;
use ad_resource::{AdModule, CodeLayout, GmMessage, GmResult, ResourceFork};
use ad_toolbox::{quickdraw, Toolbox};

/// Where the module's code is loaded.
///
/// Below the heap deliberately: see the address map in `ad_memory`. With the heap
/// above it, a growing heap can never reach the code.
pub const CODE_BASE: u32 = ad_memory::CODE_REGION;

/// Sentinel return address. Reaching it means the module returned.
///
/// Odd addresses would fault on a 68000, so this is even; it sits above RAM so
/// no real code can live there.
pub const HOST_RETURN: u32 = ad_memory::HOST_ARENA;

/// Cycles to allow for one call before declaring the module hung.
pub const DEFAULT_CYCLE_BUDGET: u32 = 50_000_000;

/// A [`Host::cycle_budget`] of this value means "never call the module hung".
///
/// The interactive player sets it, and needs it: a game legitimately never
/// returns from `DrawFrame`, so its cycles accumulate inside one call for as long
/// as someone plays. With a finite budget that call is eventually declared a hang
/// — at 8 MHz, `u32::MAX` cycles is about **nine minutes** of Lunatic Fringe —
/// and the accumulator saturating there made the comparison fire regardless of
/// what the budget was set to. A person quitting is what ends a game; the
/// watchdog is for the lab, where nobody is watching.
pub const NO_CYCLE_LIMIT: u32 = u32::MAX;

/// Emulated cycles per 1/60 s tick.
///
/// Models an ~8 MHz 68000, the class of machine these modules were tuned on.
/// Time must advance *during* a call, not just between frames: modules pace
/// themselves by polling `TickCount` or the monitor's VBL `synchFlag` inside
/// `DrawFrame`, and a clock that only moves between calls turns those loops
/// into permanent hangs. Driving ticks from executed cycles keeps time flowing
/// and — because Musashi's cycle counts are deterministic — keeps replays
/// deterministic too.
pub const CYCLES_PER_TICK: u32 = 133_333;

/// Cycles in one tick for `profile`, never zero.
///
/// The rate lives in [`ad_toolbox::profile::MachineProfile::clock_hz`] so the
/// machine's described speed and its actual speed are one value. [`CYCLES_PER_TICK`]
/// remains as the 8 MHz figure this was hard-coded to, for the tests that pin it.
#[must_use]
pub fn cycles_per_tick(profile: &ad_toolbox::profile::MachineProfile) -> u32 {
    (profile.clock_hz / 60).max(1)
}

/// One After Dark extension service: its selector and its method table, each
/// entry a name for diagnostics plus the behaviour to run.
type Service<'a> = ([u8; 4], Vec<(&'a str, ad_toolbox::Callout)>);

/// Maps an RGB triple onto the framebuffer's palette.
///
/// Threaded into the blitter and the `PICT` decoder so both resolve colour the
/// same way.
pub type ColourMapper<'a> = &'a dyn Fn(&mut ad_memory::Memory, [u8; 3]) -> u8;

/// A per-tick presentation callback: the live framebuffer and the tick count.
pub type PresentHook = Box<dyn FnMut(&quickdraw::Framebuffer, u32)>;

/// Polls live keyboard state each tick: `(held key codes, quit requested)`.
pub type KeySource = Box<dyn FnMut() -> (Vec<u8>, bool)>;

/// Polls the live cursor position each tick, in global coordinates as `(h, v)`.
///
/// Optional on purpose. A handful of modules read `_GetMouse` to avoid drawing
/// under the cursor, so an interactive host should install one — but a *moving*
/// mouse makes a run irreproducible, so the compatibility lab installs none and
/// keeps `Toolbox::mouse` at its fixed screen centre.
pub type MouseSource = Box<dyn FnMut() -> (i16, i16)>;

/// Receives everything the module did to the sound hardware, in order.
///
/// The audio analogue of [`PresentHook`], and it exists for the same reason: a
/// game never returns from `DrawFrame`, so a host that only looked at sounds
/// between messages would hear a whole session's effects at once, at the end.
pub type SoundHook = Box<dyn FnMut(&[ad_toolbox::snd::SoundEvent])>;

/// Receives the strings the module drew, in order, on the present schedule.
///
/// The same shape and the same reason as [`SoundHook`]: a game never returns
/// from `DrawFrame`, so a host that read the module's words between messages
/// would see a prompt long after the moment it mattered. See
/// [`Host::drain_drawn_text`] for what a host does with these.
pub type TextHook = Box<dyn FnMut(&[String])>;

/// The After Dark version this host declares, in BCD.
///
/// 2.0 by default because that is the ABI this runtime implements. `AD_VERSION`
/// overrides it — 3.0-era modules read `params->adVersion` and decline below
/// 0x0300, so raising it is how one finds out what they ask for *next*.
fn ad_version() -> u16 {
    std::env::var("AD_VERSION")
        .ok()
        .and_then(|v| u16::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok())
        .unwrap_or(0x0200)
}

/// Byte size of `GMParamBlock`, from `GraphicsModule_Types.h`.
pub const PARAM_BLOCK_SIZE: u32 = 44;

/// Why a module call ended badly.
#[derive(Debug)]
pub enum HostError {
    /// The fork has no `ADgm` resource.
    NoCodeResource,
    /// A compressed resource could not be expanded; see [`decompress`].
    Decompression(String),
    /// The code resource is compressed and this runtime cannot expand it.
    ///
    /// Its own error rather than a trap fault, because the trap fault was a
    /// lie: the first word of a compressed resource is `$A89F`, so handing the
    /// bytes to the CPU reported "unhandled Toolbox trap" for what is really a
    /// packaging format we do not read yet.
    CompressedCode {
        /// Size the code would expand to.
        unpacked_len: u32,
        /// `dcmp` resource that would expand it.
        decompressor_id: i16,
    },
    /// Retired: execution now enters the resource at offset 0, so the entry
    /// stub never needs decoding. Kept so older diagnostics still read.
    UnresolvedEntryPoint,
    /// The CPU stopped for a reason the runtime must fix.
    Cpu(RunError),
    /// The module ran past its cycle budget without returning.
    ///
    /// Carries a CPU snapshot: a hang is almost always a tight loop, and the
    /// disassembly at the stuck PC says immediately which one.
    Hung { cycles: u64, snapshot: String },
    /// The module returned an error selector.
    Module(GmResult),
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCodeResource => write!(f, "module has no ADgm code resource"),
            Self::Decompression(e) => write!(f, "compressed resource: {e}"),
            Self::CompressedCode { unpacked_len, decompressor_id } => write!(
                f,
                "module code is a compressed resource ({unpacked_len} bytes packed, \
                 needs dcmp {decompressor_id}); resource decompression is not implemented"
            ),
            Self::UnresolvedEntryPoint => {
                write!(f, "could not resolve the module's entry point stub")
            }
            Self::Cpu(e) => write!(f, "{e}"),
            Self::Hung { cycles, snapshot } => {
                write!(
                    f,
                    "module did not return within {cycles} cycles\n{snapshot}"
                )
            }
            Self::Module(r) => write!(f, "module returned {r:?}"),
        }
    }
}

impl std::error::Error for HostError {}

impl From<RunError> for HostError {
    fn from(e: RunError) -> Self {
        Self::Cpu(e)
    }
}

/// A module loaded and ready to run.
#[allow(missing_debug_implementations, reason = "wraps the CPU singleton")]
pub struct Host {
    pub tb: Toolbox,
    cpu: Cpu,
    /// Address of the `GMParamBlock`.
    param_block: u32,
    /// Address of the `Handle` variable the module fills in at Initialize.
    storage_var: u32,
    /// `params->errorMessage`, a Str255 the module fills in when it declines.
    error_message: u32,
    /// Address of the first `MonitorData` record, for the VBL `synchFlag`.
    monitor_data: u32,
    /// Cycles accumulated toward the next tick.
    cycle_bank: u32,
    /// Counters for `AD_STATS`: DrawFrame calls, ticks fired, hook invocations.
    pub counters: (u64, u64, u64),
    /// Entry point of the module's `main`, absolute.
    entry: u32,
    /// Cycle budget per call.
    pub cycle_budget: u32,
    /// Scheduled key changes: `(tick, virtual key code, down)`, sorted by tick.
    key_queue: Vec<(u32, u8, bool)>,
    /// Whether `AD_TRACE_EVENT` has already captured a delivery.
    traced_event: bool,
    /// Called every `present_every` ticks with the live framebuffer.
    ///
    /// A game module owns the machine — `DrawFrame` does not return while play
    /// is active — so anything that wants to *see* the game (a window, a frame
    /// dump) must be driven from inside the tick loop. This hook is that seam,
    /// and it is what an interactive host presents from.
    present: Option<PresentHook>,
    /// Tick interval for `present`; 0 disables.
    present_every: u32,
    /// Called with strings the module drew, on the same schedule as `present`.
    text: Option<TextHook>,
    /// Called with new sound events on the same schedule as `present`.
    sound: Option<SoundHook>,
    /// Polled every tick for live keyboard state; see [`KeySource`].
    keys: Option<KeySource>,
    /// Polled every tick for the cursor position; see [`MouseSource`].
    mouse: Option<MouseSource>,
    /// Key codes currently believed held, so edges can be detected.
    held: Vec<u8>,
    /// Set when the key source asks to stop.
    pub quit: bool,
}

impl Host {
    /// Load a module's code and build the host structures around it.
    ///
    /// # Errors
    /// [`HostError::NoCodeResource`] or [`HostError::UnresolvedEntryPoint`].
    pub fn load(fork: ResourceFork<'_>, settings_values: [i16; 4]) -> Result<Self, HostError> {
        let module = AdModule::new(fork);
        let code = module.code().ok_or(HostError::NoCodeResource)?;
        let packed = code.compression();
        // Layout is metadata now; execution never depends on decoding the stub.
        let _layout = CodeLayout::detect(code.data);

        let mut tb = Toolbox::new();
        // A compressed code resource is expanded further down, once the CPU
        // exists to run the module's own decompressor with. Writing the packed
        // bytes here would put a `$A89F` trap word at the entry point — the
        // fault that made seventeen of these modules look like they needed a
        // missing Toolbox service. See `decompress`.
        if packed.is_none() {
            tb.mem.write_bytes(CODE_BASE, code.data);
        }

        // Give the Resource Manager the module's own resources, plus the
        // system resources modules assume the System file provides (KCHR).
        // Module resources come first so they shadow same-typed system ones,
        // which is the real search order: current file before System.
        let mut stored: Vec<ad_toolbox::resources::StoredResource> = module
            .fork()
            .all()
            .iter()
            .map(|r| ad_toolbox::resources::StoredResource {
                res_type: r.res_type,
                id: r.id,
                name: r.name.clone(),
                // Carried, not dropped: the parser preserves both and a store
                // that discards them cannot write a fork back without losing
                // data the original had.
                name_bytes: r.name_bytes.map(<[u8]>::to_vec),
                attrs: r.attrs,
                data: r.data.to_vec(),
            })
            .collect();
        stored.extend(ad_toolbox::resources::system_resources());
        tb.resources = ad_toolbox::ResourceStore::new(stored);
        // Enter the resource at offset 0 — always.
        //
        // Headered modules begin `BRA.S +14`, then the stub
        // `LEA -18(PC),A0 ; BRA.W main`; Think C's OpenGlobals() turns that A0
        // into A4, the globals register everything else is addressed through.
        // An earlier host resolved the stub and jumped straight to `main`,
        // skipping the LEA — so A4 was built from garbage and every
        // `jsr (d16,A4)` leapt into low memory, presenting as 27 unexplained
        // hangs. The module's own entry path is part of the ABI; execute it.
        let entry = CODE_BASE;

        // Host-owned structures live outside the module's heap so a stray write
        // cannot corrupt them.
        let param_block = tb.mem.reserve_host(PARAM_BLOCK_SIZE, "GMParamBlock");
        let storage_var = tb.mem.reserve_host(4, "module storage handle slot");
        let qd_globals = tb.mem.reserve_host(0x100, "QuickDraw globals");
        let monitors = tb.mem.reserve_host(0x40, "monitor list");

        // Every unknown pointer-sized field of the AD info record becomes a
        // callout slot: a module that calls through one produces a named failure
        // ("host callout [info+N] invoked") instead of a wild jump to nil. The
        // version word at +12 stays a version word.
        {
            let info = tb.ad_info;
            let mut off = 0u32;
            while off < ad_toolbox::ad_detect::INFO_SIZE {
                if off == ad_toolbox::ad_detect::INFO_VERSION {
                    off += 2;
                    continue;
                }
                if off % 4 == 0 && off != 12 {
                    let slot = tb.callout_slot(&format!("info+{off}"));
                    tb.mem.write_u32(info.wrapping_add(off), slot);
                    off += 4;
                } else {
                    off += 2;
                }
            }
            // Re-assert the version over whatever the loop wrote near it.
            tb.mem.write_u16(
                info.wrapping_add(ad_toolbox::ad_detect::INFO_VERSION),
                ad_toolbox::ad_detect::AD_VERSION,
            );
        }

        let error_message = Self::build_param_block(
            &mut tb,
            param_block,
            settings_values,
            qd_globals,
            monitors,
        );

        // The module's storage handle starts nil; Initialize allocates it.
        tb.mem.write_u32(storage_var, 0);

        // Point the line-A exception vector at the trap gate, and put an RTE
        // there so the gate returns to the interrupted code after the handler
        // has run. Without this every Toolbox call would vector into nothing.
        tb.mem
            .write_u32(ad_m68k::VECTOR_LINE_A, ad_m68k::TRAP_GATE);
        tb.mem.write_u16(ad_m68k::TRAP_GATE, 0x4E73); // RTE

        // Every *other* exception vector points at a named callout, so an
        // exception says what it was.
        //
        // Unpopulated vectors on a 68000 are address 0, so until now any
        // exception a module took surfaced as "wild jump: executing low memory at
        // 0x000000 (a nil host function pointer, an unbuilt jump table, or a
        // corrupted return address)" — three guesses, none of them the answer.
        // ProtoToasters was filed as a nil function pointer for exactly that
        // reason; it is `divs.w D6,D0` with a zero divisor, and it says so now.
        //
        // A real Mac had handlers here that put up a bomb dialog, so vectoring
        // them somewhere is faithful as well as useful.
        {
            use ad_toolbox::Callout::CpuException;
            // (vector number, what it is, short exception frame)
            const VECTORS: &[(u32, &str, bool)] = &[
                (2, "68000 exception: bus error", false),
                (3, "68000 exception: address error (odd address)", false),
                (4, "68000 exception: illegal instruction", true),
                (5, "68000 exception: divide by zero", true),
                (6, "68000 exception: CHK out of bounds", true),
                (7, "68000 exception: TRAPV overflow", true),
                (8, "68000 exception: privilege violation", true),
                (9, "68000 exception: trace", true),
                (11, "68000 exception: line-F emulator", true),
            ];
            for &(vector, name, short_frame) in VECTORS {
                let slot = tb.callout_slot_kind(name, CpuException { short_frame });
                tb.mem.write_u32(vector * 4, slot);
            }
        }

        // The display starts black, as it did when After Dark took the screen.
        tb.blank_screen();

        // The profile decides, so the CPU we execute as and the CPU we *say* we
        // are cannot drift apart. They were two independent constants before.
        let mut cpu = Cpu::new(tb.profile.cpu.core_type());
        cpu.set_stop_address(Some(HOST_RETURN));
        // Nothing below the code region is ever legitimate code; entering it is
        // a nil function pointer or an unbuilt jump table, and must fail loudly.
        cpu.set_wild_jump_floor(ad_memory::CODE_REGION);
        // Nothing above the host arena is code either. The trap gate and return
        // sentinel live inside the arena, so the ceiling sits past its end.
        cpu.set_wild_jump_ceiling(
            ad_memory::HOST_ARENA.saturating_add(ad_memory::HOST_ARENA_SIZE),
        );

        // Expand a compressed code resource by running the module's own `dcmp`.
        if let Some(header) = packed {
            let dcmp = tb
                .resources
                .find(b"dcmp", header.decompressor_id)
                .map(<[u8]>::to_vec)
                .ok_or(HostError::CompressedCode {
                    unpacked_len: header.unpacked_len,
                    decompressor_id: header.decompressor_id,
                })?;
            let scratch = tb.mem.reserve_host(
                header
                    .unpacked_len
                    .saturating_mul(2)
                    .saturating_add(0x2_0000),
                "resource decompression scratch",
            );
            let packed_bytes = code.data.to_vec();
            let plain = decompress::expand(
                &mut tb, &mut cpu, header, &packed_bytes, &dcmp, scratch,
            )
            .map_err(|e| HostError::Decompression(e.to_string()))?;
            // The expansion must be exactly what the header promised and must
            // look like a module: an `ADgm` opens `BRA.S +14` then `'ADgm'`, or
            // is a bare Pascal prologue. Checked here rather than trusted,
            // because a decompressor given a wrong ABI tends to *return*.
            if plain.len() != header.unpacked_len as usize {
                return Err(HostError::Decompression(format!(
                    "{} bytes, but the header promised {}",
                    plain.len(),
                    header.unpacked_len
                )));
            }
            tb.mem.write_bytes(CODE_BASE, &plain);
            // Restore the execution bounds the expansion had to relax.
            cpu.set_stop_address(Some(HOST_RETURN));
            cpu.set_wild_jump_floor(ad_memory::CODE_REGION);
            cpu.set_wild_jump_ceiling(
                ad_memory::HOST_ARENA.saturating_add(ad_memory::HOST_ARENA_SIZE),
            );
        }

        Ok(Self {
            tb,
            cpu,
            param_block,
            storage_var,
            error_message,
            monitor_data: monitors.wrapping_add(2),
            cycle_bank: 0,
            counters: (0, 0, 0),
            entry,
            cycle_budget: DEFAULT_CYCLE_BUDGET,
            key_queue: Vec::new(),
            traced_event: false,
            present: None,
            text: None,
            present_every: 0,
            sound: None,
            keys: None,
            mouse: None,
            held: Vec::new(),
            quit: false,
        })
    }

    /// Populate `GMParamBlock`. Field offsets come from the SDK header.
    #[allow(clippy::too_many_arguments, reason = "one argument per GMParamBlock region")]
    fn build_param_block(
        tb: &mut Toolbox,
        pb: u32,
        controls: [i16; 4],
        qd_globals: u32,
        monitors: u32,
    ) -> u32 {
        use ad_resource::module::param_block as off;

        // controlValues[4] — the user's slider/checkbox/menu settings, in
        // resource-ID order (id 1000 + i).
        for (i, v) in controls.iter().enumerate() {
            let a = pb
                .wrapping_add(off::CONTROL_VALUES as u32)
                .wrapping_add((i as u32).wrapping_mul(2));
            tb.mem.write_u16(a, *v as u16);
        }

        // MonitorsInfo: one monitor, full screen, 8 bits deep.
        tb.mem.write_u16(monitors, 1); // monitorCount
        let md = monitors.wrapping_add(2);
        quickdraw::Rect::new(
            0,
            0,
            quickdraw::SCREEN_HEIGHT as i16,
            quickdraw::SCREEN_WIDTH as i16,
        )
        .write(&mut tb.mem, md);
        tb.mem.write_u8(md.wrapping_add(8), 0); // synchFlag
        tb.mem.write_u8(md.wrapping_add(9), 8); // curDepth
        tb.mem
            .write_u32(pb.wrapping_add(off::MONITORS as u32), monitors);

        // Color QuickDraw is available, and its Boolean is padded to a word.
        tb.mem
            .write_u16(pb.wrapping_add(off::COLOR_QD_AVAIL as u32), 0x0100);

        // systemConfig. `ExtensionsAvailable` is not optional: Bouncing Ball's own
        // source (reference/sdk/.../Bouncing Ball.c) opens with
        //
        //     if (!(ExtensionsAvailable & params->systemConfig))
        //         return ModuleError;   /* "you need After Dark 2.0u or later" */
        //
        // so leaving it clear makes a module refuse to start for a reason that
        // looks nothing like a missing trap.
        use ad_resource::module::system_config as cfg;
        tb.mem.write_u16(
            pb.wrapping_add(off::SYSTEM_CONFIG as u32),
            cfg::SOUND_AVAILABLE | cfg::EXTENSIONS_AVAILABLE,
        );

        // A read-only copy of the QuickDraw globals. Layout from `QDGlobals` in
        // GraphicsModule_Types.h:
        //   +0   GrafPtr thePort
        //   +4   Pattern white, black, gray, ltGray, dkGray   (5 x 8 = 40)
        //   +44  Cursor  arrow                                (32+32+4 = 68)
        //   +112 BitMap  screenBits  { baseAddr(4) rowBytes(2) bounds(8) }
        //   +126 long    randSeed
        // Filled by `init_graf` — the same routine that answers a module's own
        // `_InitGraf` — because a hand-rolled copy here once wrote screenBits
        // and randSeed and **stopped**, leaving all five patterns zero. Every
        // Pattern is eight zero bytes, and eight zero bytes *is* the white
        // pattern — so `FillRgn(blankRgn, qdGlobalsCopy->qdBlack)`, the blank
        // in Berkeley's own SDK example and the idiom across this disk,
        // painted with the background colour instead of black. Two successive
        // "default port colour" schemes (fore white/back black, then both
        // black) were tuned to make *that* look right, each fixing some
        // modules by breaking others: Say What?'s black-ink quotes vanished
        // into the black its own white-page blank had become. The patterns
        // are data, not policy; with them present the port defaults can be
        // QuickDraw's own.
        tb.mem
            .write_u32(pb.wrapping_add(off::QD_GLOBALS_COPY as u32), qd_globals);
        tb.init_graf(qd_globals);

        tb.mem.write_u16(pb.wrapping_add(off::BRIGHTNESS as u32), 0);
        quickdraw::Rect::default().write(&mut tb.mem, pb.wrapping_add(off::DEMO_RECT as u32));
        // After Dark extensions, with the ABI taken from the modules' own glue
        // (see docs/LEARNINGS.md): `LookUpEntryPoints` walks
        // params->extensions as {u16 count; {OSType sel; u32 entryPoints}[]}
        // and returns `entryPoints` RAW in D0 — it is a direct pointer to a
        // method table ({u16 version; fn ptrs at +2,+6,+10,...}), not a handle.
        // The first attempt added a handle indirection that does not exist, and
        // modules jumped through garbage. The methods are Pascal (callee pops,
        // result in a caller-reserved slot); each is a callout slot whose
        // handler enforces exactly that.
        use ad_toolbox::Callout as C;
        let services: [Service<'_>; 3] = [
            (
                *b"ADmf",
                vec![
                    ("ADmf FindMemory", C::MemFind),
                    ("ADmf ReleaseMemory", C::MemRelease),
                    ("ADmf UseMemory", C::MemUse),
                    ("ADmf RestoreMemory", C::MemRestore),
                ],
            ),
            (
                *b"ADSd",
                vec![
                    ("ADSd OpenSound", C::SndOpen),
                    ("ADSd CloseSound", C::SndClose),
                    ("ADSd PlaySound", C::SndPlay),
                    ("ADSd QuietSound", C::SndQuiet),
                    ("ADSd FlushSound", C::SndFlush),
                    ("ADSd GetSoundLength", C::SndLength),
                    ("ADSd SoundBusy", C::SndBusy),
                ],
            ),
            // No glue symbols exist for 'CCOD'; its methods are discovery slots
            // until real calls name themselves.
            (*b"CCOD", vec![]),
        ];
        let ext_table =
            tb.mem.reserve_host(2 + 8 * services.len() as u32, "extension table");
        tb.mem.write_u16(ext_table, services.len() as u16);
        for (i, (sel, methods)) in services.iter().enumerate() {
            let entry = ext_table + 2 + (i as u32) * 8;
            tb.mem.write_u32(entry, u32::from_be_bytes(*sel));
            let table = tb.mem.reserve_host(2 + 12 * 4, "extension method table");
            tb.mem.write_u16(table, 1); // version word
            for m in 0..12usize {
                let (name, kind) = methods.get(m).cloned().unwrap_or_else(|| {
                    (Box::leak(
                        format!("{} fn@+{}", String::from_utf8_lossy(sel), 2 + m * 4)
                            .into_boxed_str(),
                    ) as &str, C::Discover)
                });
                let slot = tb.callout_slot_kind(name, kind);
                tb.mem.write_u32(table + 2 + (m as u32) * 4, slot);
            }
            tb.mem.write_u32(entry.wrapping_add(4), table);
        }

        // errorMessage must be a real 256-byte Str255, not nil. Modules BlockMove
        // their failure text into it *before* checking anything, so a nil pointer
        // means a 255-byte write to address 0 — over the exception vectors, which
        // then presents as a wild PC rather than a polite refusal.
        let error_message = tb.mem.reserve_host(256, "errorMessage Str255");
        tb.mem
            .write_u32(pb.wrapping_add(off::ERROR_MESSAGE as u32), error_message);
        tb.mem.write_u32(pb.wrapping_add(off::SND_CHANNEL as u32), 0);
        // BCD 2.0x, matching the control panel on the source disk.
        tb.mem
            .write_u16(pb.wrapping_add(off::AD_VERSION as u32), ad_version());
        tb.mem
            .write_u32(pb.wrapping_add(off::EXTENSIONS as u32), ext_table);
        error_message
    }

    /// Install the host's diagnostic switches.
    ///
    /// Nothing in this crate or `ad-toolbox` reads the environment; a caller that
    /// wants logging asks for it. `ad_runtime::RuntimeOptions::from_env` is what
    /// maps `AD_*` variables onto these for the lab.
    pub fn set_diagnostics(&mut self, diag: ad_toolbox::Diagnostics) {
        self.tb.set_diagnostics(diag);
    }

    /// Load every `FONT`/`NFNT` strike in a resource fork the host supplies.
    ///
    /// Returns how many were added. Call before `Initialize`: a module reads
    /// `GetFontInfo` and lays out text from it there, so a font arriving later
    /// would be measured against metrics it never saw.
    ///
    /// `ad_runtime::font_forks` finds the files; this takes bytes, so nothing in
    /// this crate or `ad-toolbox` needs to know what a path is.
    pub fn add_font_fork(&mut self, fork_bytes: &[u8]) -> usize {
        self.tb.qd.fonts.load_fork(fork_bytes)
    }

    /// How many strikes are available to the module.
    #[must_use]
    pub fn font_count(&self) -> usize {
        self.tb.qd.fonts.len()
    }

    /// Merge previously saved resources and install the durable sink.
    ///
    /// **Call before `Initialize`.** A module reads its saved state during
    /// Initialize — Lunatic Fringe loads `LFhs 128` there to build the high-score
    /// table — so merging afterwards would show the shipped defaults for one
    /// session and then overwrite the real scores with them.
    pub fn attach_saved_state(
        &mut self,
        saved: Vec<ad_toolbox::resources::StoredResource>,
        sink: Box<dyn ad_toolbox::resources::ResourceSink>,
    ) {
        for entry in saved {
            self.tb.resources.put(entry);
        }
        self.tb.sink = Some(sink);
    }

    /// Write out anything the module changed but has not asked to save.
    ///
    /// This is `_CloseResFile`: the Resource Manager wrote changed resources when
    /// a file closed, and for this runtime the file closes when the host shuts
    /// down. A module that calls `_UpdateResFile` itself has already saved and
    /// this does nothing.
    ///
    /// # Errors
    /// The sink's message. Reported rather than swallowed: a lost high score with
    /// no explanation is worse than a visible failure.
    pub fn flush_saved_state(&mut self) -> Result<(), String> {
        self.tb.flush_resources_on_close()
    }

    /// The message the module left in `params->errorMessage`.
    ///
    /// Modules write a human-readable reason there before returning
    /// `ModuleError`, so this usually says exactly what they think is missing.
    #[must_use]
    pub fn error_message(&mut self) -> Option<String> {
        let len = usize::from(self.tb.mem.read_u8(self.error_message));
        if len == 0 || len > 255 {
            return None;
        }
        let bytes = self.tb.mem.read_bytes(self.error_message.wrapping_add(1), len);
        Some(ad_resource::macroman::decode(&bytes))
    }

    /// The module's private storage handle, once Initialize has run.
    #[must_use]
    pub fn storage(&mut self) -> u32 {
        self.tb.mem.read_u32(self.storage_var)
    }

    /// Advance the emulated clock by exactly one tick, with everything a tick
    /// entails: input, the VBL flag, and a presented frame.
    ///
    /// # Why this is one function
    ///
    /// It used not to be. Ticks happened in two places — here, from accumulated
    /// cycles, and one per `DrawFrame` **call** at the top of [`Host::draw_frame`]
    /// — and only this one presented. So the clock could advance a hundred times
    /// without a single frame reaching the window.
    ///
    /// That is fine until something paces on the clock. `ad_runtime::Pacer` does:
    /// it holds tick *n* to n/60 of a real second. Given a clock that had jumped
    /// 190 ticks since the last frame it computed a due time three seconds out and
    /// slept there — inside the present hook, which is the window's only path for
    /// redraw and input. Measured on Lunatic Fringe's attract screen: 189 calls,
    /// one presented frame, one 3.17-second sleep. The application looked frozen.
    ///
    /// Removing the per-call tick instead is *also* wrong, and the survey says so:
    /// seven modules stopped drawing entirely. A screen saver is called once per
    /// vertical retrace and paces itself on `TickCount`, so one tick per frame is
    /// the faithful model — with 20-frame runs, dropping it left those modules
    /// waiting for a clock that never moved.
    ///
    /// So both tick sources are correct and both now run through here.
    fn advance_tick(&mut self) -> Result<(), HostError> {
        self.counters.1 += 1;
        self.tb.tick();
        self.deliver_due_keys()?;
        self.poll_keys()?;
        self.poll_mouse();
        if self.quit {
            // End the timeslice so the run loop regains control now rather than
            // after the rest of the 100k-cycle chunk.
            self.cpu.halt();
        }
        if self.present_every > 0 && self.tb.ticks % self.present_every == 0 {
            // Push the module's drawing into the framebuffer first: modules write
            // screen memory directly.
            self.tb.sync_screen();
            let ticks = self.tb.ticks;
            self.counters.2 += 1;
            if let Some(mut f) = self.present.take() {
                f(&self.tb.qd.fb, ticks);
                self.present = Some(f);
            }
            // Sound on the same schedule as the picture. Drained even with no hook
            // installed, so the log cannot grow unbounded in a long session just
            // because nobody is listening.
            let events = self.tb.sounds.drain_new();
            if let Some(mut f) = self.sound.take() {
                f(&events);
                self.sound = Some(f);
            }
        }
        // The module's words, every tick and not on the present schedule.
        //
        // Deliberately not grouped with the picture and the sound above: those
        // are for a host that is *showing* the module, and a host may well want
        // the text without drawing anything — the lab's `AD_TEXT_LOG` installs no
        // present hook at all, and gating this the same way meant it printed
        // nothing. A tick with no text drawn costs an empty `Vec::take`.
        let said = self.tb.qd.drain_drawn_text();
        if !said.is_empty() {
            if let Some(mut f) = self.text.take() {
                f(&said);
                self.text = Some(f);
            }
        }
        // VBL task: set the monitor's synchFlag; modules clear it and wait for the
        // next vertical blank to set it again.
        let md = self.monitor_data;
        self.tb.mem.write_u8(md.wrapping_add(8), 1);
        Ok(())
    }

    /// Send one message to the module and return its result.
    ///
    /// # Errors
    /// [`HostError`] if the CPU faults, a trap is unimplemented, or the module
    /// fails to return within the cycle budget.
    pub fn call(&mut self, message: GmMessage) -> Result<GmResult, HostError> {
        // After Dark made the screen port current before each message. Without
        // this a module that left an offscreen port current during Initialize
        // paints its next frame into that buffer instead of the display.
        let screen_port = self.tb.screen.port;
        self.tb.set_port(screen_port);

        // A vertical retrace has, by definition, occurred by the time the host
        // asks for the next frame, and modules clear this flag then wait for the
        // VBL to set it again — so entering a call with the module's own cleared
        // value is wrong regardless.
        //
        // Recorded honestly: this was hypothesised as the cause of several
        // modules rendering a correct first frame and then never changing, and
        // measurement DISPROVED it. Flying Toasters is still byte-identical at
        // 2, 20 and 60 frames with the flag set. The real cause is elsewhere —
        // its DrawFrame does no drawing at all (identical trap counts whatever
        // the frame count), so the visible image comes from Initialize. That is
        // the top open fidelity bug; see docs/module-findings.md.
        let md = self.monitor_data;
        self.tb.mem.write_u8(md.wrapping_add(8), 1);

        // Fresh stack for every call, below the host structures.
        let mut sp = ad_memory::STACK_TOP;

        // Pascal pushes left to right, so `storage` first and `params` last.
        // The last-pushed argument ends up nearest SP.
        sp = sp.wrapping_sub(4);
        self.tb.mem.write_u32(sp, self.storage_var); // Handle *storage
        sp = sp.wrapping_sub(4);
        self.tb.mem.write_u32(sp, self.tb.qd.blank_rgn); // RgnHandle blankRgn
        sp = sp.wrapping_sub(2);
        self.tb.mem.write_u16(sp, message as i16 as u16); // short message
        sp = sp.wrapping_sub(4);
        self.tb.mem.write_u32(sp, self.param_block); // GMParamBlockPtr params

        // Return address the module's RTS will jump to.
        sp = sp.wrapping_sub(4);
        self.tb.mem.write_u32(sp, HOST_RETURN);

        self.cpu.set_sp(sp);
        self.cpu.set_pc(self.entry);
        // Supervisor mode, interrupts enabled. Modules ran in supervisor state
        // under After Dark, and the A-line exception needs it anyway.
        self.cpu.set_sr(0x2000);

        self.counters.0 += 1;
        // u64: a game runs inside one `DrawFrame` indefinitely, and a u32
        // accumulator saturates after about nine minutes at 8 MHz — which then
        // reads as "used >= budget" no matter how large the budget is.
        let mut used = 0u64;
        loop {
            let chunk = self.cpu.run(&mut self.tb, 100_000)?;
            used = used.saturating_add(u64::from(chunk));
            // Let time flow with execution: advance Ticks and fire the VBL as
            // cycles accumulate, so pacing loops inside a call make progress.
            self.cycle_bank = self.cycle_bank.saturating_add(chunk);
            let per_tick = cycles_per_tick(&self.tb.profile);
            while self.cycle_bank >= per_tick {
                self.cycle_bank -= per_tick;
                self.advance_tick()?;
            }
            if self.cpu.take_stop_hit() {
                break;
            }
            // A game module never returns from DrawFrame, so "the user closed
            // the window" has to unwind the call from outside it. The module's
            // state is abandoned deliberately: Close follows, and a module that
            // owns the machine has no resumable mid-frame point anyway.
            if self.quit {
                self.tb.sync_screen();
                return Ok(GmResult::Ok);
            }
            // Installed VBL tasks whose countdown just expired run here, between
            // chunks, where the CPU context is a real interrupted program. A
            // tick can also expire a task outside any call (draw_frame ticks
            // before entering); those fire on the first chunk boundary.
            self.run_due_vbl_tasks()?;
            let limited = self.cycle_budget != NO_CYCLE_LIMIT;
            if (limited && used >= u64::from(self.cycle_budget)) || chunk == 0 {
                // Sync what the module drew before reporting: for a game
                // running its own loop inside one message, the screen at
                // timeout is the evidence of how far it really got.
                self.tb.sync_screen();
                let snapshot = self.snapshot();
                return Err(HostError::Hung {
                    cycles: used,
                    snapshot,
                });
            }
        }

        // Pascal leaves the 16-bit result where the arguments were.
        let result = self.tb.mem.read_u16(self.cpu.sp()) as i16;
        // The module may have drawn straight to screen memory.
        self.tb.sync_screen();
        Ok(GmResult::from_raw(result))
    }

    /// Run every due VBL task to completion, interrupt-style.
    ///
    /// The Vertical Retrace Manager called tasks from the VBL interrupt with
    /// `A0` pointing at the task record, all registers saved around the call,
    /// and `RTS` returning to the dispatcher. Reproduced here as a nested run:
    /// push a sentinel return address, jump to `vblAddr`, run until the `RTS`
    /// lands on the sentinel, then restore the interrupted register file.
    ///
    /// # Errors
    /// [`HostError`] if a task faults or runs past its budget — a VBL task is
    /// a few hundred cycles of flag-setting, so a million means it's lost.
    fn run_due_vbl_tasks(&mut self) -> Result<(), HostError> {
        while let Some(task) = {
            let due = &mut self.tb.vbl_due;
            if due.is_empty() { None } else { Some(due.remove(0)) }
        } {
            let addr = self
                .tb
                .mem
                .read_u32(task.wrapping_add(ad_toolbox::vbl::VBL_ADDR));
            if addr == 0 {
                continue;
            }
            if self.tb.diag.qd_log {
                eprintln!("[vbl] fire task={task:#x} addr={addr:#x}");
            }
            let saved = self.cpu.save_regs();
            let sp = self.cpu.sp().wrapping_sub(4);
            self.tb.mem.write_u32(sp, HOST_RETURN);
            self.cpu.set_sp(sp);
            self.cpu.set_addr(0, task);
            self.cpu.set_pc(addr);
            self.cpu.set_sr(0x2000);

            let mut used = 0u64;
            loop {
                let chunk = self.cpu.run(&mut self.tb, 20_000)?;
                used = used.saturating_add(u64::from(chunk));
                if self.cpu.take_stop_hit() {
                    break;
                }
                if used >= 1_000_000 || chunk == 0 {
                    let snapshot = self.snapshot();
                    return Err(HostError::Hung {
                        cycles: used,
                        snapshot,
                    });
                }
            }
            self.cpu.restore_regs(&saved);
        }
        Ok(())
    }

    /// `Initialize`, then `Blank`, which is the sequence After Dark sends when a
    /// module starts.
    ///
    /// # Errors
    /// [`HostError`] from either call.
    pub fn start(&mut self) -> Result<GmResult, HostError> {
        let r = self.call(GmMessage::Initialize)?;
        if r != GmResult::Ok {
            return Err(HostError::Module(r));
        }
        self.call(GmMessage::Blank)
    }

    /// Draw one frame.
    ///
    /// # Errors
    /// [`HostError`] if the call fails.
    /// Send one `DrawFrame`.
    ///
    /// # Errors
    /// As [`Host::call`].
    ///
    /// # The clock is not advanced here
    ///
    /// This used to begin `self.tb.tick()`, one tick per *call*, left over from
    /// before time was driven by executed cycles. Once the run loop started
    /// ticking from `cycle_bank` that became a double count, and it was not
    /// harmless in either direction.
    ///
    /// It made `TickCount` a function of how often the host happened to call the
    /// module rather than of work done — so a module that paces itself on ticks
    /// saw time run at a rate the emulated machine had nothing to do with.
    ///
    /// And it broke the interactive player badly. A module in an idle state
    /// returns from `DrawFrame` almost immediately, so the host calls it hundreds
    /// of times per real tick; the clock then advanced hundreds of ticks per tick
    /// of emulated time. `ad_runtime::Pacer` paces on that number, computed a due
    /// time seconds into the future, and slept there — inside the present hook,
    /// which is the window's only path for input and redraw. Measured on Lunatic
    /// Fringe's attract screen: 189 calls, 1 genuine tick, and a single 3.17-second
    /// sleep. That is what "very very laggy" was.
    pub fn draw_frame(&mut self) -> Result<GmResult, HostError> {
        // The vertical retrace that precedes the frame. Through `advance_tick`,
        // so it presents and polls input like every other tick.
        self.advance_tick()?;
        self.call(GmMessage::DrawFrame)
    }

    /// Turn on PC tracing for the next call.
    pub fn set_trace(&mut self, len: usize) {
        self.cpu.set_trace(len);
    }

    /// Disassemble the tail of the PC trace, for diagnosing a refusal.
    pub fn trace_tail(&mut self, count: usize) -> String {
        use std::fmt::Write as _;
        let pcs = self.cpu.trace();
        let start = pcs.len().saturating_sub(count);
        let mut out = String::new();
        for pc in pcs.iter().skip(start) {
            let (text, _) = self.cpu.disassemble(&mut self.tb, *pc);
            let _ = writeln!(
                out,
                "    +{:#06x}  {text}",
                pc.wrapping_sub(CODE_BASE)
            );
        }
        out
    }

    /// Raw bytes of the storage variable, for diagnosis.
    #[must_use]
    pub fn storage_debug(&mut self) -> String {
        let v = self.tb.mem.read_u32(self.storage_var);
        let block = self.tb.mem.deref_handle(v);
        format!(
            "storage_var@{:#x} = {:#x}  deref={:?}  live_handles={}",
            self.storage_var,
            v,
            block,
            self.tb.mem.live_handles()
        )
    }

    /// Addresses of the host-owned structures, for diagnosis.
    #[must_use]
    pub fn layout(&self) -> String {
        format!(
            "param_block={:#x} storage_var={:#x} screen={:#x} blankRgn={:#x} entry={:#x}",
            self.param_block, self.storage_var, self.tb.qd.screen_base, self.tb.qd.blank_rgn,
            self.entry
        )
    }

    /// A human-readable CPU snapshot for diagnosing a hang or fault.
    pub fn snapshot(&mut self) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let pc = self.cpu.pc();
        let _ = writeln!(
            s,
            "  PC {:#010x} (code +{:#06x})  SP {:#010x}  SR {:#06x}",
            pc,
            pc.wrapping_sub(CODE_BASE),
            self.cpu.sp(),
            self.cpu.sr()
        );
        for row in 0..2u8 {
            let mut line = String::from("  ");
            for i in 0..4u8 {
                let n = row * 4 + i;
                let _ = write!(line, "D{n}={:08x} ", self.cpu.data(n));
            }
            let _ = writeln!(s, "{line}");
        }
        for row in 0..2u8 {
            let mut line = String::from("  ");
            for i in 0..4u8 {
                let n = row * 4 + i;
                let _ = write!(line, "A{n}={:08x} ", self.cpu.addr(n));
            }
            let _ = writeln!(s, "{line}");
        }
        let _ = writeln!(s, "  disassembly at PC:");
        let mut at = pc;
        for _ in 0..8 {
            let (text, len) = self.cpu.disassemble(&mut self.tb, at);
            let _ = writeln!(s, "    {:#010x} (+{:#06x})  {text}", at, at.wrapping_sub(CODE_BASE));
            if len == 0 {
                break;
            }
            at = at.wrapping_add(len);
        }
        s
    }

    /// The framebuffer, which is the canonical output.
    #[must_use]
    pub fn framebuffer(&self) -> &quickdraw::Framebuffer {
        &self.tb.qd.fb
    }

    /// Install a live keyboard source, polled once per tick.
    ///
    /// Edges are what matter: a key that is newly held goes down (KeyMap bit
    /// set, plus a `keyDown` event for non-modifiers), and one that is no
    /// longer held goes up. Re-posting a held key every tick would flood a
    /// module's event path with autorepeat it never asked for.
    pub fn set_key_source(&mut self, f: KeySource) {
        self.keys = Some(f);
    }

    /// Install a per-tick presentation hook. See the `present` field.
    ///
    /// `every` is in ticks (60ths of a second); 1 presents every tick.
    pub fn set_present_hook(
        &mut self,
        every: u32,
        f: PresentHook,
    ) {
        self.present_every = every;
        self.present = Some(f);
    }

    /// Every sound the module played, in order — the audio analogue of the
    /// framebuffer. An interactive host drains this into its mixer.
    #[must_use]
    pub fn played_sounds(&self) -> Vec<&ad_toolbox::snd::PlayEvent> {
        self.tb.sounds.plays().collect()
    }

    /// Install a cursor-position source; see [`MouseSource`].
    pub fn set_mouse_source(&mut self, source: MouseSource) {
        self.mouse = Some(source);
    }

    /// Strings the module has drawn since this was last called.
    ///
    /// What a module writes on screen is the only account it gives of what it
    /// wants *from the user*, and for one case that is load-bearing rather than
    /// cosmetic: Lunatic Fringe's "Enter your name:" is the moment its key
    /// polling stops meaning "fly" and starts meaning "type". Nothing else
    /// distinguishes those — name entry runs inside the same never-returning
    /// `DrawFrame` as the game and reads the same `KeyMap` — so a host that
    /// remaps keys for playability has to watch the words to know when to stop.
    ///
    /// Drained, and bounded whether or not anyone drains it; see
    /// [`quickdraw::TEXT_LOG_LINES`].
    pub fn drain_drawn_text(&mut self) -> Vec<String> {
        self.tb.qd.drain_drawn_text()
    }

    /// Install a hook for the module's drawn text; see [`TextHook`].
    pub fn set_text_hook(&mut self, hook: TextHook) {
        self.text = Some(hook);
    }

    /// Install a hook for sound events; see [`SoundHook`].
    ///
    /// Called on the same schedule as the present hook, so audio and picture stay
    /// together inside a `DrawFrame` that never returns.
    pub fn set_sound_hook(&mut self, hook: SoundHook) {
        self.sound = Some(hook);
    }

    /// The whole ordered sound-event log: plays and stops, with their ticks.
    ///
    /// What the lab renders through the mixer to produce an artefact that can be
    /// listened to. The per-sound WAVs show the decoder is right; only the mixed
    /// render shows the *path* is.
    #[must_use]
    pub fn sound_log(&self) -> &[ad_toolbox::snd::SoundEvent] {
        self.tb.sounds.log()
    }

    /// Plays not yet handed to an output device, in order.
    ///
    /// An interactive host calls this from the present hook: a game never returns
    /// from `DrawFrame`, so anything that wants to *hear* the game has to be
    /// driven from inside the tick loop, exactly as the window is.
    pub fn drain_new_sounds(&mut self) -> Vec<ad_toolbox::snd::SoundEvent> {
        self.tb.sounds.drain_new()
    }

    /// Press or release a key, by classic Mac virtual key code.
    ///
    /// Writes the low-memory `KeyMap` at `$174`: byte `$174 + (code >> 3)`,
    /// bit `code & 7`. That layout isn't from documentation — it's what
    /// Lunatic Fringe's own polling code does (`and.l #$2, $178.w` for Caps
    /// Lock 0x39, `and.l #$8000, $178.w` for Command 0x37). Games of this era
    /// read the map directly instead of calling `_GetKeys`, so keeping the map
    /// current IS the keyboard input path. Caps Lock (0x39) is the After Dark
    /// convention for "the user wants to play, stop treating keys as wake-ups".
    pub fn set_key(&mut self, code: u8, down: bool) {
        let at = globals::KEY_MAP + u32::from(code >> 3);
        let bit = 1u8 << (code & 7);
        let cur = self.tb.mem.read_u8(at);
        let new = if down { cur | bit } else { cur & !bit };
        self.tb.mem.write_u8(at, new);
    }

    /// Schedule a key change for an absolute tick, deliverable mid-call.
    ///
    /// A game that owns the machine (Lunatic Fringe never returns from
    /// `DrawFrame` while playing) can only receive input from inside the
    /// cycle-driven tick loop — frame boundaries never come. Events fire in
    /// the order queued once `ticks` passes their time.
    pub fn queue_key(&mut self, at_tick: u32, code: u8, down: bool) {
        self.key_queue.push((at_tick, code, down));
        self.key_queue.sort_by_key(|&(t, _, _)| t);
    }

    /// Poll the live key source and apply the edges.
    /// Copy the live cursor position into the Toolbox, if a host supplies one.
    fn poll_mouse(&mut self) {
        let Some(mut src) = self.mouse.take() else {
            return;
        };
        let (h, v) = src();
        self.mouse = Some(src);
        self.tb.mouse = (h, v);
        // `_GetMouse` and every `EventRecord.where` read this one field, so there
        // is nowhere else to keep the two views in step.
    }

    fn poll_keys(&mut self) -> Result<(), HostError> {
        let Some(mut src) = self.keys.take() else {
            return Ok(());
        };
        let (now, quit) = src();
        self.keys = Some(src);
        if quit {
            self.quit = true;
        }
        let previously = std::mem::take(&mut self.held);
        for code in &now {
            if !previously.contains(code) {
                self.press(*code, true)?;
            }
        }
        for code in &previously {
            if !now.contains(code) {
                self.press(*code, false)?;
            }
        }
        self.held = now;
        Ok(())
    }

    /// Set a key's state and, for non-modifiers, post the matching event.
    fn press(&mut self, code: u8, down: bool) -> Result<(), HostError> {
        self.set_key(code, down);
        // Modifier keys never generated keyDown/keyUp on a real Mac; they only
        // appear in the KeyMap and in an event's modifiers field.
        if !matches!(code, 0x37..=0x3B) {
            let what = if down { 3 } else { 4 };
            let ch = ad_toolbox::resources::us_char_for(code);
            let message = (u32::from(code) << 8) | u32::from(ch);
            self.post_event(what, message)?;
        }
        Ok(())
    }

    /// Apply every queued key whose tick has arrived.
    fn deliver_due_keys(&mut self) -> Result<(), HostError> {
        while let Some(&(t, code, down)) = self.key_queue.first() {
            if t > self.tb.ticks {
                break;
            }
            self.key_queue.remove(0);
            self.set_key(code, down);
            // Ordinary keys also arrive as keyDown/keyUp events. Modifier
            // keys never did on a real Mac — they are KeyMap/modifiers-only.
            if !matches!(code, 0x37..=0x3B) {
                let what = if down { 3 } else { 4 }; // keyDown / keyUp
                let ch = ad_toolbox::resources::us_char_for(code);
                let message = (u32::from(code) << 8) | u32::from(ch);
                self.post_event(what, message)?;
            }
        }
        Ok(())
    }

    /// Deliver one event the way the keyboard driver did: by calling
    /// `_PostEvent`'s trap vector at interrupt time.
    ///
    /// Lunatic Fringe never polls for game keys — it patches `_PostEvent` at
    /// game start and reads keyDown/keyUp out of the calls as they happen, so
    /// a host that only maintains the KeyMap sends it no input at all. The
    /// patch is run like a VBL task: full register save, A0 = event number,
    /// D0 = message, and it either `RTS`es or chains to the old vector (a real
    /// `RTS` in the trap table).
    fn post_event(&mut self, what: u16, message: u32) -> Result<(), HostError> {
        let Some(&hook) = self.tb.trap_patches.get(&0xA02F) else {
            if self.tb.diag.qd_log {
                eprintln!("[evt] what={what} message={message:#x}: no _PostEvent patch installed");
            }
            return Ok(()); // nothing installed; KeyMap state is enough
        };
        if self.tb.diag.qd_log {
            let mut d = |o: i32| self.tb.mem.read_u32(hook.wrapping_add(o as u32));
            let (f12, f10, f8, f4) = (d(-12), d(-10), d(-8), d(-4));
            eprintln!(
                "[evt] what={what} message={message:#x} -> hook {hook:#x} \
                 fields[-c]={f12:#x} [-a]={f10:#x} [-8]={f8:#x} [-4]={f4:#x}"
            );
        }
        let saved = self.cpu.save_regs();
        let sp = self.cpu.sp().wrapping_sub(4);
        self.tb.mem.write_u32(sp, HOST_RETURN);
        self.cpu.set_sp(sp);
        self.cpu.set_addr(0, u32::from(what));
        self.cpu.set_data(0, message);
        self.cpu.set_pc(hook);
        self.cpu.set_sr(0x2000);
        // `Diagnostics::trace_event` traces one delivery end to end. A patched
        // trap runs through the module's own glue and segment loader, so the only
        // reliable way to see where it lands is to watch it.
        let trace_this = self.tb.diag.trace_event && !self.traced_event;
        if trace_this {
            self.traced_event = true;
            self.cpu.set_trace(3000);
        }
        let mut used = 0u64;
        loop {
            let chunk = self.cpu.run(&mut self.tb, 20_000)?;
            used = used.saturating_add(u64::from(chunk));
            if self.cpu.take_stop_hit() {
                break;
            }
            if used >= 1_000_000 || chunk == 0 {
                let snapshot = self.snapshot();
                return Err(HostError::Hung {
                    cycles: used,
                    snapshot,
                });
            }
        }
        if trace_this {
            eprintln!("[evt trace]\n{}", self.trace_tail(300));
            self.cpu.set_trace(0);
        }
        self.cpu.restore_regs(&saved);
        Ok(())
    }

    /// Read the four-character magic a module stamps at the head of its storage.
    ///
    /// Modules verify this on every non-Initialize call and bail with
    /// `ModuleError` if it is wrong, which makes it a free check that our
    /// storage marshalling is correct.
    #[must_use]
    pub fn storage_magic(&mut self) -> Option<[u8; 4]> {
        let h = self.storage();
        let block = self.tb.mem.deref_handle(h)?;
        let v = self.tb.mem.read_u32(block);
        Some(v.to_be_bytes())
    }
}
