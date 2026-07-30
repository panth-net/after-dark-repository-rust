//! Tests for the CPU wrapper and the A-line trap gate.
//!
//! Musashi keeps CPU state in C globals, so `Cpu::new` allows only one instance
//! per process. Cargo runs tests on threads within one process, so these tests
//! share a single CPU behind a mutex rather than each making their own.

use super::*;
use std::sync::{Mutex, MutexGuard, OnceLock};

fn cpu() -> MutexGuard<'static, Cpu> {
    static CPU: OnceLock<Mutex<Cpu>> = OnceLock::new();
    let mut guard = CPU
        .get_or_init(|| Mutex::new(Cpu::new(CpuType::M68000)))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    // Musashi keeps registers in C globals, so they survive between tests. Clear
    // them or an assertion can pass on a value another test left behind.
    for n in 0..8 {
        guard.set_data(n, 0);
        guard.set_addr(n, 0);
    }
    guard.set_stop_address(None);
    let _ = guard.take_stop_hit();
    guard
}

const RAM: usize = 0x10_000;
const CODE: u32 = 0x0400;
const STACK: u32 = 0x8000;

/// Flat RAM plus a recording trap handler.
struct TestBus {
    ram: Vec<u8>,
    /// Every trap the CPU asked for, in order.
    traps: Vec<u16>,
    /// Traps to reject, to exercise the unhandled path.
    reject: Vec<u16>,
    /// `D0` values observed at each trap.
    d0_at_trap: Vec<u32>,
}

impl TestBus {
    fn new() -> Self {
        let mut bus = Self {
            ram: vec![0; RAM],
            traps: Vec::new(),
            reject: Vec::new(),
            d0_at_trap: Vec::new(),
        };
        // Reset vector: SP at 0, PC at 4.
        bus.write_u32(0, STACK);
        bus.write_u32(4, CODE);
        // Line-A exception vector -> the trap gate.
        bus.write_u32(VECTOR_LINE_A, TRAP_GATE);
        bus
    }

    fn load(&mut self, addr: u32, words: &[u16]) {
        for (i, w) in words.iter().enumerate() {
            self.write_u16(addr + (i as u32 * 2), *w);
        }
    }
}

impl Bus for TestBus {
    fn read_u8(&mut self, addr: u32) -> u8 {
        // The trap gate is not RAM: it holds a single RTE ($4E73).
        if addr == TRAP_GATE {
            return 0x4E;
        }
        if addr == TRAP_GATE + 1 {
            return 0x73;
        }
        self.ram.get(addr as usize).copied().unwrap_or(0)
    }
    fn read_u16(&mut self, addr: u32) -> u16 {
        u16::from(self.read_u8(addr)) << 8 | u16::from(self.read_u8(addr.wrapping_add(1)))
    }
    fn read_u32(&mut self, addr: u32) -> u32 {
        u32::from(self.read_u16(addr)) << 16 | u32::from(self.read_u16(addr.wrapping_add(2)))
    }
    fn write_u8(&mut self, addr: u32, value: u8) {
        if let Some(slot) = self.ram.get_mut(addr as usize) {
            *slot = value;
        }
    }
    fn write_u16(&mut self, addr: u32, value: u16) {
        self.write_u8(addr, (value >> 8) as u8);
        self.write_u8(addr.wrapping_add(1), value as u8);
    }
    fn write_u32(&mut self, addr: u32, value: u32) {
        self.write_u16(addr, (value >> 16) as u16);
        self.write_u16(addr.wrapping_add(2), value as u16);
    }

    fn trap(&mut self, trap: u16, regs: &mut dyn Registers) -> Result<(), TrapError> {
        self.traps.push(trap);
        self.d0_at_trap.push(regs.data(0));
        if self.reject.contains(&trap) {
            return Err(TrapError::unimplemented(trap));
        }
        // Stand in for a real Toolbox call: hand back a known value in D0.
        regs.set_data(0, 0x1234_5678);
        Ok(())
    }
}

