//! Motorola 68000 execution for the After Dark runtime.
//!
//! Backed by Musashi (Karl Stenerud, MIT), vendored under `vendor/` with its
//! licence text. The C core is reached only through this module, so it can be
//! swapped for another implementation (Moira, `r68k`, a Rust core) without
//! touching the rest of the runtime.
//!
//! # A-line traps
//!
//! Macintosh Toolbox calls are `$Axxx` opcodes. Those raise the "line 1010
//! emulator" exception: the CPU pushes SR and the address *of the trap word
//! itself* and jumps through vector 10. A 68000 pushes exactly that, six bytes;
//! a 68010 and later append a format word, which the gate round-trips. See
//! [`CpuType::exception_frame_size`].
//!
//! Musashi has no callback for that exception, so the runtime installs a **trap
//! gate**:
//!
//! 1. Vector 10 points at [`TRAP_GATE`], a reserved address holding a single
//!    `RTE`.
//! 2. Musashi's instruction hook fires with `pc == TRAP_GATE`. The hook reads
//!    the stacked SR and address, and fetches the trap word.
//! 3. **The hook pops the exception frame before dispatching.** This matters:
//!    Toolbox traps take their arguments on the stack, and the exception frame
//!    sits *below* those arguments. A handler that read from SP directly would
//!    take the SR word as its first argument and write its result over the
//!    frame — corrupting the very SR the `RTE` restores.
//! 4. The handler runs with SP pointing at the real arguments, and adjusts SP
//!    normally (popping arguments, filling the result slot).
//! 5. The hook pushes a fresh frame at the handler's new SP, with the return
//!    address advanced *past* the trap word — otherwise the `RTE` would land on
//!    the trap again and loop forever.
//! 6. The `RTE` executes, restoring SR and resuming after the trap.
//!
//! # Single instance
//!
//! Musashi keeps its CPU state in C globals, so there is one CPU per process.
//! That matches the design — one module per player process — and [`Cpu`]
//! enforces it at construction.

use std::cell::Cell;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};

mod ffi;

pub use ffi::CpuType;

/// Address of the trap gate.
///
/// Above emulated RAM but inside the 68000's 24-bit address space, and inside the
/// reserved prefix of `ad_memory::HOST_ARENA` so it is backed by real storage and
/// can never be handed out by an allocator. The two constants must agree; there
/// is a test for it.
pub const TRAP_GATE: u32 = 0x00A0_0100;

/// Exception vector 10 ("line 1010 emulator") lives at address `0x28`.
pub const VECTOR_LINE_A: u32 = 10 * 4;

/// Size of a 68000 group-2 exception frame: SR word plus PC long.
pub const EXCEPTION_FRAME_SIZE: u32 = 6;

/// Size of a 68010-and-later short exception frame.
///
/// Every 68010+ frame carries a trailing **format word**: bits 15–12 select the
/// frame layout and bits 11–0 hold the vector offset. `RTE` reads it to decide
/// how much to pop, so the gate must preserve it rather than assume six bytes.
///
/// This is the whole reason the first attempt at a 68020 sent 54 of 66 modules
/// into wild jumps: the gate read the PC from the right place, popped two bytes
/// too few, and the `RTE` then resumed through a word of the caller's stack.
pub const EXCEPTION_FRAME_SIZE_68010: u32 = 8;

/// What the memory bus does when the CPU touches an address.
///
/// All accesses are big-endian; the 68000 has no other mode. Implementations
/// must never panic: an out-of-range access is a module bug to be *reported*,
/// so return a defined value and record the fault.
pub trait Bus {
    fn read_u8(&mut self, addr: u32) -> u8;
    fn read_u16(&mut self, addr: u32) -> u16;
    fn read_u32(&mut self, addr: u32) -> u32;
    fn write_u8(&mut self, addr: u32, value: u8);
    fn write_u16(&mut self, addr: u32, value: u16);
    fn write_u32(&mut self, addr: u32, value: u32);