#[test]
fn executes_basic_instructions() {
    let mut bus = TestBus::new();
    // moveq #7,d0 ; moveq #2,d1 ; add.l d1,d0 ; nop (stop here)
    bus.load(CODE, &[0x700F, 0x7202, 0xD081, 0x4E71]);
    let mut cpu = cpu();
    cpu.reset(&mut bus);
    assert_eq!(cpu.pc(), CODE, "reset should load PC from vector 1");
    assert_eq!(cpu.sp(), STACK, "reset should load SP from vector 0");

    cpu.set_stop_address(Some(CODE + 6));
    let _ = cpu.run(&mut bus, 100);
    assert_eq!(cpu.data(0), 0x11, "0x0F + 0x02");
}

#[test]
fn a_line_trap_is_dispatched_and_execution_resumes() {
    let mut bus = TestBus::new();
    // moveq #1,d0 ; _Random ($A861) ; moveq #9,d1 ; nop (stop here)
    // If the gate mis-handles the stacked PC we either loop on the trap forever
    // or skip the following instruction.
    bus.load(CODE, &[0x7001, 0xA861, 0x7209, 0x4E71]);
    let mut cpu = cpu();
    cpu.reset(&mut bus);
    cpu.set_stop_address(Some(CODE + 6));
    let _ = cpu.run(&mut bus, 2_000);

    assert_eq!(bus.traps, vec![0xA861], "the trap should fire exactly once");
    assert_eq!(
        bus.d0_at_trap,
        vec![1],
        "the handler should see register state from before the trap"
    );
    assert_eq!(
        cpu.data(0),
        0x1234_5678,
        "the handler's D0 write must survive the RTE"
    );
    assert_eq!(
        cpu.data(1),
        9,
        "execution must resume at the instruction after the trap word"
    );
}

#[test]
fn multiple_traps_in_sequence() {
    let mut bus = TestBus::new();
    // Three Toolbox calls back to back, then a NOP we stop on.
    //
    // Do not end with `illegal` here: with no handler installed the CPU vectors
    // to address 0 and walks up through zeroed memory (`ori.b #0,D0`) until it
    // re-enters this code and runs the traps a second time.
    bus.load(CODE, &[0xA861, 0xA029, 0xA02A, 0x4E71]);
    let mut cpu = cpu();
    cpu.reset(&mut bus);
    cpu.set_stop_address(Some(CODE + 6));
    let _ = cpu.run(&mut bus, 4_000);
    assert!(cpu.take_stop_hit(), "should have reached the stop address");
    assert_eq!(bus.traps, vec![0xA861, 0xA029, 0xA02A]);
}

#[test]
fn unhandled_trap_stops_execution_with_context() {
    let mut bus = TestBus::new();
    bus.reject.push(0xA9F0);
    // moveq #1,d0 ; _LoadSeg ($A9F0) ; moveq #9,d1 ; nop
    bus.load(CODE, &[0x7001, 0xA9F0, 0x7209, 0x4E71]);
    let mut cpu = cpu();
    cpu.reset(&mut bus);
    let err = cpu.run(&mut bus, 2_000).expect_err("should fault");

    match err {
        RunError::UnhandledTrap { trap, pc, .. } => {
            assert_eq!(trap, 0xA9F0);
            assert_eq!(pc, CODE + 2, "PC should point at the trap word itself");
        }
        other => panic!("expected UnhandledTrap, got {other:?}"),
    }
    assert_eq!(
        cpu.data(1),
        0,
        "execution must not continue past an unhandled trap"
    );
}

#[test]
fn trap_error_message_is_actionable() {
    let e = RunError::UnhandledTrap {
        trap: 0xA9F0,
        pc: 0x0012_F82A,
        detail: "not implemented".into(),
    };
    let s = e.to_string();
    // A developer reading this should know the trap and where it came from.
    assert!(s.contains("A9F0"), "{s}");
    assert!(s.contains("12f82a"), "{s}");
}

#[test]
fn disassembles_instructions() {
    let mut bus = TestBus::new();
    bus.load(CODE, &[0x7001]);
    let mut cpu = cpu();
    let (text, len) = cpu.disassemble(&mut bus, CODE);
    assert_eq!(len, 2, "moveq is one word");
    assert!(
        text.to_ascii_lowercase().contains("moveq"),
        "unexpected disassembly: {text}"
    );
}