    /// Service an A-line Toolbox trap.
    ///
    /// `trap` is the full opcode word (e.g. `0xA861` for `_Random`). `regs`
    /// gives read/write access to the CPU while the trap runs — Toolbox calls
    /// pass arguments on the stack and return results in `D0` or on the stack.
    ///
    /// Returning `Err` stops execution with [`RunError::UnhandledTrap`]; that is
    /// the intended behaviour for a trap the runtime does not implement yet.
    /// Never silently ignore one — a missing trap that returns garbage produces
    /// a subtly wrong render instead of a diagnosable failure.
    fn trap(&mut self, trap: u16, regs: &mut dyn Registers) -> Result<(), TrapError>;
}

/// CPU register access handed to a trap handler.
pub trait Registers {
    fn data(&self, n: u8) -> u32;
    fn set_data(&mut self, n: u8, value: u32);
    fn addr(&self, n: u8) -> u32;
    fn set_addr(&mut self, n: u8, value: u32);
    fn sp(&self) -> u32;
    fn set_sp(&mut self, value: u32);
    /// Address of the trap instruction currently being serviced.
    fn trap_pc(&self) -> u32;

    /// Resume somewhere other than after the trap word.
    ///
    /// Exists for the Toolbox **auto-pop** convention: a trap word with bit 10
    /// set is called from glue that has pushed the caller's return address on
    /// top of the arguments, and expects the trap to return straight to the
    /// caller. Resuming after the trap word instead lands in the glue's own
    /// dispatch table, which branches back into the stub — forever.
    ///
    /// Mountains is the module that proves it exists: its Think C glue reaches
    /// `_ColorUtilities` as `$AC2E`.
    fn set_resume_pc(&mut self, pc: u32);

    /// What [`Self::set_resume_pc`] was given, if anything.
    fn resume_pc(&self) -> Option<u32>;

    /// Replace the condition codes the `RTE` will restore.
    ///
    /// The trap gate preserves the stacked SR verbatim, which is right for almost
    /// every trap: a Toolbox call is not supposed to disturb the caller's flags.
    /// SANE's comparisons are the exception. `FOCMP` and `FOCPX` answer *only*
    /// through the CCR — the caller's next instruction is a `bgt`, `blt` or `beq`
    /// — so a handler that computes the ordering and leaves the flags alone has
    /// not answered at all.
    ///
    /// SunBurst is the module that proves it: `while (angle > limit) angle -=
    /// step`, with the comparison in `FP68K` and the branch immediately after.
    /// With stale flags saying "greater" the loop never terminates, which read as
    /// a hang half a million FP68K calls deep.
    ///
    /// Only the low five bits (X, N, Z, V, C) are taken; the supervisor half of
    /// the SR is not a trap handler's business.
    fn set_condition_codes(&mut self, ccr: u8);

    /// What [`Self::set_condition_codes`] was given, if anything.
    fn condition_codes(&self) -> Option<u8>;
}

/// Condition-code bit positions in the low byte of the SR.
pub mod ccr {
    pub const CARRY: u8 = 1 << 0;
    pub const OVERFLOW: u8 = 1 << 1;
    pub const ZERO: u8 = 1 << 2;
    pub const NEGATIVE: u8 = 1 << 3;
    pub const EXTEND: u8 = 1 << 4;
    /// Mask of everything a trap handler may set.
    pub const MASK: u8 = 0x1F;
}

/// A trap the runtime could not service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrapError {
    pub trap: u16,
    pub detail: String,
}

impl TrapError {
    pub fn unimplemented(trap: u16) -> Self {
        Self {
            trap,
            detail: "not implemented".into(),
        }
    }
}

/// Why [`Cpu::run`] stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// The PC entered low memory, where no code can legitimately live.
    ///
    /// This is what calling a nil function pointer looks like: the CPU lands at
    /// (or near) address 0 and walks through zeroed RAM executing `ori.b #0,D0`
    /// until the cycle budget dies. Twenty of the first 27 unexplained hangs were
    /// exactly this. Faulting immediately turns a 50-million-cycle silence into a
    /// diagnosis, with the PC trace still warm enough to show the jump site.
    WildJump { pc: u32 },
    /// A Toolbox trap is not implemented. Carries everything needed to go and
    /// implement it.
    UnhandledTrap {
        trap: u16,
        pc: u32,
        detail: String,
    },
    /// The CPU executed an illegal instruction.
    IllegalInstruction { opcode: u16, pc: u32 },
    /// The module ran longer than the caller allowed.
    CycleLimit,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WildJump { pc } => write!(
                f,
                "wild jump: executing low memory at {pc:#08x} (a nil host function pointer, \
                 an unbuilt jump table, or a corrupted return address)"
            ),
            Self::UnhandledTrap { trap, pc, detail } => write!(
                f,
                "unhandled Toolbox trap ${trap:04X} at PC {pc:#08x}: {detail}"
            ),
            Self::IllegalInstruction { opcode, pc } => {
                write!(f, "illegal instruction {opcode:#06x} at PC {pc:#08x}")
            }
            Self::CycleLimit => write!(f, "cycle limit reached"),
        }
    }
}

impl std::error::Error for RunError {}

/// Only one Musashi CPU may exist per process.
static CPU_TAKEN: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// Bus in use for the duration of a `run` call — the type-erased pointer
    /// and its vtable as **one** value, because the C callbacks consult this on
    /// every memory access and each thread-local is a separate `tlv_get_addr`
    /// at runtime. Null pointer means no bus is active.
    static ACTIVE: Cell<(*mut (), Option<&'static BusVTable>)> =
        const { Cell::new((std::ptr::null_mut(), None)) };
    /// Set when a trap handler failed, so `run` can report it.
    static PENDING_FAULT: Cell<Option<RunError>> = const { Cell::new(None) };
    /// Everything [`ad_m68k_instr_hook`] consults, in **one** thread-local.
    ///
    /// The hook runs before every emulated instruction — about a million times
    /// an emulated second — and each `thread_local!` it touches is a separate
    /// `tlv_get_addr` call at runtime. As five separate statics this hook was
    /// the single hottest symbol in a profile of Lunatic Fringe, ahead of the
    /// CPU core itself, and the cost scales with instructions per frame — which
    /// is why crowded scenes (the home base, the enemy spawner) dragged and
    /// empty space did not. One struct, one lookup.
    static HOOK: HookState = const { HookState::new() };
    /// Set when the stop address was reached.
    ///
    /// Musashi runs the hook *before* fetching, then executes the instruction
    /// anyway — `end_timeslice` only shortens the loop. So the PC has already
    /// moved past the sentinel by the time `run` returns, and testing it would
    /// silently never match. This flag is the reliable signal.
    static STOP_HIT: Cell<bool> = const { Cell::new(false) };
}

/// What the per-instruction fast path decided; acted on outside the `with` so
/// the thread-local borrow is over before `end_timeslice` re-enters anything.
enum HookOutcome {
    Run,
    Stop,
    Wild,
}

/// See the `HOOK` thread-local for why these live together.
struct HookState {
    /// PCs below this bound fault as wild jumps when nonzero.
    wild_floor: Cell<u32>,
    /// PCs at or above this bound fault as wild jumps when nonzero.
    ///
    /// A jump through a corrupted pointer lands as readily above the address
    /// space as below it, and the symptom is identical: thousands of cycles
    /// executing whatever unmapped reads return.
    wild_ceil: Cell<u32>,
    /// Bytes in the exception frame the selected CPU pushes. Set by construction.
    frame_size: Cell<u32>,
    /// Address at which execution should stop, if any.
    ///
    /// The host pushes a sentinel return address before calling a module; when
    /// the module's final `RTS` lands there the call is over. Stopping in the
    /// instruction hook means we never execute whatever happens to be at that
    /// address, so the sentinel does not have to be real memory.
    stop_addr: Cell<Option<u32>>,
    /// Whether `pc_trace` currently holds a live ring, mirrored out of the
    /// `RefCell` so the common no-trace case never pays for a borrow.
    tracing: Cell<bool>,
    /// Ring buffer of recently executed PCs, when tracing is on.
    ///
    /// A module that returns `ModuleError` has usually taken a branch we cannot
    /// see from the trap log, because the deciding instruction was a comparison
    /// rather than a call. The last few dozen PCs point straight at it.
    pc_trace: std::cell::RefCell<Option<(Vec<u32>, usize)>>,
}