#[test]
fn bus_reads_are_big_endian() {
    let mut bus = TestBus::new();
    bus.write_u32(0x1000, 0xDEAD_BEEF);
    assert_eq!(bus.read_u8(0x1000), 0xDE, "68000 is big-endian");
    assert_eq!(bus.read_u8(0x1003), 0xEF);
    assert_eq!(bus.read_u16(0x1000), 0xDEAD);
    assert_eq!(bus.read_u32(0x1000), 0xDEAD_BEEF);
}

#[test]
fn toolbox_trap_sees_its_stack_arguments() {
    // The exception frame sits below the arguments, so a gate that does not pop
    // it hands the handler the SR word instead of its first argument — and any
    // result it writes lands on the frame the RTE is about to restore.
    struct ArgBus {
        inner: TestBus,
        seen_arg: Option<u32>,
        sp_at_trap: u32,
    }
    impl Bus for ArgBus {
        fn read_u8(&mut self, a: u32) -> u8 {
            self.inner.read_u8(a)
        }
        fn read_u16(&mut self, a: u32) -> u16 {
            self.inner.read_u16(a)
        }
        fn read_u32(&mut self, a: u32) -> u32 {
            self.inner.read_u32(a)
        }
        fn write_u8(&mut self, a: u32, v: u8) {
            self.inner.write_u8(a, v);
        }
        fn write_u16(&mut self, a: u32, v: u16) {
            self.inner.write_u16(a, v);
        }
        fn write_u32(&mut self, a: u32, v: u32) {
            self.inner.write_u32(a, v);
        }
        fn trap(&mut self, _trap: u16, regs: &mut dyn Registers) -> Result<(), TrapError> {
            self.sp_at_trap = regs.sp();
            // The argument the caller pushed must be at SP+0.
            self.seen_arg = Some(self.inner.read_u32(regs.sp()));
            // Pop it and leave a 16-bit result in the caller's reserved slot,
            // exactly as a Pascal function would.
            let sp = regs.sp().wrapping_add(4);
            self.inner.write_u16(sp, 0xBEEF);
            regs.set_sp(sp);
            Ok(())
        }
    }

    let mut bus = ArgBus {
        inner: TestBus::new(),
        seen_arg: None,
        sp_at_trap: 0,
    };
    // clr.w -(sp)              ; reserve the result slot
    // move.l #$deadbeef,-(sp)  ; push one argument
    // _Random                  ; the trap
    // move.w (sp)+,d1          ; pop the result
    // illegal
    bus.inner.load(
        CODE,
        &[
            0x4267, // clr.w -(a7)
            0x2F3C, 0xDEAD, 0xBEEF, // move.l #$deadbeef,-(a7)
            0xA861, // _Random
            0x321F, // move.w (a7)+,d1
            0x4E71, // nop  <- stop here; the hook fires before it but Musashi
                    //         still executes one instruction, so it must be
                    //         harmless or the final SP is skewed.
        ],
    );
    let mut cpu = cpu();
    cpu.reset(&mut bus);
    // Stop at the trailing `illegal` rather than executing it: an unhandled
    // illegal-instruction exception would push frames and make the final SP
    // meaningless.
    cpu.set_stop_address(Some(CODE + 12));
    let _ = cpu.run(&mut bus, 4_000);
    assert!(cpu.take_stop_hit(), "should have reached the stop address");

    assert_eq!(
        bus.seen_arg,
        Some(0xDEAD_BEEF),
        "handler must see the pushed argument at SP, not the exception frame"
    );
    assert_eq!(
        cpu.data(1) & 0xFFFF,
        0xBEEF,
        "the handler's result must reach the caller through the RTE"
    );
    assert_eq!(
        cpu.sp(),
        STACK,
        "the stack must be fully unwound: frame + argument + result"
    );
}

/// `Cpu` must be `Send` but not `Sync`.
///
/// `!Sync` because Musashi's register file is in C globals: two threads driving
/// one CPU concurrently would corrupt each other. `Send` because moving one to
/// another thread and using it exclusively is sound, and because it is what
/// makes `Mutex<Cpu>` — the pattern this very test module relies on — legal.
#[test]
fn cpu_is_send_but_not_sync() {
    fn assert_send<T: Send>() {}
    assert_send::<Cpu>();
    // The negative half cannot be asserted in the type system from inside the
    // same crate, but `static OnceLock<Mutex<Cpu>>` above compiling proves
    // `Cpu: Send`, and `Cpu: !Sync` is enforced by the PhantomData marker —
    // remove it and `Mutex` would no longer be the only way to share one.
}