impl HookState {
    const fn new() -> Self {
        Self {
            wild_floor: Cell::new(0),
            wild_ceil: Cell::new(0),
            frame_size: Cell::new(EXCEPTION_FRAME_SIZE),
            stop_addr: Cell::new(None),
            tracing: Cell::new(false),
            pc_trace: std::cell::RefCell::new(None),
        }
    }
}

/// Manual vtable so the C callbacks can reach a `Bus` without generics.
struct BusVTable {
    read_u8: fn(*mut (), u32) -> u8,
    read_u16: fn(*mut (), u32) -> u16,
    read_u32: fn(*mut (), u32) -> u32,
    write_u8: fn(*mut (), u32, u8),
    write_u16: fn(*mut (), u32, u16),
    write_u32: fn(*mut (), u32, u32),
    trap: fn(*mut (), u16, &mut dyn Registers) -> Result<(), TrapError>,
}

fn vtable_for<B: Bus>() -> &'static BusVTable {
    // One monomorphised vtable per bus type, promoted to 'static.
    &BusVTable {
        read_u8: |p, a| unsafe { (*p.cast::<B>()).read_u8(a) },
        read_u16: |p, a| unsafe { (*p.cast::<B>()).read_u16(a) },
        read_u32: |p, a| unsafe { (*p.cast::<B>()).read_u32(a) },
        write_u8: |p, a, v| unsafe { (*p.cast::<B>()).write_u8(a, v) },
        write_u16: |p, a, v| unsafe { (*p.cast::<B>()).write_u16(a, v) },
        write_u32: |p, a, v| unsafe { (*p.cast::<B>()).write_u32(a, v) },
        trap: |p, t, r| unsafe { (*p.cast::<B>()).trap(t, r) },
    }
}

/// Live register access backed by the C core.
struct LiveRegs {
    trap_pc: u32,
    /// Where an auto-pop trap asked to resume; `None` means "after the word".
    resume: Option<u32>,
    /// Condition codes a handler asked to publish; `None` preserves the caller's.
    ccr: Option<u8>,
}

impl Registers for LiveRegs {
    fn data(&self, n: u8) -> u32 {
        ffi::get_data_reg(n)
    }
    fn set_data(&mut self, n: u8, value: u32) {
        ffi::set_data_reg(n, value);
    }
    fn addr(&self, n: u8) -> u32 {
        ffi::get_addr_reg(n)
    }
    fn set_addr(&mut self, n: u8, value: u32) {
        ffi::set_addr_reg(n, value);
    }
    fn sp(&self) -> u32 {
        ffi::get_sp()
    }
    fn set_sp(&mut self, value: u32) {
        ffi::set_sp(value);
    }
    fn trap_pc(&self) -> u32 {
        self.trap_pc
    }
    fn set_resume_pc(&mut self, pc: u32) {
        self.resume = Some(pc);
    }
    fn resume_pc(&self) -> Option<u32> {
        self.resume
    }
    fn set_condition_codes(&mut self, c: u8) {
        self.ccr = Some(c & ccr::MASK);
    }
    fn condition_codes(&self) -> Option<u8> {
        self.ccr
    }
}

/// The 68000.
#[derive(Debug)]
pub struct Cpu {
    _private: (),
    /// Makes `Cpu` **`Send` but not `Sync`**.
    ///
    /// `!Sync` is required: Musashi keeps the register file in C globals, so two
    /// threads driving one `Cpu` concurrently would corrupt each other.
    ///
    /// `Send` is deliberately *kept*. Moving a `Cpu` to another thread and using
    /// it exclusively there is sound — `run` installs the bus into the calling
    /// thread's local on every call — and it is what lets a caller wrap one in a
    /// `Mutex` to serialise access, which is precisely the exclusion `!Sync`
    /// asks for. (The audit recommended `!Send` too; that would forbid
    /// `Mutex<Cpu>`, whose whole purpose is to make sharing safe. This crate's
    /// own test harness does exactly that.)
    ///
    /// `Cell<()>` is the standard marker for "Send, not Sync".
    _not_sync: PhantomData<std::cell::Cell<()>>,
}

/// Why a `Cpu` could not be created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuAlreadyTaken;

impl std::fmt::Display for CpuAlreadyTaken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "a Cpu already exists in this process; Musashi keeps its state in C \
             globals, so run each module in its own worker process"
        )
    }
}

impl std::error::Error for CpuAlreadyTaken {}

impl Cpu {
    /// Create the process's CPU, or report that one already exists.
    ///
    /// # Errors
    /// [`CpuAlreadyTaken`] if a `Cpu` is live. Musashi's state is in C globals,
    /// so a second instance would silently share registers and memory with the
    /// first. Callers that can recover — a library GUI opening a preview
    /// alongside a running module — need the error, not a panic.
    pub fn try_new(cpu_type: CpuType) -> Result<Self, CpuAlreadyTaken> {
        if CPU_TAKEN.swap(true, Ordering::SeqCst) {
            return Err(CpuAlreadyTaken);
        }
        ffi::init();
        ffi::set_cpu_type(cpu_type);
        ffi::set_instr_hook();
        HOOK.with(|h| h.frame_size.set(cpu_type.exception_frame_size()));
        Ok(Self {
            _private: (),
            _not_sync: PhantomData,
        })
    }

    /// Create the process's CPU.
    ///
    /// # Panics
    /// If a `Cpu` already exists. Prefer [`Self::try_new`] anywhere the caller
    /// could plausibly recover.
    #[must_use]
    pub fn new(cpu_type: CpuType) -> Self {
        match Self::try_new(cpu_type) {
            Ok(c) => c,
            Err(e) => panic!("{e}"),
        }
    }

    /// Reset the CPU, loading SP and PC from the vector table via the bus.
    pub fn reset<B: Bus>(&mut self, bus: &mut B) {
        self.with_bus(bus, ffi::pulse_reset);
    }

    /// Run until `max_cycles` are consumed, or a fault stops execution.
    ///
    /// Returns the cycles actually used.
    pub fn run<B: Bus>(&mut self, bus: &mut B, max_cycles: u32) -> Result<u32, RunError> {
        let used = self.with_bus(bus, || ffi::execute(max_cycles));
        if let Some(fault) = PENDING_FAULT.with(Cell::take) {
            return Err(fault);
        }
        Ok(used)
    }

    fn with_bus<B: Bus, R>(&mut self, bus: &mut B, f: impl FnOnce() -> R) -> R {
        // The restore must survive an unwind. This used to be three statements
        // with `f()` in the middle, so a panic inside a trap handler or a bus
        // callback skipped the restore and left a **dangling bus pointer** in
        // the thread-local for the next call to read. The guard's Drop runs on
        // both the normal and the unwinding path.
        struct Restore {
            prev: (*mut (), Option<&'static BusVTable>),
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                ACTIVE.with(|c| c.set(self.prev));
            }
        }
        let _restore = Restore {
            prev: ACTIVE
                .with(|c| c.replace(((bus as *mut B).cast::<()>(), Some(vtable_for::<B>())))),
        };
        f()
    }

    // ---- register access -------------------------------------------------