#[test]
fn a_second_cpu_is_refused_rather_than_silently_sharing_state() {
    // try_new exists so a caller that can recover — a library UI opening a
    // preview beside a running module — gets an error instead of a panic.
    let _guard = cpu();
    assert_eq!(
        Cpu::try_new(CpuType::M68000).err(),
        Some(CpuAlreadyTaken),
        "a second Cpu would share Musashi's C globals with the first"
    );
}

#[test]
fn a_panic_inside_run_does_not_leave_a_dangling_bus_pointer() {
    // The bug this guards: `with_bus` used to install the bus, call `f()`, then
    // restore — so an unwind through `f()` skipped the restore and left a
    // pointer to a dead bus in the thread-local for the next caller to read.
    struct Boom;
    impl Bus for Boom {
        fn read_u8(&mut self, _: u32) -> u8 {
            panic!("bus explodes")
        }
        fn read_u16(&mut self, _: u32) -> u16 {
            0
        }
        fn read_u32(&mut self, _: u32) -> u32 {
            0
        }
        fn write_u8(&mut self, _: u32, _: u8) {}
        fn write_u16(&mut self, _: u32, _: u16) {}
        fn write_u32(&mut self, _: u32, _: u32) {}
        fn trap(&mut self, _: u16, _: &mut dyn Registers) -> Result<(), TrapError> {
            Ok(())
        }
    }

    let mut guard = cpu();
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        guard.with_bus(&mut Boom, || {
            // Reach through the thread-local the way a C callback does.
            with_vtable(|p, vt| (vt.read_u8)(p, 0), 0)
        });
    }));
    assert!(caught.is_err(), "the panic must propagate, not be swallowed");

    // The guard's Drop must have cleared the thread-local back to null.
    let (leaked, _) = ACTIVE.with(std::cell::Cell::get);
    assert!(
        leaked.is_null(),
        "unwinding left a dangling bus pointer at {leaked:?}"
    );
}

/// Frame size follows the CPU, and only the 68000 uses six bytes.
///
/// Every later member appends a format word that `RTE` reads to size its own pop.
/// The gate assumed six unconditionally, so the first attempt at a 68020 popped
/// two bytes too few and sent 54 of 66 modules into wild jumps — the `RTE`
/// resumed through a word of the caller's stack.
#[test]
fn the_exception_frame_grows_after_the_68000() {
    assert_eq!(CpuType::M68000.exception_frame_size(), EXCEPTION_FRAME_SIZE);
    assert_eq!(EXCEPTION_FRAME_SIZE, 6);
    for later in [
        CpuType::M68010,
        CpuType::M68020,
        CpuType::M68030,
        CpuType::M68040,
    ] {
        assert_eq!(
            later.exception_frame_size(),
            EXCEPTION_FRAME_SIZE_68010,
            "{later:?} appends a format word"
        );
    }
    assert_eq!(EXCEPTION_FRAME_SIZE_68010, 8);
}

/// A handler may replace the condition codes without touching supervisor state.
///
/// SANE's comparisons answer only through the CCR, so the gate has to let a
/// handler publish flags. It must not let one publish an interrupt mask.
#[test]
fn published_condition_codes_are_masked_to_the_ccr() {
    assert_eq!(ccr::MASK, 0x1F);
    // The bits, in their 68000 positions.
    assert_eq!(ccr::CARRY, 0x01);
    assert_eq!(ccr::OVERFLOW, 0x02);
    assert_eq!(ccr::ZERO, 0x04);
    assert_eq!(ccr::NEGATIVE, 0x08);
    assert_eq!(ccr::EXTEND, 0x10);

    // What the gate computes: the low five bits come from the handler, the rest
    // from the caller's stacked SR. `0x2700` is supervisor with interrupts masked.
    let stacked_sr: u16 = 0x2700 | 0x001F;
    let published: u8 = ccr::ZERO;
    let merged = (stacked_sr & !u16::from(ccr::MASK)) | u16::from(published & ccr::MASK);
    assert_eq!(merged, 0x2704, "supervisor half preserved, CCR replaced");
}