    #[must_use]
    pub fn pc(&self) -> u32 {
        ffi::get_pc()
    }
    pub fn set_pc(&mut self, v: u32) {
        ffi::set_pc(v);
    }
    #[must_use]
    pub fn sp(&self) -> u32 {
        ffi::get_sp()
    }
    pub fn set_sp(&mut self, v: u32) {
        ffi::set_sp(v);
    }
    #[must_use]
    pub fn data(&self, n: u8) -> u32 {
        ffi::get_data_reg(n)
    }
    pub fn set_data(&mut self, n: u8, v: u32) {
        ffi::set_data_reg(n, v);
    }
    #[must_use]
    pub fn addr(&self, n: u8) -> u32 {
        ffi::get_addr_reg(n)
    }
    pub fn set_addr(&mut self, n: u8, v: u32) {
        ffi::set_addr_reg(n, v);
    }
    #[must_use]
    pub fn sr(&self) -> u32 {
        ffi::get_sr()
    }
    pub fn set_sr(&mut self, v: u32) {
        ffi::set_sr(v);
    }

    /// Disassemble one instruction, for traces and diagnostics.
    #[must_use]
    pub fn disassemble<B: Bus>(&mut self, bus: &mut B, pc: u32) -> (String, u32) {
        self.with_bus(bus, || ffi::disassemble(pc))
    }

    /// Stop execution as soon as control returns to the core.
    pub fn halt(&mut self) {
        ffi::end_timeslice();
    }

    /// Stop cleanly whenever the PC reaches `addr`.
    ///
    /// Pass `None` to clear. Used for the host's sentinel return address.
    pub fn set_stop_address(&mut self, addr: Option<u32>) {
        HOOK.with(|h| h.stop_addr.set(addr));
    }

    /// Whether the stop address was reached, clearing the flag.
    pub fn take_stop_hit(&mut self) -> bool {
        STOP_HIT.with(Cell::take)
    }

    /// Fault any execution below `floor` as a wild jump. `0` disables.
    ///
    /// The host sets this to the base of the code region: exception vectors,
    /// low-memory globals and master pointers live below it, and none of those
    /// are ever legitimate code.
    pub fn set_wild_jump_floor(&mut self, floor: u32) {
        HOOK.with(|h| h.wild_floor.set(floor));
    }

    /// Fault any execution at or above `ceiling` as a wild jump. `0` disables.
    pub fn set_wild_jump_ceiling(&mut self, ceiling: u32) {
        HOOK.with(|h| h.wild_ceil.set(ceiling));
    }

    /// Record the last `len` program counters. `0` disables tracing.
    ///
    /// Costs a bounds check and a store per instruction, so leave it off for soak
    /// runs and turn it on to diagnose a specific module.
    pub fn set_trace(&mut self, len: usize) {
        HOOK.with(|h| {
            *h.pc_trace.borrow_mut() = if len == 0 {
                None
            } else {
                Some((vec![0u32; len], 0))
            };
            h.tracing.set(len != 0);
        });
    }

    /// The recorded PCs, oldest first.
    #[must_use]
    pub fn trace(&self) -> Vec<u32> {
        HOOK.with(|h| match &*h.pc_trace.borrow() {
            Some((buf, next)) => buf
                .iter()
                .cycle()
                .skip(*next)
                .take(buf.len())
                .copied()
                .filter(|p| *p != 0)
                .collect(),
            None => Vec::new(),
        })
    }

    /// Snapshot the whole register file.
    ///
    /// Interrupt-style deliveries (VBL tasks) borrow the CPU mid-run; the real
    /// interrupt saved every register, so the host must too — a task that
    /// clobbers D3 must not break the loop it interrupted.
    #[must_use]
    pub fn save_regs(&self) -> RegisterFile {
        let mut d = [0u32; 8];
        let mut a = [0u32; 8];
        for n in 0..8u8 {
            d[usize::from(n)] = self.data(n);
            a[usize::from(n)] = self.addr(n);
        }
        RegisterFile {
            d,
            a,
            pc: self.pc(),
            sr: self.sr(),
        }
    }

    /// Restore a register file saved by [`Self::save_regs`].
    pub fn restore_regs(&mut self, r: &RegisterFile) {
        // SR first: flipping the S bit remaps which stack pointer A7 names,
        // so the A7 write must happen under the same mode it was saved in.
        self.set_sr(r.sr);
        for n in 0..8u8 {
            self.set_data(n, r.d[usize::from(n)]);
            self.set_addr(n, r.a[usize::from(n)]);
        }
        self.set_pc(r.pc);
    }
}

/// A complete 68000 register snapshot. See [`Cpu::save_regs`].
#[derive(Debug, Clone, Copy)]
pub struct RegisterFile {
    pub d: [u32; 8],
    pub a: [u32; 8],
    pub pc: u32,
    pub sr: u32,
}

/// The PC of the instruction currently executing, callable from bus handlers.
///
/// Lets a memory watchpoint attribute a write to the code that made it.
#[must_use]
pub fn current_pc() -> u32 {
    ffi::get_pc()
}

impl Drop for Cpu {
    fn drop(&mut self) {
        CPU_TAKEN.store(false, Ordering::SeqCst);
    }
}

// ------------------------------------------------------------------ callbacks

fn with_vtable<R>(f: impl FnOnce(*mut (), &'static BusVTable) -> R, fallback: R) -> R {
    let (ptr, vt) = ACTIVE.with(Cell::get);
    match (ptr.is_null(), vt) {
        (false, Some(vt)) => f(ptr, vt),
        _ => fallback,
    }
}

macro_rules! read_cb {
    ($name:ident, $field:ident) => {
        /// Returns `c_uint`, not the width being read.
        ///
        /// Musashi declares every one of these as `unsigned int
        /// m68k_read_memory_N(unsigned int)` — the *width* is in the name, not
        /// the return type. Narrowing the Rust side to `u8`/`u16` is an ABI
        /// mismatch, not a tidier spelling of the same thing: under the x86-64
        /// SysV convention a `u8` return only defines `al`, so the C core reads
        /// whatever was left in the upper 24 bits of `eax`, and an operand
        /// fetch comes back with garbage above the byte. clang happens to
        /// zero-extend narrow returns and AArch64 does too, which is why this
        /// survived on macOS and on arm64 Linux while gcc and MSVC on x86-64
        /// segfaulted the moment a module executed its first instruction.
        #[unsafe(no_mangle)]
        pub extern "C" fn $name(address: u32) -> core::ffi::c_uint {
            with_vtable(|p, vt| core::ffi::c_uint::from((vt.$field)(p, address)), 0)
        }
    };
}

read_cb!(m68k_read_memory_8, read_u8);
read_cb!(m68k_read_memory_16, read_u16);
read_cb!(m68k_read_memory_32, read_u32);
// Musashi uses the "immediate" and "disassembler" variants for instruction and
// operand fetch when M68K_SEPARATE_READS is on; alias them to the same bus so a
// future config change cannot silently diverge.
read_cb!(m68k_read_immediate_16, read_u16);
read_cb!(m68k_read_immediate_32, read_u32);
read_cb!(m68k_read_disassembler_8, read_u8);
read_cb!(m68k_read_disassembler_16, read_u16);
read_cb!(m68k_read_disassembler_32, read_u32);

#[unsafe(no_mangle)]
pub extern "C" fn m68k_write_memory_8(address: u32, value: u32) {
    with_vtable(|p, vt| (vt.write_u8)(p, address, value as u8), ());
}

#[unsafe(no_mangle)]
pub extern "C" fn m68k_write_memory_16(address: u32, value: u32) {
    with_vtable(|p, vt| (vt.write_u16)(p, address, value as u16), ());
}

#[unsafe(no_mangle)]
pub extern "C" fn m68k_write_memory_32(address: u32, value: u32) {
    with_vtable(|p, vt| (vt.write_u32)(p, address, value), ());
}

/// Musashi's per-instruction hook: the A-line trap gate.
///
/// See the module docs. This runs *before* the `RTE` at [`TRAP_GATE`], services
/// the trap, and corrects the stacked return address so the `RTE` resumes after
/// the trap word rather than on it.
#[unsafe(no_mangle)]
pub extern "C" fn ad_m68k_instr_hook(pc: u32) {
    // One thread-local read for the whole fast path, and no `RefCell` borrow
    // unless a trace is actually recording. This function runs before every
    // emulated instruction; see the `HOOK` declaration for what its old shape
    // cost.
    let go = HOOK.with(|h| {
        if h.tracing.get() {
            if let Some((buf, next)) = &mut *h.pc_trace.borrow_mut() {
                let n = buf.len();
                if let Some(slot) = buf.get_mut(*next) {
                    *slot = pc;
                }
                *next = next.saturating_add(1) % n.max(1);
            }
        }
        if h.stop_addr.get() == Some(pc) {
            return HookOutcome::Stop;
        }
        let ceil = h.wild_ceil.get();
        if ceil != 0 && pc >= ceil {
            return HookOutcome::Wild;
        }
        let floor = h.wild_floor.get();
        if floor != 0 && pc < floor {
            return HookOutcome::Wild;
        }
        HookOutcome::Run
    });
    match go {
        HookOutcome::Stop => {
            STOP_HIT.with(|c| c.set(true));
            ffi::end_timeslice();
            return;
        }
        HookOutcome::Wild => {
            PENDING_FAULT.with(|c| c.set(Some(RunError::WildJump { pc })));
            ffi::end_timeslice();
            return;
        }
        HookOutcome::Run => {}
    }
    if pc != TRAP_GATE {
        return;
    }
    with_vtable(
        |p, vt| {
            let sp = ffi::get_sp();
            let frame_size = HOOK.with(|h| h.frame_size.get());
            // Exception frame format 0000: SR at SP+0, faulting PC at SP+2. On a
            // 68010 and later a format/vector word follows at SP+6; it is read
            // here and written back unchanged, because `RTE` uses it to decide
            // how far to pop and synthesising one would mean encoding the vector
            // offset ourselves for no gain.
            let sr = (vt.read_u16)(p, sp);
            let trap_pc = (vt.read_u32)(p, sp.wrapping_add(2));
            let format_word = if frame_size > EXCEPTION_FRAME_SIZE {
                Some((vt.read_u16)(p, sp.wrapping_add(6)))
            } else {
                None
            };
            let trap_word = (vt.read_u16)(p, trap_pc);

            // Pop the frame so the handler sees the Toolbox arguments at SP.
            ffi::set_sp(sp.wrapping_add(frame_size));

            let mut regs = LiveRegs {
                trap_pc,
                resume: None,
                ccr: None,
            };
            if let Err(e) = (vt.trap)(p, trap_word, &mut regs) {
                PENDING_FAULT.with(|c| {
                    c.set(Some(RunError::UnhandledTrap {
                        trap: e.trap,
                        pc: trap_pc,
                        detail: e.detail,
                    }));
                });
                ffi::end_timeslice();
                return;
            }

            // Rebuild the frame at wherever the handler left SP. Normally the
            // return address is just past the trap word; an auto-pop trap asks to
            // resume at the caller instead, having popped the address its glue
            // pushed.
            let resume = regs.resume.unwrap_or(trap_pc.wrapping_add(2));
            // A handler that published condition codes replaces only the low five
            // bits; everything above them, including the supervisor state the
            // `RTE` needs, is the caller's.
            let sr = match regs.ccr {
                Some(c) => (sr & !u16::from(ccr::MASK)) | u16::from(c & ccr::MASK),
                None => sr,
            };
            let new_sp = ffi::get_sp().wrapping_sub(frame_size);
            (vt.write_u16)(p, new_sp, sr);
            (vt.write_u32)(p, new_sp.wrapping_add(2), resume);
            if let Some(fw) = format_word {
                (vt.write_u16)(p, new_sp.wrapping_add(6), fw);
            }
            ffi::set_sp(new_sp);
        },
        (),
    );
}

#[cfg(test)]
mod tests;
