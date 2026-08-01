#![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use super::*;
use crate::quickdraw::{Rect, SCREEN_HEIGHT, SCREEN_WIDTH, map_rect};

/// A stand-in for CPU registers so traps can be tested without running code.
struct FakeRegs {
    d: [u32; 8],
    a: [u32; 8],
    sp: u32,
    trap_pc: u32,
    resume: Option<u32>,
    ccr: Option<u8>,
}

impl FakeRegs {
    fn new(sp: u32) -> Self {
        Self {
            d: [0; 8],
            a: [0; 8],
            sp,
            trap_pc: 0x1000,
            resume: None,
            ccr: None,
        }
    }
}

impl Registers for FakeRegs {
    fn data(&self, n: u8) -> u32 {
        self.d[usize::from(n.min(7))]
    }
    fn set_data(&mut self, n: u8, v: u32) {
        self.d[usize::from(n.min(7))] = v;
    }
    fn addr(&self, n: u8) -> u32 {
        self.a[usize::from(n.min(7))]
    }
    fn set_addr(&mut self, n: u8, v: u32) {
        self.a[usize::from(n.min(7))] = v;
    }
    fn sp(&self) -> u32 {
        self.sp
    }
    fn set_sp(&mut self, v: u32) {
        self.sp = v;
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
        self.ccr = Some(c);
    }
    fn condition_codes(&self) -> Option<u8> {
        self.ccr
    }
}

const SP: u32 = 0x0060_0000;

/// Read a Pascal `BOOLEAN` result: it lives in the **high** byte of its 2-byte
/// slot, because the caller fetches it with `MOVE.B (A7)+,Dn`.
fn read_bool(tb: &mut Toolbox, at: u32) -> bool {
    tb.mem.read_u8(at) != 0
}

/// Push Pascal arguments left to right, returning the resulting SP.
fn push_args(tb: &mut Toolbox, args: &[Arg]) -> u32 {
    let mut sp = SP;
    for a in args {
        match a {
            Arg::W(v) => {
                sp -= 2;
                tb.mem.write_u16(sp, *v as u16);
            }
            Arg::L(v) => {
                sp -= 4;
                tb.mem.write_u32(sp, *v);
            }
        }
    }
    sp
}

#[derive(Clone, Copy)]
enum Arg {
    W(i16),
    L(u32),
}

/// Call a trap the way compiled code does: reserve `result_bytes` for the
/// function result, then push arguments left to right.
fn call_fn(tb: &mut Toolbox, trap: u16, args: &[Arg], result_bytes: u32) -> (FakeRegs, u32) {
    let slot = SP - result_bytes;
    let mut sp = slot;
    for a in args {
        match a {
            Arg::W(v) => {
                sp -= 2;
                tb.mem.write_u16(sp, *v as u16);
            }
            Arg::L(v) => {
                sp -= 4;
                tb.mem.write_u32(sp, *v);
            }
        }
    }
    let mut regs = FakeRegs::new(sp);
    tb.trap(trap, &mut regs).expect("trap should be handled");
    (regs, slot)
}

fn call(tb: &mut Toolbox, trap: u16, args: &[Arg]) -> FakeRegs {
    let sp = push_args(tb, args);
    let mut regs = FakeRegs::new(sp);
    tb.trap(trap, &mut regs).expect("trap should be handled");
    regs
}

// ---------------------------------------------------------------- dispatcher

#[test]
fn unimplemented_trap_reports_its_identity() {
    let mut tb = Toolbox::new();
    let mut regs = FakeRegs::new(SP);
    // _LoadSeg is deliberately not implemented yet.
    let err = tb.trap(0xA9F0, &mut regs).expect_err("must fail");
    assert_eq!(err.trap, 0xA9F0);
    assert!(err.detail.contains("LoadSeg"), "{}", err.detail);
    assert_eq!(tb.log.unimplemented.get(&0xA9F0), Some(&1));
}

#[test]
fn trap_log_records_calls_and_names() {
    let mut tb = Toolbox::new();
    let _ = call(&mut tb, 0xA861, &[]);
    let _ = call(&mut tb, 0xA861, &[]);
    let _ = call(&mut tb, 0xA975, &[]);
    assert_eq!(tb.log.counts.get(&0xA861), Some(&2));
    assert_eq!(tb.log.counts.get(&0xA975), Some(&1));
    assert_eq!(tb.log.distinct(), 2);
    let s = tb.log.summary();
    assert!(s.contains("Random"), "{s}");
    assert!(s.contains("TickCount"), "{s}");
    assert_eq!(tb.log.history[0].pc, 0x1000, "PC should be recorded");
}

#[test]
fn os_trap_flag_variants_share_one_implementation() {
    let mut tb = Toolbox::new();
    // Both $A122 (NewHandle) and $A322 (NewHandleClear) must allocate.
    for word in [0xA022u16, 0xA122, 0xA322] {
        let mut regs = FakeRegs::new(SP);
        regs.set_data(0, 64);
        tb.trap(word, &mut regs).expect("handled");
        assert_ne!(regs.addr(0), 0, "{word:#06x} should return a handle");
        assert_eq!(regs.data(0), 0, "OSErr should be noErr");
    }
}

// ---------------------------------------------------------------- memory traps

#[test]
fn new_handle_clear_actually_clears() {
    let mut tb = Toolbox::new();
    // Dirty the heap so a stale value would be visible.
    let mut regs = FakeRegs::new(SP);
    regs.set_data(0, 128);
    tb.trap(0xA022, &mut regs).expect("handled");
    let dirty = tb.mem.deref_handle(regs.addr(0)).expect("resolve");
    for i in 0..128 {
        tb.mem.write_u8(dirty + i, 0xAA);
    }

    let mut regs = FakeRegs::new(SP);
    regs.set_data(0, 128);
    tb.trap(0xA322, &mut regs).expect("handled"); // NewHandleClear
    let block = tb.mem.deref_handle(regs.addr(0)).expect("resolve");
    assert!((0..128).all(|i| tb.mem.read_u8(block + i) == 0));
}

#[test]
fn hlock_and_dispose_round_trip() {
    let mut tb = Toolbox::new();
    let mut regs = FakeRegs::new(SP);
    regs.set_data(0, 32);
    tb.trap(0xA022, &mut regs).expect("handled");
    let h = regs.addr(0);

    let mut regs = FakeRegs::new(SP);
    regs.set_addr(0, h);
    tb.trap(0xA029, &mut regs).expect("HLock");
    assert!(tb.mem.handle_info(h).expect("info").locked);

    let mut regs = FakeRegs::new(SP);
    regs.set_addr(0, h);
    tb.trap(0xA02A, &mut regs).expect("HUnlock");
    assert!(!tb.mem.handle_info(h).expect("info").locked);

    let mut regs = FakeRegs::new(SP);
    regs.set_addr(0, h);
    tb.trap(0xA023, &mut regs).expect("DisposHandle");
    assert_eq!(tb.mem.deref_handle(h), None);
}

#[test]
fn delay_reads_the_count_from_a0_and_returns_ticks_in_d0() {
    // IM II-384 and the Universal Interfaces glue (`_Delay` then
    // `MOVE.L D0,(A1)`): numTicks arrives in A0, the final tick count leaves
    // in D0. Reading D0 as the count added whatever the module last computed
    // to the clock — Lunatic Fringe's death pause put a pointer there, and a
    // paced host then owed seven hours of wall clock: the reported "freeze".
    let mut tb = Toolbox::new();
    let before = tb.ticks;
    let mut regs = FakeRegs::new(SP);
    regs.set_addr(0, 90);
    regs.set_data(0, 0x0018_48A0); // stale pointer-sized junk must be ignored
    tb.trap(0xA03B, &mut regs).expect("Delay");
    assert_eq!(tb.ticks, before + 90, "the clock advances by A0, not D0");
    assert_eq!(regs.data(0), tb.ticks, "D0 returns the final tick count");
}

#[test]
fn block_move_uses_registers_not_the_stack() {
    let mut tb = Toolbox::new();
    tb.mem.write_u32(0x5000, 0x1122_3344);
    let mut regs = FakeRegs::new(SP);
    regs.set_addr(0, 0x5000);
    regs.set_addr(1, 0x6000);
    regs.set_data(0, 4);
    tb.trap(0xA02E, &mut regs).expect("BlockMove");
    assert_eq!(tb.mem.read_u32(0x6000), 0x1122_3344);
    assert_eq!(regs.sp(), SP, "an OS trap must not touch the stack");
}

// ---------------------------------------------------------------- Random

#[test]
fn random_advances_the_seed_and_returns_on_the_stack() {
    let mut tb = Toolbox::new();
    let before = globals::LowMem::rnd_seed(&mut tb.mem);
    let (regs, slot) = call_fn(&mut tb, 0xA861, &[], 2);
    let after = globals::LowMem::rnd_seed(&mut tb.mem);

    assert_ne!(before, after, "_Random must advance RndSeed");
    let result = tb.mem.read_u16(slot) as i16;
    let (_, expected) = random::next(before);
    assert_eq!(result, expected);
    assert_eq!(
        regs.sp(),
        slot,
        "the routine must fill the caller's slot, not push a new one"
    );
}

#[test]
fn random_sequence_is_reproducible_across_toolbox_instances() {
    let seq = |_| {
        let mut tb = Toolbox::new();
        (0..16)
            .map(|_| {
                let (_, slot) = call_fn(&mut tb, 0xA861, &[], 2);
                tb.mem.read_u16(slot) as i16
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        seq(()),
        seq(()),
        "a fresh Toolbox must replay the same RNG stream"
    );
}

// ---------------------------------------------------------------- QuickDraw

#[test]
fn rect_memory_layout_is_top_left_bottom_right() {
    let mut tb = Toolbox::new();
    let addr = 0x7000;
    Rect::new(10, 20, 30, 40).write(&mut tb.mem, addr);
    assert_eq!(tb.mem.read_u16(addr), 10, "top comes first");
    assert_eq!(tb.mem.read_u16(addr + 2), 20, "then left");
    assert_eq!(tb.mem.read_u16(addr + 4), 30, "then bottom");
    assert_eq!(tb.mem.read_u16(addr + 6), 40, "then right");
    assert_eq!(Rect::read(&mut tb.mem, addr), Rect::new(10, 20, 30, 40));
}

#[test]
fn set_rect_argument_order_is_left_top_right_bottom() {
    let mut tb = Toolbox::new();
    let addr = 0x7100u32;
    // SetRect(r, left=5, top=6, right=7, bottom=8)
    call(
        &mut tb,
        0xA8A7,
        &[Arg::L(addr), Arg::W(5), Arg::W(6), Arg::W(7), Arg::W(8)],
    );
    assert_eq!(
        Rect::read(&mut tb.mem, addr),
        Rect::new(6, 5, 8, 7),
        "SetRect takes l,t,r,b but stores t,l,b,r"
    );
}

#[test]
fn offset_and_inset_rect_mutate_in_place() {
    let mut tb = Toolbox::new();
    let addr = 0x7200u32;
    Rect::new(10, 10, 20, 20).write(&mut tb.mem, addr);
    call(&mut tb, 0xA8A8, &[Arg::L(addr), Arg::W(5), Arg::W(3)]);
    assert_eq!(Rect::read(&mut tb.mem, addr), Rect::new(13, 15, 23, 25));

    Rect::new(10, 10, 20, 20).write(&mut tb.mem, addr);
    call(&mut tb, 0xA8A9, &[Arg::L(addr), Arg::W(2), Arg::W(2)]);
    assert_eq!(Rect::read(&mut tb.mem, addr), Rect::new(12, 12, 18, 18));
}

#[test]
fn sect_rect_returns_false_and_empties_on_no_overlap() {
    let mut tb = Toolbox::new();
    let (a, b, dst) = (0x7300u32, 0x7310u32, 0x7320u32);
    Rect::new(0, 0, 10, 10).write(&mut tb.mem, a);
    Rect::new(50, 50, 60, 60).write(&mut tb.mem, b);
    let regs = call(&mut tb, 0xA8AA, &[Arg::L(a), Arg::L(b), Arg::L(dst)]);
    let sp = regs.sp();
    assert!(!read_bool(&mut tb, sp), "no intersection -> false");
    assert!(Rect::read(&mut tb.mem, dst).is_empty());

    Rect::new(0, 0, 10, 10).write(&mut tb.mem, a);
    Rect::new(5, 5, 20, 20).write(&mut tb.mem, b);
    let regs = call(&mut tb, 0xA8AA, &[Arg::L(a), Arg::L(b), Arg::L(dst)]);
    let sp = regs.sp();
    assert!(read_bool(&mut tb, sp), "overlap -> true");
    assert_eq!(Rect::read(&mut tb.mem, dst), Rect::new(5, 5, 10, 10));
}

#[test]
fn pt_in_rect_reads_point_as_v_then_h() {
    let mut tb = Toolbox::new();
    let r = 0x7400u32;
    Rect::new(10, 20, 30, 40).write(&mut tb.mem, r);
    // Point packs v in the high word, h in the low word.
    let inside = (15u32 << 16) | 25;
    let outside = (5u32 << 16) | 25;
    let regs = call(&mut tb, 0xA8AD, &[Arg::L(inside), Arg::L(r)]);
    let sp = regs.sp();
    assert!(read_bool(&mut tb, sp));
    let regs = call(&mut tb, 0xA8AD, &[Arg::L(outside), Arg::L(r)]);
    let sp = regs.sp();
    assert!(!read_bool(&mut tb, sp));
}

#[test]
fn map_rect_scales_between_coordinate_spaces() {
    let src = Rect::new(0, 0, 100, 100);
    let dst = Rect::new(0, 0, 200, 400);
    let r = map_rect(&Rect::new(10, 10, 20, 20), &src, &dst);
    assert_eq!(r, Rect::new(20, 40, 40, 80), "2x vertical, 4x horizontal");
    // Identity mapping must be exact.
    assert_eq!(
        map_rect(&Rect::new(3, 4, 5, 6), &src, &src),
        Rect::new(3, 4, 5, 6)
    );
}

#[test]
fn map_rect_trap_mutates_in_place() {
    let mut tb = Toolbox::new();
    let (target, src, dst) = (0x7500u32, 0x7510u32, 0x7520u32);
    Rect::new(10, 10, 20, 20).write(&mut tb.mem, target);
    Rect::new(0, 0, 100, 100).write(&mut tb.mem, src);
    Rect::new(0, 0, 200, 400).write(&mut tb.mem, dst);
    call(&mut tb, 0xA8FA, &[Arg::L(target), Arg::L(src), Arg::L(dst)]);
    assert_eq!(Rect::read(&mut tb.mem, target), Rect::new(20, 40, 40, 80));
}

#[test]
fn drawing_reaches_the_framebuffer() {
    // Drawn on the fresh screen (index 0, white), not a blanked one: the
    // default pen is black, and black ink on a blanked-black screen counts
    // every pixel as painted.
    let mut tb = Toolbox::new();
    let bg = 0u8;
    assert_eq!(tb.qd.fb.ink(), 0, "a uniform screen has no ink");

    let r = 0x7600u32;
    Rect::new(10, 10, 20, 30).write(&mut tb.mem, r);
    call(&mut tb, 0xA8A2, &[Arg::L(r)]); // PaintRect
    tb.sync_screen();
    let fore = tb.qd.fore;
    let painted = tb.qd.fb.pixels.iter().filter(|p| **p == fore).count();
    assert_eq!(painted, 10 * 20, "PaintRect should fill exactly its area");
    assert_eq!(tb.qd.fb.get(10, 10), fore);
    assert_eq!(tb.qd.fb.get(9, 9), bg, "must not draw outside the rect");
}

#[test]
fn lines_draw_and_move_the_pen() {
    // The fresh white screen, for the same reason as above: the pen is black.
    let mut tb = Toolbox::new();
    let bg = 0u8;
    call(&mut tb, 0xA893, &[Arg::W(5), Arg::W(5)]); // MoveTo(5,5)
    assert_eq!((tb.qd.pen_h, tb.qd.pen_v), (5, 5));
    call(&mut tb, 0xA891, &[Arg::W(5), Arg::W(15)]); // LineTo(5,15)
    tb.sync_screen();
    assert_eq!((tb.qd.pen_h, tb.qd.pen_v), (5, 15));
    // Vertical line from y=5 up to but not including y=15.
    let fore = tb.qd.fore;
    assert_eq!(tb.qd.fb.pixels.iter().filter(|p| **p == fore).count(), 10);
    assert_eq!(tb.qd.fb.get(5, 5), fore);
    assert_eq!(tb.qd.fb.get(5, 14), fore);
    assert_eq!(tb.qd.fb.get(5, 15), bg, "LineTo excludes the end point");
}

#[test]
fn drawing_is_clipped_to_the_surface() {
    let mut tb = Toolbox::new();
    tb.blank_screen();
    let r = 0x7700u32;
    // Well outside the screen in both directions.
    Rect::new(-100, -100, 10_000, 10_000).write(&mut tb.mem, r);
    call(&mut tb, 0xA8A2, &[Arg::L(r)]);
    tb.sync_screen();
    let fore = tb.qd.fore;
    assert_eq!(
        tb.qd.fb.pixels.iter().filter(|p| **p == fore).count(),
        usize::from(SCREEN_WIDTH) * usize::from(SCREEN_HEIGHT),
        "a huge rect should fill the screen, not overflow it"
    );
}

#[test]
fn regions_allocate_dispose_and_carry_a_bbox() {
    let mut tb = Toolbox::new();
    let regs = call(&mut tb, 0xA8D8, &[]); // NewRgn
    let rgn = tb.mem.read_u32(regs.sp());
    assert_ne!(rgn, 0);

    let r = 0x7800u32;
    Rect::new(1, 2, 3, 4).write(&mut tb.mem, r);
    call(&mut tb, 0xA8DF, &[Arg::L(rgn), Arg::L(r)]); // RectRgn
    let block = tb.mem.deref_handle(rgn).expect("resolve");
    assert_eq!(
        Rect::read(&mut tb.mem, block + quickdraw::RGN_BBOX_OFFSET),
        Rect::new(1, 2, 3, 4)
    );

    call(&mut tb, 0xA8D9, &[Arg::L(rgn)]); // DisposeRgn
    assert_eq!(tb.mem.deref_handle(rgn), None);
}

#[test]
fn an_auto_pop_trap_finds_its_arguments_and_returns_to_the_caller() {
    // Mountains' Think C glue reaches `_ColorUtilities` as `$AC2E`: bit 10 set,
    // the caller's return address pushed on top of the arguments, and a `bsr`
    // dispatch table sitting immediately after the trap word. Two things have to
    // happen and neither is optional.
    //
    // Before this worked, SP+0 held the high half of a return address — zero,
    // because module code sits below 64K of its base — so eight call sites all
    // asking for selector 7 were reported as "selector 0 is not implemented".
    let mut tb = Toolbox::new();
    let hsv = 0x7C00u32;
    let out = 0x7C10u32;
    // A pure hue at full saturation and value: the answer must not be grey.
    tb.mem.write_u16(hsv, 0x5555);
    tb.mem.write_u16(hsv + 2, 0xFFFF);
    tb.mem.write_u16(hsv + 4, 0xFFFF);

    const CALLER: u32 = 0x0009_ABCD & !1; // even, as a 68000 return address must be

    // Push exactly what the glue pushes: src, out, selector, return address.
    let mut sp = SP;
    sp -= 4;
    tb.mem.write_u32(sp, hsv);
    sp -= 4;
    tb.mem.write_u32(sp, out);
    sp -= 2;
    tb.mem.write_u16(sp, 7);
    sp -= 4;
    tb.mem.write_u32(sp, CALLER);

    let mut regs = FakeRegs::new(sp);
    tb.trap(0xAC2E, &mut regs)
        .expect("auto-pop Colour Utilities");

    // The selector was found: a real colour came back, not a refusal.
    let rgb = [
        tb.mem.read_u16(out),
        tb.mem.read_u16(out + 2),
        tb.mem.read_u16(out + 4),
    ];
    assert!(
        rgb.iter().any(|&c| c != rgb[0]),
        "a saturated hue must not convert to grey: {rgb:?}"
    );

    // The stack is clear of the return address, the selector and both arguments.
    assert_eq!(
        regs.sp(),
        SP,
        "auto-pop must pop everything the glue pushed"
    );

    // And execution resumes at the caller, not at the glue's dispatch table —
    // which would branch straight back into the stub, forever.
    assert_eq!(regs.resume_pc(), Some(CALLER));
}

#[test]
fn a_normal_trap_does_not_redirect_where_it_resumes() {
    // The auto-pop path must be reachable only by the bit that means it. The same
    // call without bit 10 takes its arguments from SP+0 and resumes after the
    // trap word, which is what `resume_pc() == None` says.
    let mut tb = Toolbox::new();
    let hsv = 0x7C20u32;
    let out = 0x7C30u32;
    tb.mem.write_u16(hsv, 0x5555);
    tb.mem.write_u16(hsv + 2, 0xFFFF);
    tb.mem.write_u16(hsv + 4, 0xFFFF);
    let mut sp = SP;
    sp -= 4;
    tb.mem.write_u32(sp, hsv);
    sp -= 4;
    tb.mem.write_u32(sp, out);
    sp -= 2;
    tb.mem.write_u16(sp, 7);
    let mut regs = FakeRegs::new(sp);
    tb.trap(0xA82E, &mut regs).expect("plain Colour Utilities");
    assert_eq!(regs.sp(), SP);
    assert_eq!(regs.resume_pc(), None);
}

#[test]
fn clipping_can_never_reach_after_darks_blank_region() {
    // The screen port's visRgn and clipRgn used to *be* the `blankRgn` handle
    // the host passes to every module. Nothing broke only because `_SetClip` and
    // `_ClipRect` were no-ops. Every module's `DoDrawFrame` opens with
    // `if (EmptyRgn(blankRgn)) return;` — Flying Toasters' does — so the first
    // clip call that wrote through would have stopped the whole fleet animating,
    // silently. This is the guard.
    let mut tb = Toolbox::new();
    let blank = tb.qd.blank_rgn;
    let at = tb.mem.deref_handle(blank).expect("blankRgn") + quickdraw::RGN_BBOX_OFFSET;
    let before = quickdraw::Rect::read(&mut tb.mem, at);
    assert!(!before.is_empty(), "blankRgn starts non-empty");

    // A clip to nothing at all: the most destructive thing a module can ask for.
    let empty = 0x7A00u32;
    quickdraw::Rect::default().write(&mut tb.mem, empty);
    let _ = call(&mut tb, 0xA87B, &[Arg::L(empty)]); // ClipRect
    let _ = call(&mut tb, 0xA879, &[Arg::L(blank)]); // SetClip

    let at = tb.mem.deref_handle(blank).expect("blankRgn") + quickdraw::RGN_BBOX_OFFSET;
    let after = quickdraw::Rect::read(&mut tb.mem, at);
    assert_eq!(after, before, "clipping must not touch blankRgn");

    // …and the module's own EmptyRgn(blankRgn) still says "there is screen to
    // blank", which is the question it actually asks.
    let regs = call(&mut tb, 0xA8E2, &[Arg::L(blank)]);
    assert_eq!(
        tb.mem.read_u16(regs.sp()),
        0,
        "blankRgn must not read as empty"
    );

    // The screen port is not the only port. Every `OpenPort`/`OpenCPort` used to
    // get `blankRgn` as its regions too, and that is the path that actually
    // fired: Flying Toasters opens five sprite ports, clips each one, and
    // `blankRgn` came back 32 pixels wide. Its `RandomRect` then divided by
    // (spriteWidth − blankWidth) and took a divide-by-zero exception. The
    // compatibility baseline caught it; this is the unit-level guard.
    for open_trap in [0xA86Fu16, 0xAA00] {
        let port_addr = 0x7B00u32;
        let _ = call(&mut tb, open_trap, &[Arg::L(port_addr)]);
        let sprite = 0x7A40u32;
        quickdraw::Rect::new(0, 0, 32, 32).write(&mut tb.mem, sprite);
        let _ = call(&mut tb, 0xA87B, &[Arg::L(sprite)]); // ClipRect on the new port
        let at = tb.mem.deref_handle(blank).expect("blankRgn") + quickdraw::RGN_BBOX_OFFSET;
        let now = quickdraw::Rect::read(&mut tb.mem, at);
        assert_eq!(
            now, before,
            "clipping port opened by {open_trap:#06x} reached blankRgn"
        );
        // The port's own clip *did* take the value, so this is not a no-op.
        let clip = tb.mem.read_u32(port_addr + crate::port::port::CLIP_RGN);
        let at = tb.mem.deref_handle(clip).expect("port clip") + quickdraw::RGN_BBOX_OFFSET;
        assert_eq!(
            quickdraw::Rect::read(&mut tb.mem, at),
            quickdraw::Rect::new(0, 0, 32, 32)
        );
        let _ = call(&mut tb, 0xA87D, &[Arg::L(port_addr)]); // ClosePort
    }
}

#[test]
fn get_clip_returns_what_clip_rect_set() {
    // GetClip used to leave the caller's region untouched, so a save/restore pair
    // restored whatever was already there.
    let mut tb = Toolbox::new();
    let want = quickdraw::Rect::new(10, 20, 30, 40);
    let r = 0x7A20u32;
    want.write(&mut tb.mem, r);
    let _ = call(&mut tb, 0xA87B, &[Arg::L(r)]); // ClipRect(10,20,30,40)

    let saved = quickdraw::QuickDraw::full_screen_region(&mut tb.mem);
    let _ = call(&mut tb, 0xA87A, &[Arg::L(saved)]); // GetClip(saved)
    let at = tb.mem.deref_handle(saved).expect("saved") + quickdraw::RGN_BBOX_OFFSET;
    let got = quickdraw::Rect::read(&mut tb.mem, at);
    assert_eq!(got, want);
}

#[test]
fn blank_rgn_bbox_is_the_full_screen() {
    let mut tb = Toolbox::new();
    let rgn = tb.qd.blank_rgn;
    let block = tb.mem.deref_handle(rgn).expect("blankRgn must resolve");
    let bbox = Rect::read(&mut tb.mem, block + quickdraw::RGN_BBOX_OFFSET);
    assert_eq!(
        bbox,
        Rect::new(0, 0, SCREEN_HEIGHT as i16, SCREEN_WIDTH as i16),
        "modules read screen bounds out of blankRgn's rgnBBox"
    );
    assert_eq!(tb.mem.read_u16(block), quickdraw::RGN_HEADER_SIZE as u16);
}

// ---------------------------------------------------------------- stack hygiene

#[test]
fn every_toolbox_trap_balances_the_stack() {
    // A trap that leaves the stack unbalanced corrupts the caller's frame and
    // surfaces much later as nonsense — or, as happened here, as a module that
    // runs and then never returns. Check the arithmetic explicitly.
    //
    // Convention: the caller reserves `result_bytes`, pushes args, and the
    // routine pops only the args, leaving SP at the result slot.
    let mut tb = Toolbox::new();
    let r = 0x7900u32;
    Rect::new(0, 0, 4, 4).write(&mut tb.mem, r);
    // A separate 16-byte scratch for the VAR parameters (Point, KeyMap,
    // EventRecord), so a routine that writes through one cannot quietly clobber
    // the Rect the later cases depend on.
    let out = 0x7920u32;

    let cases: Vec<(u16, Vec<Arg>, u32)> = vec![
        (0xA861, vec![], 2),                                // Random -> INTEGER
        (0xA975, vec![], 4),                                // TickCount -> LONGINT
        (0xA973, vec![], 2),                                // StillDown -> BOOLEAN
        (0xA974, vec![], 2),                                // Button -> BOOLEAN
        (0xA977, vec![], 2),                                // WaitMouseUp -> BOOLEAN
        (0xA972, vec![Arg::L(out)], 0),                     // GetMouse(VAR Point)
        (0xA976, vec![Arg::L(out)], 0),                     // GetKeys(VAR KeyMap)
        (0xA970, vec![Arg::W(0), Arg::L(out)], 2),          // GetNextEvent -> BOOLEAN
        (0xA971, vec![Arg::W(0), Arg::L(out)], 2),          // EventAvail -> BOOLEAN
        (0xA893, vec![Arg::W(1), Arg::W(2)], 0),            // MoveTo
        (0xA891, vec![Arg::W(1), Arg::W(2)], 0),            // LineTo
        (0xA8A2, vec![Arg::L(r)], 0),                       // PaintRect
        (0xA8A1, vec![Arg::L(r)], 0),                       // FrameRect
        (0xA8A3, vec![Arg::L(r)], 0),                       // EraseRect
        (0xA8AE, vec![Arg::L(r)], 2),                       // EmptyRect -> BOOLEAN
        (0xA8A6, vec![Arg::L(r), Arg::L(r)], 2),            // EqualRect -> BOOLEAN
        (0xA8A8, vec![Arg::L(r), Arg::W(0), Arg::W(0)], 0), // OffsetRect
        (0xA8A9, vec![Arg::L(r), Arg::W(0), Arg::W(0)], 0), // InsetRect
        (0xA9C8, vec![Arg::W(1)], 0),                       // SysBeep
        (0xA8D8, vec![], 4),                                // NewRgn -> RgnHandle
        (0xA862, vec![Arg::L(33)], 0),                      // ForeColor
        (0xA89E, vec![], 0),                                // PenNormal
        (0xA850, vec![], 0),                                // InitCursor
    ];

    for (trap, args, result_bytes) in cases {
        let arg_bytes: u32 = args
            .iter()
            .map(|a| match a {
                Arg::W(_) => 2,
                Arg::L(_) => 4,
            })
            .sum();
        let slot = SP - result_bytes;
        let mut sp = slot;
        for a in &args {
            match a {
                Arg::W(v) => {
                    sp -= 2;
                    tb.mem.write_u16(sp, *v as u16);
                }
                Arg::L(v) => {
                    sp -= 4;
                    tb.mem.write_u32(sp, *v);
                }
            }
        }
        assert_eq!(sp, slot - arg_bytes, "test setup");
        let mut regs = FakeRegs::new(sp);
        tb.trap(trap, &mut regs)
            .unwrap_or_else(|e| panic!("{trap:#06x}: {}", e.detail));
        assert_eq!(
            regs.sp(),
            slot,
            "{trap:#06x} left the stack unbalanced (args={arg_bytes}, result={result_bytes})"
        );
    }
}

#[test]
fn ticks_advance_and_are_visible_to_modules() {
    let mut tb = Toolbox::new();
    for _ in 0..5 {
        tb.tick();
    }
    let regs = call(&mut tb, 0xA975, &[]); // TickCount
    assert_eq!(tb.mem.read_u32(regs.sp()), 5);
    assert_eq!(tb.mem.read_u32(globals::TICKS), 5);
}

#[test]
fn the_event_manager_block_is_where_the_modules_say_it_is() {
    // The trap numbers here were wrong — shifted by two — and 48 modules paid
    // for it: `TickCount` sat at $A973 and answered a two-byte BOOLEAN, so every
    // module pacing itself on the tick count saw time stopped at 0 and froze
    // after one frame. The table is not the evidence; the call sites are. Each
    // assertion below names the module whose call shape proves the number, so
    // re-shifting the block fails here rather than 66 modules later.
    let mut tb = Toolbox::new();
    for _ in 0..7 {
        tb.tick();
    }

    // Bogglins, ten sites: `clr.l -(a7); trap; move.l (a7)+,field` — a four-byte
    // result with no arguments, which within this block is only TickCount.
    let regs = call(&mut tb, 0xA975, &[]);
    assert_eq!(tb.mem.read_u32(regs.sp()), 7, "$A975 must be TickCount");

    // Strange Attractors passes a 16-byte local and tests bit 1 of the long at
    // +4 — a KeyMap, so $A976 is GetKeys and it must copy all four longs.
    let out = 0x7940u32;
    for i in 0..16u32 {
        tb.mem.write_u8(globals::KEY_MAP + i, 0xA0 | i as u8);
        tb.mem.write_u8(out + i, 0);
    }
    let _ = call(&mut tb, 0xA976, &[Arg::L(out)]);
    for i in 0..16u32 {
        assert_eq!(
            tb.mem.read_u8(out + i),
            0xA0 | i as u8,
            "$A976 KeyMap byte {i}"
        );
    }

    // The After Dark control panel reads a *long* (a Point) back out of the
    // local it passes to $A972, so that is GetMouse — and a Point is { v; h }.
    tb.mouse = (123, 45);
    let _ = call(&mut tb, 0xA972, &[Arg::L(out)]);
    assert_eq!(tb.mem.read_u16(out), 45, "$A972 GetMouse writes v first");
    assert_eq!(
        tb.mem.read_u16(out + 2),
        123,
        "$A972 GetMouse writes h second"
    );

    // Monitors pushes (result, mask, EventRecord*) at $A970 and $A971 and reads
    // a BOOLEAN back: GetNextEvent and EventAvail. No event is ever available in
    // a screen saver, and the record comes back as a real null event.
    for trap in [0xA970u16, 0xA971] {
        tb.mem.write_u16(out, 0xFFFF);
        let regs = call(&mut tb, trap, &[Arg::W(-1), Arg::L(out)]);
        assert_eq!(
            tb.mem.read_u16(regs.sp()),
            0,
            "{trap:#06x} must report no event"
        );
        assert_eq!(tb.mem.read_u16(out), 0, "{trap:#06x} leaves a nullEvent");
        assert_eq!(tb.mem.read_u32(out + 6), 7, "{trap:#06x} fills in `when`");
        assert_eq!(
            tb.mem.read_u16(out + 10),
            45,
            "{trap:#06x} fills in `where.v`"
        );
    }

    // StillDown, Button and WaitMouseUp are all no-argument BOOLEANs, and the
    // button is never down.
    for trap in [0xA973u16, 0xA974, 0xA977] {
        let regs = call(&mut tb, trap, &[]);
        assert_eq!(tb.mem.read_u16(regs.sp()), 0, "{trap:#06x} says button up");
    }
}

#[test]
fn boolean_results_live_in_the_high_byte() {
    // The caller reads a BOOLEAN with `MOVE.B (A7)+,Dn`, which on the 68000 takes
    // the byte at the even address — the high byte of the word. Writing it in the
    // low byte makes every predicate read as false, and Lunatic Fringe refused to
    // start because `EmptyRect` appeared to say "not empty".
    let mut tb = Toolbox::new();
    let r = 0x0050_0000u32;
    Rect::default().write(&mut tb.mem, r); // empty

    let (regs, slot) = call_fn(&mut tb, 0xA8AE, &[Arg::L(r)], 2);
    assert_eq!(regs.sp(), slot);
    assert_eq!(tb.mem.read_u8(slot), 1, "true must be in the high byte");
    assert_eq!(tb.mem.read_u8(slot + 1), 0, "low byte must be clear");
    assert_eq!(tb.mem.read_u16(slot), 0x0100);

    // And false must be all zero.
    Rect::new(0, 0, 10, 10).write(&mut tb.mem, r); // not empty
    let (_, slot) = call_fn(&mut tb, 0xA8AE, &[Arg::L(r)], 2);
    assert_eq!(tb.mem.read_u16(slot), 0x0000);
}

#[test]
fn after_dark_presence_handshake_answers_through_the_event_record() {
    // AFTERDARKEXISTS stuffs 'aYmm' into EventRecord.message, calls GetOSEvent
    // with a mask of 0, and expects After Dark's patch to answer in place. 38
    // modules refused to start until this worked.
    let mut tb = Toolbox::new();
    let evt = 0x0051_0000u32;
    for i in 0..16 {
        tb.mem.write_u8(evt + i, 0);
    }
    tb.mem
        .write_u32(evt + ad_detect::EVT_MESSAGE, ad_detect::MAGIC_REQUEST);

    let mut regs = FakeRegs::new(SP);
    regs.set_addr(0, evt);
    regs.set_data(0, 0); // event mask 0
    tb.trap(0xA031, &mut regs).expect("GetOSEvent");

    assert_eq!(
        tb.mem.read_u32(evt + ad_detect::EVT_MESSAGE),
        ad_detect::MAGIC_REPLY,
        "message must become 'ADrk'"
    );
    let info = tb.mem.read_u32(evt + ad_detect::EVT_WHERE);
    assert_ne!(info, 0, "where must carry the info pointer");
    assert_eq!(
        tb.mem.read_u16(info + ad_detect::INFO_VERSION),
        ad_detect::AD_VERSION,
        "the version the module compares against"
    );
    // Modules mask the top three bytes, so only those need to match.
    assert_eq!(ad_detect::MAGIC_REPLY & 0xFFFF_FF00, 0x4144_7200);
}

#[test]
fn a_plain_get_os_event_reports_a_null_event() {
    // The handshake cookie must be read *before* anything is written, or the
    // module's request is destroyed and it concludes After Dark is not running.
    // Once that is established, a non-handshake call gets what a real Mac gives
    // it: FALSE, plus a filled-in null event. An earlier version of this test
    // asserted the record was left completely untouched, which was a statement
    // about the implementation rather than about the Event Manager.
    let mut tb = Toolbox::new();
    for _ in 0..3 {
        tb.tick();
    }
    let evt = 0x0052_0000u32;
    tb.mem.write_u32(evt + ad_detect::EVT_MESSAGE, 0xDEAD_BEEF);
    tb.mem.write_u16(evt, 0xFFFF);
    let mut regs = FakeRegs::new(SP);
    regs.set_addr(0, evt);
    tb.trap(0xA031, &mut regs).expect("GetOSEvent");
    assert_eq!(regs.data(0), 0, "no event available");
    assert_eq!(tb.mem.read_u16(evt), 0, "what = nullEvt");
    assert_eq!(tb.mem.read_u32(evt + ad_detect::EVT_MESSAGE), 0);
    assert_eq!(tb.mem.read_u32(evt + 6), 3, "when = TickCount");
}

#[test]
fn the_handshake_cookie_survives_and_is_answered() {
    let mut tb = Toolbox::new();
    let evt = 0x0052_0000u32;
    tb.mem
        .write_u32(evt + ad_detect::EVT_MESSAGE, ad_detect::MAGIC_REQUEST);
    let mut regs = FakeRegs::new(SP);
    regs.set_addr(0, evt);
    tb.trap(0xA031, &mut regs).expect("GetOSEvent");
    assert_eq!(
        tb.mem.read_u32(evt + ad_detect::EVT_MESSAGE) & 0xFFFF_FF00,
        0x4144_7200,
        "the cookie must be answered, not overwritten with a null event"
    );
    let info = tb.mem.read_u32(evt + ad_detect::EVT_WHERE);
    assert_eq!(
        tb.mem.read_u16(info + ad_detect::INFO_VERSION),
        ad_detect::AD_VERSION
    );
}

#[test]
fn flag_bits_are_modifiers_only_for_the_allocators() {
    // $A01D is ReserveMem and $A11D is MaxMem: different calls, not variants.
    // Folding them together silently merged two unrelated traps.
    assert!(Trap::decode(0xA122).flags_are_modifiers(), "NewHandle");
    assert!(Trap::decode(0xA31E).flags_are_modifiers(), "NewPtrClear");
    assert!(!Trap::decode(0xA11D).flags_are_modifiers(), "MaxMem");
    assert!(!Trap::decode(0xA01D).flags_are_modifiers(), "ReserveMem");
    assert!(!Trap::decode(0xA861).flags_are_modifiers(), "Toolbox trap");
}

#[test]
fn max_mem_and_reserve_mem_are_not_confused() {
    let mut tb = Toolbox::new();
    // MaxMem must report a large figure; modules refuse to run on a small one.
    let mut regs = FakeRegs::new(SP);
    tb.trap(0xA11D, &mut regs).expect("MaxMem");
    assert!(
        regs.data(0) > 1_000_000,
        "MaxMem reported only {} bytes",
        regs.data(0)
    );
    // ReserveMem is a different call and must not answer with a size.
    let mut regs = FakeRegs::new(SP);
    tb.trap(0xA01D, &mut regs).expect("ReserveMem");
    assert_eq!(regs.data(0), 0, "ReserveMem returns an OSErr");
}

#[test]
fn sane_is_reachable_through_the_dispatcher() {
    // Rainstorm's real call: opword $200E = FFINT | FOZ2X, integer -> extended.
    let mut tb = Toolbox::new();
    let (src, dst) = (0x0053_0000u32, 0x0053_0010u32);
    tb.mem.write_u16(src, 300);
    // Push src, dst, then the opword.
    let mut sp = SP;
    sp -= 4;
    tb.mem.write_u32(sp, src);
    sp -= 4;
    tb.mem.write_u32(sp, dst);
    sp -= 2;
    tb.mem.write_u16(sp, 0x200E);
    let mut regs = FakeRegs::new(sp);
    tb.trap(0xA9EB, &mut regs).expect("FP68K");
    assert_eq!(sane::read_ext(&mut tb.mem, dst), 300.0);
    assert_eq!(regs.sp(), SP, "FP68K pops everything it was given");
}

/// A full-screen painted oval must not overflow its scanline arithmetic.
///
/// `(x+1)^2 * b^2` is quartic in the radii: for a 640x480 oval it reaches about
/// 5.9e9, past `i32::MAX`. Pearls is the first module to paint one that big, and
/// the two builds failed differently — debug aborted the process, release wrapped
/// and compared against a negative product, ending the scanline at the wrong `x`.
/// The silent one is worse, so this asserts the *shape*, not merely that it ran.
///
/// Runs with overflow checks on under `cargo test`, so a regression to `i32`
/// panics here rather than drawing quietly wrong pixels.
#[test]
fn a_full_screen_oval_paints_without_overflowing() {
    let mut tb = Toolbox::new();
    let r = 0x0055_0000u32;
    // The whole 640x480 screen, which is what Pearls asks for.
    tb.mem.write_u16(r, 0); // top
    tb.mem.write_u16(r + 2, 0); // left
    tb.mem.write_u16(r + 4, 480); // bottom
    tb.mem.write_u16(r + 6, 640); // right

    // Index 0 is *white* on a Mac and is also the default `fore`, so painting
    // with it onto a blanked screen would be invisible whether or not the
    // arithmetic was right. Paint in black and count that.
    const BLACK: u8 = 255;
    tb.qd.fore = BLACK;

    let mut sp = SP;
    sp -= 4;
    tb.mem.write_u32(sp, r);
    let mut regs = FakeRegs::new(sp);
    tb.trap(0xA8B8, &mut regs).expect("PaintOval"); // no panic == no overflow

    // Ovals go straight into emulated memory; the framebuffer is a cache.
    tb.sync_screen();
    let painted = tb.qd.fb.pixels.iter().filter(|&&p| p == BLACK).count();

    // An inscribed ellipse covers pi/4 of its bounding box, ~78.5%. Checking the
    // *area* is what makes this more than a crash test: the release build did not
    // panic, it ended each scanline at the wrong x.
    let expected = (640.0 * 480.0 * std::f64::consts::FRAC_PI_4) as usize;
    let (low, high) = (expected * 96 / 100, expected * 104 / 100);
    assert!(
        (low..=high).contains(&painted),
        "filled {painted} pixels, expected about {expected} for an inscribed ellipse"
    );

    // The widest scanline is the centre one, and it must span the full width.
    let centre = (0..640).filter(|&x| tb.qd.fb.get(x, 240) == BLACK).count();
    assert!(
        centre >= 636,
        "the centre scanline spanned only {centre} of 640 pixels"
    );
}

/// A SANE comparison answers through the **condition codes**, not just `D0`.
///
/// SunBurst is `while (angle > limit) angle -= step`, with the comparison in
/// `FP68K` and a `bgt` on the next instruction. Setting only `D0` left that
/// branch reading pre-trap flags, and the loop never terminated — half a million
/// FP68K calls before the cycle budget called it a hang.
#[test]
fn a_sane_comparison_publishes_condition_codes() {
    use ad_m68k::ccr;
    use std::cmp::Ordering;

    // FOCMP with an integer source: opword $2008 = FFINT | FOCMP, exactly
    // SunBurst's. `dst` is the extended left operand, `src` the int16 right.
    let compare = |left: f64, right: i16| -> (u8, u32) {
        let mut tb = Toolbox::new();
        let (src, dst) = (0x0053_0000u32, 0x0053_0010u32);
        tb.mem.write_u16(src, right as u16);
        sane::write_ext(&mut tb.mem, dst, left);
        let mut sp = SP;
        sp -= 4;
        tb.mem.write_u32(sp, src);
        sp -= 4;
        tb.mem.write_u32(sp, dst);
        sp -= 2;
        tb.mem.write_u16(sp, 0x2008);
        let mut regs = FakeRegs::new(sp);
        tb.trap(0xA9EB, &mut regs).expect("FP68K FOCMP");
        assert_eq!(regs.sp(), SP, "FOCMP pops everything it was given");
        (
            regs.condition_codes()
                .expect("a comparison must set the CCR"),
            regs.data(0),
        )
    };

    // `bgt` is `Z == 0 && N == V`; `blt` is `N != V`.
    let bgt = |c: u8| c & ccr::ZERO == 0 && (c & ccr::NEGATIVE != 0) == (c & ccr::OVERFLOW != 0);
    let blt = |c: u8| (c & ccr::NEGATIVE != 0) != (c & ccr::OVERFLOW != 0);
    let beq = |c: u8| c & ccr::ZERO != 0;

    let (greater, d0) = compare(400.0, 360);
    assert!(
        bgt(greater) && !blt(greater) && !beq(greater),
        "{greater:#04x}"
    );
    assert_eq!(
        d0, 1,
        "D0 mirrors the ordering for glue that tests a register"
    );

    let (less, d0) = compare(10.0, 360);
    assert!(blt(less) && !bgt(less) && !beq(less), "{less:#04x}");
    assert_eq!(d0 as i32, -1);

    let (equal, d0) = compare(360.0, 360);
    assert!(beq(equal) && !bgt(equal) && !blt(equal), "{equal:#04x}");
    assert_eq!(d0, 0);

    // Unordered is a fourth outcome, and must never read as "greater" — that is
    // what would spin `while (x > limit)` forever on a NaN.
    let (unordered, _) = compare(f64::NAN, 360);
    assert!(
        !bgt(unordered),
        "a NaN must not satisfy bgt: {unordered:#04x}"
    );
    assert!(blt(unordered), "unordered reads as less: {unordered:#04x}");
    assert_eq!(
        sane::comparison_ccr(sane::SaneResult::Unordered),
        ccr::OVERFLOW
    );
    // And the ordinary orderings keep their documented encoding.
    assert_eq!(
        sane::comparison_ccr(sane::SaneResult::Compared(Ordering::Greater)),
        0
    );
}

/// The screen is a `clutType` device, so its `PixMap` must carry a real palette.
///
/// Supernova walks `GetMaxDevice()` → `gdPMap` → `pmTable` → `ctSize` and divides
/// by a quarter of it. With `pmTable` nil that dereferenced address zero, read an
/// exception vector as a `ColorTable`, and divided by zero.
#[test]
fn the_screen_device_has_a_full_colour_table() {
    use crate::port::ctab;

    let mut tb = Toolbox::new();
    let gd = tb.mem.deref_handle(tb.screen.device).expect("GDevice");
    assert_eq!(
        tb.mem.read_u16(gd + crate::port::gdevice::GD_TYPE),
        2,
        "clutType"
    );

    // Walk it exactly as the module does, handle by handle.
    let pm = tb.mem.read_u32(gd + crate::port::gdevice::GD_PMAP);
    let pmp = tb.mem.deref_handle(pm).expect("PixMap");
    let ct = tb.mem.read_u32(pmp + crate::port::pixmap::PM_TABLE);
    assert_ne!(ct, 0, "pmTable must not be nil on a clut device");
    let ctp = tb.mem.deref_handle(ct).expect("ColorTable");

    // ctSize is entries MINUS ONE: 255, not 256.
    assert_eq!(tb.mem.read_u16(ctp + ctab::CT_SIZE), 255);
    assert_ne!(
        tb.mem.read_u32(ctp + ctab::CT_SEED),
        0,
        "a zero seed is indistinguishable from an uninitialised table"
    );
    // Supernova's actual arithmetic: ctSize / 4 becomes a divisor.
    assert_eq!(tb.mem.read_u16(ctp + ctab::CT_SIZE) / 4, 63);

    // Each ColorSpec carries its own index, and 8-bit channels are promoted by
    // replication so white is exactly 0xFFFF rather than 0xFF00.
    let spec = |i: u32| ctp + ctab::CT_TABLE + i * ctab::SPEC_SIZE;
    assert_eq!(tb.mem.read_u16(spec(0)), 0);
    assert_eq!(tb.mem.read_u16(spec(17)), 17);
    let white = tb.qd.fb.palette.first().copied().expect("palette entry 0");
    if white == [0xFF, 0xFF, 0xFF] {
        assert_eq!(tb.mem.read_u16(spec(0) + 2), 0xFFFF, "white must be full");
    }
}

/// `SetEntries` must move the emulated `ColorTable`, not only the host palette.
#[test]
fn set_entries_writes_through_to_the_colour_table() {
    use crate::port::ctab;

    let mut tb = Toolbox::new();
    let ct = tb.screen.color_table;
    let ctp = tb.mem.deref_handle(ct).expect("ColorTable");
    let seed_before = tb.mem.read_u32(ctp + ctab::CT_SEED);

    // One ColorSpec: index ignored (start = 7), pure green.
    let table = 0x0054_0000u32;
    tb.mem.write_u16(table, 0);
    tb.mem.write_u16(table + 2, 0x0000);
    tb.mem.write_u16(table + 4, 0xFFFF);
    tb.mem.write_u16(table + 6, 0x0000);

    let mut sp = SP;
    sp -= 2;
    tb.mem.write_u16(sp, 7); // start
    sp -= 2;
    tb.mem.write_u16(sp, 0); // count - 1, so one entry
    sp -= 4;
    tb.mem.write_u32(sp, table);
    let mut regs = FakeRegs::new(sp);
    tb.trap(0xAA3F, &mut regs).expect("SetEntries");

    assert_eq!(tb.qd.fb.palette[7], [0, 0xFF, 0], "host palette updated");
    let spec = ctp + ctab::CT_TABLE + 7 * ctab::SPEC_SIZE;
    assert_eq!(tb.mem.read_u16(spec + 2), 0x0000);
    assert_eq!(tb.mem.read_u16(spec + 4), 0xFFFF, "table must follow");
    assert_eq!(tb.mem.read_u16(spec + 6), 0x0000);
    assert_ne!(
        tb.mem.read_u32(ctp + ctab::CT_SEED),
        seed_before,
        "the seed is how code caching against the table learns it changed"
    );
}

#[test]
fn high_scores_survive_the_rmve_add_release_cycle() {
    // Lunatic Fringe saves scores exactly this way: RmveResource the old
    // 'LFhs' 128, build a fresh handle, AddResource, ReleaseResource. The
    // store must hold the new bytes afterwards even though the handle died.
    let mut tb = Toolbox::new();
    tb.resources = resources::ResourceStore::new(vec![resources::StoredResource::synthetic(
        *b"LFhs",
        128,
        None,
        vec![0xAA; 16],
    )]);

    // GetResource('LFhs', 128).
    let old = tb.resources.get(&mut tb.mem, b"LFhs", 128);
    assert_ne!(old, 0, "the seeded resource must load");
    // RmveResource(old): detached, handle still the caller's.
    tb.resources.remove_by_handle(&mut tb.mem, old);
    assert!(
        tb.resources.bytes_of(b"LFhs", 128).is_none(),
        "removed from the map"
    );

    // Fresh handle with the new score table.
    let new = tb.mem.new_handle(240, true);
    let block = tb.mem.deref_handle(new).expect("block");
    tb.mem.write_bytes(block, b"ACES scored here");
    tb.resources.add(&mut tb.mem, *b"LFhs", 128, None, new);
    tb.resources.release(&mut tb.mem, new);

    let saved = tb.resources.bytes_of(b"LFhs", 128).expect("saved");
    assert_eq!(&saved[..16], b"ACES scored here");
    assert_eq!(saved.len(), 240);
    // And a later GetResource loads the new table afresh.
    let again = tb.resources.get(&mut tb.mem, b"LFhs", 128);
    let block = tb.mem.deref_handle(again).expect("reload");
    assert_eq!(&tb.mem.read_bytes(block, 16), b"ACES scored here");
}

#[test]
fn bit_tst_numbers_bits_from_the_high_bit_of_the_first_byte() {
    // BitTst's numbering is the opposite of the obvious one: bit 0 is 0x80 of
    // byte 0. Numbering from the low bit instead makes every flag test read a
    // different bit and fails silently, which is worse than not implementing it.
    let mut tb = Toolbox::new();
    let buf = 0x0057_0000u32;
    tb.mem.write_u8(buf, 0b1000_0001);
    tb.mem.write_u8(buf + 1, 0b0010_0000);

    let test = |tb: &mut Toolbox, bit: u32| -> bool {
        let mut sp = SP;
        sp -= 2; // caller's BOOLEAN slot
        sp -= 4;
        tb.mem.write_u32(sp, buf);
        sp -= 4;
        tb.mem.write_u32(sp, bit);
        let mut regs = FakeRegs::new(sp);
        tb.trap(0xA85D, &mut regs).expect("BitTst");
        // A Pascal BOOLEAN lives in the high byte of its two-byte slot.
        tb.mem.read_u16(regs.sp()) >> 8 != 0
    };

    assert!(test(&mut tb, 0), "bit 0 is 0x80 of byte 0");
    assert!(!test(&mut tb, 1));
    assert!(test(&mut tb, 7), "bit 7 is 0x01 of byte 0");
    assert!(test(&mut tb, 10), "bit 10 is 0x20 of byte 1");
    assert!(!test(&mut tb, 8));
}

#[test]
fn hsv_to_rgb_matches_the_wheel_at_the_primaries() {
    use quickdraw::hsv_to_rgb;
    // Fully saturated, full value: pure hues at 0, 120 and 240 degrees. The
    // thirds are not representable in 16 bits (65536/3 is not an integer), so
    // the off-primary channels land within a couple of counts of zero rather
    // than exactly on it. Asserting exact zeros here would be asserting a
    // rounding accident, not the conversion.
    const NEAR: u16 = 4;
    let close = |got: [u16; 3], want: [u16; 3], what: &str| {
        for i in 0..3 {
            let d = got[i].abs_diff(want[i]);
            assert!(
                d <= NEAR,
                "{what}: channel {i} was {got:?}, wanted {want:?}"
            );
        }
    };
    close(
        hsv_to_rgb([0, 0xFFFF, 0xFFFF]),
        [0xFFFF, 0, 0],
        "red at hue 0",
    );
    close(
        hsv_to_rgb([0x5555, 0xFFFF, 0xFFFF]),
        [0, 0xFFFF, 0],
        "green at one third of the wheel",
    );
    close(
        hsv_to_rgb([0xAAAA, 0xFFFF, 0xFFFF]),
        [0, 0, 0xFFFF],
        "blue at two thirds",
    );
    // Zero saturation is grey at the value level, whatever the hue.
    assert_eq!(hsv_to_rgb([0x1234, 0, 0x8000]), [0x8000; 3]);
    // Zero value is black.
    assert_eq!(hsv_to_rgb([0x4000, 0xFFFF, 0]), [0, 0, 0]);
}

#[test]
fn a_recorded_polygon_fills_its_interior_and_not_its_outside() {
    // OpenPoly makes LineTo record vertices instead of drawing; ClosePoly
    // writes the point list and bounding box; PaintPoly scanline-fills it.
    let mut tb = Toolbox::new();
    tb.qd.cur_port = 0; // draw straight to the screen surface

    let call = |tb: &mut Toolbox, trap: u16, args: &[u32]| {
        let mut sp = SP;
        for a in args {
            sp -= 4;
            tb.mem.write_u32(sp, *a);
        }
        let mut regs = FakeRegs::new(sp);
        tb.trap(trap, &mut regs).expect("trap");
        regs
    };
    let move_to = |tb: &mut Toolbox, h: i16, v: i16| {
        let mut sp = SP;
        sp -= 2;
        tb.mem.write_u16(sp, v as u16);
        sp -= 2;
        tb.mem.write_u16(sp, h as u16);
        // MoveTo pops h then v, so h must be nearest SP.
        let mut sp2 = SP;
        sp2 -= 2;
        tb.mem.write_u16(sp2, v as u16);
        sp2 -= 2;
        tb.mem.write_u16(sp2, h as u16);
        let mut regs = FakeRegs::new(sp2);
        tb.trap(0xA893, &mut regs).expect("MoveTo");
    };
    let line_to = |tb: &mut Toolbox, h: i16, v: i16| {
        let mut sp = SP;
        sp -= 2;
        tb.mem.write_u16(sp, v as u16);
        sp -= 2;
        tb.mem.write_u16(sp, h as u16);
        let mut regs = FakeRegs::new(sp);
        tb.trap(0xA891, &mut regs).expect("LineTo");
    };

    // A right triangle (10,10) (30,10) (10,30).
    move_to(&mut tb, 10, 10);
    let regs = call(&mut tb, 0xA8CB, &[]); // OpenPoly
    let poly = tb.mem.read_u32(regs.sp());
    assert_ne!(poly, 0, "OpenPoly returns a handle");
    line_to(&mut tb, 30, 10);
    line_to(&mut tb, 10, 30);
    line_to(&mut tb, 10, 10);
    call(&mut tb, 0xA8CC, &[]); // ClosePoly

    // Recording must not have marked the screen.
    let surface = tb.qd.screen_surface();
    assert_eq!(
        surface.get(&mut tb.mem, 12, 12),
        Some(0),
        "OpenPoly recording must not draw"
    );

    tb.qd.fore = 200;
    tb.qd.back = 0;
    call(&mut tb, 0xA8C7, &[poly]); // PaintPoly

    let surface = tb.qd.screen_surface();
    assert_eq!(
        surface.get(&mut tb.mem, 12, 12),
        Some(200),
        "inside the triangle"
    );
    assert_eq!(
        surface.get(&mut tb.mem, 28, 28),
        Some(0),
        "beyond the hypotenuse"
    );
    assert_eq!(surface.get(&mut tb.mem, 5, 5), Some(0), "outside entirely");
}

#[test]
fn set_origin_renumbers_where_drawing_lands() {
    // SetOrigin(h, v) makes drawing at (h, v) hit the port's top-left pixel.
    // Ignoring it left Clock drawing a correct clock face clipped into the
    // screen's corner; honouring it takes Clock from 4,224 to 16,294 ink.
    let mut tb = Toolbox::new();
    let screen = tb.screen.port;
    tb.set_port(screen);

    // Pascal pushes left to right, so `h` goes on first and `v` ends up nearest
    // SP. `call` already encodes that; hand-rolling it swapped the two.
    let origin = |tb: &mut Toolbox, h: i16, v: i16| {
        let regs = call(tb, 0xA878, &[Arg::W(h), Arg::W(v)]);
        assert_eq!(regs.sp(), SP, "SetOrigin pops both arguments");
    };

    // Before: coordinate (0,0) is the top-left pixel.
    tb.qd.fore = 200;
    let before = tb.qd.dest(&mut tb.mem);
    assert_eq!(before.bounds.left, 0);

    // Shift so that (100, 50) names the top-left pixel.
    origin(&mut tb, 100, 50);
    let after = tb.qd.dest(&mut tb.mem);
    assert_eq!(
        (after.bounds.left, after.bounds.top),
        (100, 50),
        "the boundary rectangle carries the renumbering"
    );

    // Now drawing at (100, 50) must reach pixel (0, 0), and (0, 0) must fall
    // outside the port entirely rather than wrapping to some other pixel.
    after.set(&mut tb.mem, 100, 50, 77);
    assert_eq!(after.get(&mut tb.mem, 100, 50), Some(77));
    assert_eq!(
        after.get(&mut tb.mem, 0, 0),
        None,
        "coordinates before the new origin are off the port"
    );

    // A redundant SetOrigin to the same place must not shift twice.
    origin(&mut tb, 100, 50);
    let again = tb.qd.dest(&mut tb.mem);
    assert_eq!((again.bounds.left, again.bounds.top), (100, 50));

    // And returning to (0,0) must restore the original mapping exactly.
    origin(&mut tb, 0, 0);
    let restored = tb.qd.dest(&mut tb.mem);
    assert_eq!((restored.bounds.left, restored.bounds.top), (0, 0));
}

#[test]
fn the_wall_clock_advances_with_ticks_from_a_fixed_start() {
    // A frozen clock made Clock draw a correct face whose hands never moved.
    // Time must advance, but from a fixed start so runs stay reproducible.
    let mut tb = Toolbox::new();
    let start = tb.mem.read_u32(ad_memory::globals::TIME);
    assert_ne!(start, 0, "the epoch would read as 12:00 forever");
    for _ in 0..59 {
        tb.tick();
    }
    assert_eq!(
        tb.mem.read_u32(ad_memory::globals::TIME),
        start,
        "59 ticks is less than a second"
    );
    tb.tick();
    assert_eq!(
        tb.mem.read_u32(ad_memory::globals::TIME),
        start + 1,
        "60 ticks is one second"
    );
    for _ in 0..600 {
        tb.tick();
    }
    assert_eq!(tb.mem.read_u32(ad_memory::globals::TIME), start + 11);
}

// ------------------------------------------------- traps found by module 13

#[test]
fn slope_from_angle_returns_fixed_and_saturates_where_the_ray_is_horizontal() {
    // Rainstorm pushes one word and pops a longword from a slot it reserved
    // first, so the trap must consume 2 bytes and leave 4.
    let mut tb = Toolbox::new();
    let (regs, slot) = call_fn(&mut tb, 0xA8BC, &[Arg::W(45)], 4);
    assert_eq!(regs.sp(), slot, "one word in, a Fixed left in the slot");
    // 45 degrees clockwise from twelve: dh/dv = -tan(45) = -1.0 in 16.16.
    assert_eq!(tb.mem.read_u32(slot) as i32, -0x0001_0000);

    for (angle, want) in [(0i16, 0i32), (180, 0), (360, 0)] {
        let (_, slot) = call_fn(&mut tb, 0xA8BC, &[Arg::W(angle)], 4);
        assert_eq!(tb.mem.read_u32(slot) as i32, want, "angle {angle}");
    }
    // At 90 the ray is horizontal, so dv is zero and the slope is unbounded.
    // Rainstorm *divides* by this, and only a saturated value yields the
    // straight-down rain the module means; a zero here would divide by zero.
    let (_, slot) = call_fn(&mut tb, 0xA8BC, &[Arg::W(90)], 4);
    assert_eq!(tb.mem.read_u32(slot) as i32, i32::MAX);
    // 135 mirrors 45 across the vertical, which is what makes Rainstorm's
    // symmetric 30..150 range give symmetric wind.
    let (_, slot) = call_fn(&mut tb, 0xA8BC, &[Arg::W(135)], 4);
    assert_eq!(tb.mem.read_u32(slot) as i32, 0x0001_0000);
}

#[test]
fn plot_icon_scales_a_32x32_one_bit_icon_into_its_rect() {
    let mut tb = Toolbox::new();
    tb.qd.cur_port = 0;
    tb.qd.fore = 200;
    tb.qd.back = 5;

    // An ICON whose left half is set: 4 bytes a row, top two bytes 0xFF.
    let icon = tb.mem.new_handle(128, true);
    let block = tb.mem.deref_handle(icon).expect("block");
    for row in 0..32u32 {
        tb.mem.write_u8(block + row * 4, 0xFF);
        tb.mem.write_u8(block + row * 4 + 1, 0xFF);
    }

    let rect = 0x0050_0000;
    quickdraw::Rect::new(10, 10, 42, 42).write(&mut tb.mem, rect);
    let regs = call(&mut tb, 0xA94B, &[Arg::L(rect), Arg::L(icon)]);
    assert_eq!(
        regs.sp(),
        SP,
        "PlotIcon takes a rect and a handle, nothing back"
    );

    let s = tb.qd.screen_surface();
    assert_eq!(
        s.get(&mut tb.mem, 12, 12),
        Some(200),
        "set bits use the fore colour"
    );
    assert_eq!(
        s.get(&mut tb.mem, 40, 12),
        Some(5),
        "clear bits are srcCopy'd as back"
    );
    assert_eq!(
        s.get(&mut tb.mem, 5, 5),
        Some(0),
        "outside the rect is untouched"
    );
}

/// Build a `cicn` whose mask covers only the left half, in a 4x2 icon.
///
/// Layout as the resource has it: `PixMap`(50), mask `BitMap`(14), b/w
/// `BitMap`(14), `iconData`(4), then mask bits, b/w bits, colour table, pixels.
fn cicn_handle(tb: &mut Toolbox) -> u32 {
    let ct = 8 + 2 * 8; // two ColorSpecs
    let h = tb.mem.new_handle(82 + 2 + 2 + ct + 4, true);
    let b = tb.mem.deref_handle(h).expect("block");
    // iconPMap: 4 bits deep, rowBytes 2, bounds 4x2.
    tb.mem.write_u16(b + 4, 0x8002);
    quickdraw::Rect::new(0, 0, 2, 4).write(&mut tb.mem, b + 6);
    tb.mem.write_u16(b + 32, 4);
    // iconMask and iconBMap: one byte a row, same bounds.
    for at in [b + 50, b + 64] {
        tb.mem.write_u16(at + 4, 1);
        quickdraw::Rect::new(0, 0, 2, 4).write(&mut tb.mem, at + 6);
    }
    let mask = b + 82;
    tb.mem.write_u8(mask, 0xC0); // left two pixels of row 0
    tb.mem.write_u8(mask + 1, 0xC0);
    let bw = mask + 2;
    tb.mem.write_u8(bw, 0xFF);
    tb.mem.write_u8(bw + 1, 0xFF);
    let ct_at = bw + 2;
    tb.mem.write_u16(ct_at + 6, 1); // ctSize: two entries
    tb.mem.write_u16(ct_at + 8, 0);
    tb.mem.write_u16(ct_at + 16, 1);
    for (i, v) in [0u16, 0, 0].iter().enumerate() {
        tb.mem.write_u16(ct_at + 10 + (i as u32) * 2, *v);
    }
    for (i, v) in [0xFFFFu16, 0, 0].iter().enumerate() {
        tb.mem.write_u16(ct_at + 18 + (i as u32) * 2, *v);
    }
    let pix = ct_at + 8 + 2 * 8;
    tb.mem.write_u8(pix, 0x11); // pixels 0,1 = index 1 (red)
    tb.mem.write_u8(pix + 2, 0x11);
    h
}

#[test]
fn plot_cicon_draws_only_where_the_mask_allows() {
    let mut tb = Toolbox::new();
    tb.qd.cur_port = 0;
    let cicon = cicn_handle(&mut tb);
    let red = quickdraw::nearest_in(&tb.qd.fb.palette, [0xFF, 0, 0]);

    let rect = 0x0050_0000;
    quickdraw::Rect::new(4, 4, 6, 8).write(&mut tb.mem, rect);
    let regs = call(&mut tb, 0xAA1F, &[Arg::L(rect), Arg::L(cicon)]);
    assert_eq!(regs.sp(), SP, "PlotCIcon takes a rect and a handle");

    let s = tb.qd.screen_surface();
    assert_eq!(
        s.get(&mut tb.mem, 4, 4),
        Some(red),
        "masked in, colour from the cicn table"
    );
    assert_eq!(
        s.get(&mut tb.mem, 7, 4),
        Some(0),
        "masked out: destination shows through"
    );
}

#[test]
fn get_cicon_is_independent_of_the_resource_it_came_from() {
    // Confetti Factory's SAFEGETCICON releases the 'cicn' resource right after
    // GetCIcon and then plots the icon, so the two must not share a handle.
    let mut tb = Toolbox::new();
    tb.resources = resources::ResourceStore::new(vec![resources::StoredResource::synthetic(
        *b"cicn",
        1111,
        None,
        vec![0x5A; 96],
    )]);
    let (_, slot) = call_fn(&mut tb, 0xAA1E, &[Arg::W(1111)], 4);
    let icon = tb.mem.read_u32(slot);
    assert_ne!(icon, 0, "the seeded cicn must load");
    let resource = tb.resources.get(&mut tb.mem, b"cicn", 1111);
    assert_ne!(
        icon, resource,
        "GetCIcon must not hand back the resource handle"
    );

    tb.resources.release(&mut tb.mem, resource);
    let block = tb
        .mem
        .deref_handle(icon)
        .expect("the icon outlives the release");
    assert_eq!(tb.mem.read_u8(block), 0x5A);
    // DisposCIcon then frees the icon outright.
    let regs = call(&mut tb, 0xAA25, &[Arg::L(icon)]);
    assert_eq!(regs.sp(), SP);
    assert!(tb.mem.deref_handle(icon).is_none(), "DisposCIcon frees it");
}

#[test]
fn copy_mask_protects_the_destination_where_the_mask_is_clear() {
    let mut tb = Toolbox::new();
    let mut lay = |at: u32, base: u32, row: u32, w: i16, h: i16| {
        tb.mem.write_u32(at, base);
        tb.mem.write_u16(at + 4, u16::try_from(row).unwrap_or(0));
        quickdraw::Rect::new(0, 0, h, w).write(&mut tb.mem, at + 6);
    };
    // Three 8x2 one-bit bitmaps: source all ones, mask left half only.
    lay(0x0040_0000, 0x0040_1000, 1, 8, 2);
    lay(0x0040_0100, 0x0040_1100, 1, 8, 2);
    lay(0x0040_0200, 0x0040_1200, 1, 8, 2);
    for row in 0..2 {
        tb.mem.write_u8(0x0040_1000 + row, 0xFF); // src
        tb.mem.write_u8(0x0040_1100 + row, 0xF0); // mask: left four pixels
        tb.mem.write_u8(0x0040_1200 + row, 0x00); // dst
    }
    let r = 0x0050_0000;
    quickdraw::Rect::new(0, 0, 2, 8).write(&mut tb.mem, r);

    tb.qd.fore = 1;
    tb.qd.back = 0;
    let regs = call(
        &mut tb,
        0xA817,
        &[
            Arg::L(0x0040_0000),
            Arg::L(0x0040_0100),
            Arg::L(0x0040_0200),
            Arg::L(r),
            Arg::L(r),
            Arg::L(r),
        ],
    );
    assert_eq!(regs.sp(), SP, "six pointers in, nothing back");
    assert_eq!(
        tb.mem.read_u8(0x0040_1200),
        0xF0,
        "only the pixels the mask allowed were copied"
    );
}

#[test]
fn a_pixel_pattern_carries_the_colour_make_rgb_pat_was_given() {
    let mut tb = Toolbox::new();
    let (regs, slot) = call_fn(&mut tb, 0xAA07, &[], 4);
    assert_eq!(regs.sp(), slot, "NewPixPat takes no arguments");
    let ppat = tb.mem.read_u32(slot);
    assert_ne!(ppat, 0);
    let block = tb.mem.deref_handle(ppat).expect("block");
    assert_eq!(
        tb.mem.read_u16(block),
        1,
        "patType starts as a colour pattern"
    );

    let rgb = 0x0050_0000;
    for (i, v) in [0u16, 0xFFFF, 0].iter().enumerate() {
        tb.mem.write_u16(rgb + (i as u32) * 2, *v);
    }
    let regs = call(&mut tb, 0xAA0D, &[Arg::L(ppat), Arg::L(rgb)]);
    assert_eq!(regs.sp(), SP);
    assert_eq!(tb.mem.read_u16(block), 2, "patType becomes an RGB pattern");

    let want = quickdraw::nearest_in(&tb.qd.fb.palette, [0, 0xFF, 0]);
    let regs = call(&mut tb, 0xAA0A, &[Arg::L(ppat)]);
    assert_eq!(regs.sp(), SP);
    assert_eq!(tb.qd.fore, want, "PenPixPat resolves the pattern's colour");
    assert_eq!(tb.qd.pen_pat, [0xFF; 8], "an RGB pattern is solid");

    let regs = call(&mut tb, 0xAA0B, &[Arg::L(ppat)]);
    assert_eq!(regs.sp(), SP);
    assert_eq!(
        tb.qd.back, want,
        "BackPixPat does the same for the background"
    );

    let regs = call(&mut tb, 0xAA08, &[Arg::L(ppat)]);
    assert_eq!(regs.sp(), SP);
    assert!(tb.mem.deref_handle(ppat).is_none(), "DisposPixPat frees it");
}

#[test]
fn pen_pix_pat_refuses_a_pattern_whose_colour_is_unknown() {
    // A 'ppat' resource loaded by GetPixPat would land here. Painting it with
    // some plausible colour would be a fidelity bug nobody could see.
    let mut tb = Toolbox::new();
    let stray = tb.mem.new_handle(28, true);
    let mut sp = SP;
    sp -= 4;
    tb.mem.write_u32(sp, stray);
    let mut regs = FakeRegs::new(sp);
    let err = tb.trap(0xAA0A, &mut regs).expect_err("must refuse");
    assert!(err.detail.contains("_MakeRGBPat"), "{}", err.detail);
}

/// A 'MENU' resource: header, title, then one record per item.
fn menu_resource(title: &str, items: &[(&str, u8)]) -> Vec<u8> {
    let mut d = vec![0u8; 14];
    d[0..2].copy_from_slice(&128u16.to_be_bytes());
    d.push(u8::try_from(title.len()).unwrap_or(0));
    d.extend_from_slice(title.as_bytes());
    for (text, style) in items {
        d.push(u8::try_from(text.len()).unwrap_or(0));
        d.extend_from_slice(text.as_bytes());
        d.extend_from_slice(&[0, 0, 0, *style]); // icon, key, mark, style
    }
    d.push(0);
    d
}

#[test]
fn a_menu_is_walked_for_its_count_its_item_text_and_its_styles() {
    // Randomizer's module list: GetMenu(128), CountMItems, GetItem per item,
    // ReleaseResource. MultiModule adds GetItmStyle to skip italic entries.
    let mut tb = Toolbox::new();
    tb.resources = resources::ResourceStore::new(vec![resources::StoredResource::synthetic(
        *b"MENU",
        128,
        Some("Modules Menu"),
        menu_resource("Modules:", &[("Rainstorm", 0), ("Satori", 2)]),
    )]);

    let (regs, slot) = call_fn(&mut tb, 0xA9BF, &[Arg::W(128)], 4);
    assert_eq!(regs.sp(), slot, "GetMenu takes a word and leaves a handle");
    let menu = tb.mem.read_u32(slot);
    assert_ne!(menu, 0);

    let (regs, slot) = call_fn(&mut tb, 0xA950, &[Arg::L(menu)], 2);
    assert_eq!(regs.sp(), slot);
    assert_eq!(tb.mem.read_u16(slot), 2, "CountMItems counts the items");

    let out = 0x0050_0000;
    let regs = call(&mut tb, 0xA946, &[Arg::L(menu), Arg::W(1), Arg::L(out)]);
    assert_eq!(
        regs.sp(),
        SP,
        "GetItem is ten bytes of arguments, no result"
    );
    assert_eq!(tb.mem.read_u8(out), 9, "the Str255 length byte");
    assert_eq!(&tb.mem.read_bytes(out + 1, 9), b"Rainstorm");

    call(&mut tb, 0xA946, &[Arg::L(menu), Arg::W(2), Arg::L(out)]);
    assert_eq!(&tb.mem.read_bytes(out + 1, 6), b"Satori");
    // Out of range is the empty string, as the Toolbox does.
    call(&mut tb, 0xA946, &[Arg::L(menu), Arg::W(9), Arg::L(out)]);
    assert_eq!(tb.mem.read_u8(out), 0);

    // GetItmStyle writes a zero-extended word: MultiModule's Think C glue reads
    // the low half of the two bytes it reserved.
    tb.mem.write_u16(out, 0xBEEF);
    let regs = call(&mut tb, 0xA941, &[Arg::L(menu), Arg::W(2), Arg::L(out)]);
    assert_eq!(regs.sp(), SP);
    assert_eq!(tb.mem.read_u16(out), 2, "item 2 is italic");
    call(&mut tb, 0xA941, &[Arg::L(menu), Arg::W(1), Arg::L(out)]);
    assert_eq!(tb.mem.read_u16(out), 0, "item 1 is plain");
}

#[test]
fn the_video_driver_set_entries_control_call_loads_the_palette() {
    // Satori's ROTATECOLORS talks to the screen's driver instead of calling
    // _SetEntries: csCode 3 with { csTable, csStart, csCount }.
    let mut tb = Toolbox::new();
    let table = 0x0050_0000;
    for i in 0..4u32 {
        let spec = table + i * 8;
        tb.mem.write_u16(spec, u16::try_from(i).unwrap_or(0));
        tb.mem.write_u16(spec + 2, 0x1100);
        tb.mem.write_u16(spec + 4, 0x2200);
        tb.mem.write_u16(spec + 6, 0x3300);
    }
    let pb = 0x0050_1000;
    tb.mem.write_u16(pb + cntrl::IO_CREF_NUM, 0);
    tb.mem.write_u16(pb + cntrl::CS_CODE, 3);
    tb.mem.write_u32(pb + cntrl::CS_PARAM, table);
    tb.mem.write_u16(pb + cntrl::CS_PARAM + 4, 8); // csStart
    tb.mem.write_u16(pb + cntrl::CS_PARAM + 6, 3); // csCount: four entries

    let mut regs = FakeRegs::new(SP);
    regs.set_addr(0, pb);
    tb.trap(0xA004, &mut regs).expect("Control");
    assert_eq!(regs.data(0), 0, "noErr in D0");
    assert_eq!(tb.mem.read_u16(pb + cntrl::IO_RESULT), 0, "and in ioResult");
    for i in 8..12 {
        assert_eq!(tb.qd.fb.palette[i], [0x11, 0x22, 0x33], "entry {i}");
    }

    // Any other control code is a hard failure: a video driver call this
    // runtime silently accepted would change the screen invisibly.
    tb.mem.write_u16(pb + cntrl::CS_CODE, 4);
    let mut regs = FakeRegs::new(SP);
    regs.set_addr(0, pb);
    let err = tb.trap(0xA004, &mut regs).expect_err("must refuse");
    assert!(err.detail.contains("csCode 4"), "{}", err.detail);
}

#[test]
fn every_file_manager_call_reports_that_no_volume_is_mounted() {
    // This runtime hands modules a resource store, not a file system. Saying so
    // is the accurate answer; fnfErr would invite PICS Player to create files.
    let mut tb = Toolbox::new();
    let pb = 0x0050_0000;
    for word in [0xA200u16, 0xA20A] {
        tb.mem.write_u16(pb + filemgr::IO_RESULT, 0);
        let mut regs = FakeRegs::new(SP);
        regs.set_addr(0, pb);
        tb.trap(word, &mut regs).expect("handled");
        assert_eq!(regs.data(0) as i16, oserr::NSV_ERR, "{word:#06x} in D0");
        assert_eq!(
            tb.mem.read_u16(pb + filemgr::IO_RESULT) as i16,
            oserr::NSV_ERR
        );
    }
    // _HFSDispatch takes its selector in D0.
    for selector in [8u32, 9] {
        let mut regs = FakeRegs::new(SP);
        regs.set_addr(0, pb);
        regs.set_data(0, selector);
        tb.trap(0xA260, &mut regs).expect("handled");
        assert_eq!(regs.data(0) as i16, oserr::NSV_ERR, "selector {selector}");
    }
    let mut regs = FakeRegs::new(SP);
    regs.set_addr(0, pb);
    regs.set_data(0, 5); // PBCatMove
    let err = tb
        .trap(0xA260, &mut regs)
        .expect_err("must name the selector");
    assert!(err.detail.contains("selector 5"), "{}", err.detail);
}

#[test]
fn an_installed_search_proc_makes_the_red_channel_a_palette_index() {
    // Tunnel's DummyProc is `*position = rgb->red; return true`, and Supernova's
    // does the same with the other channels required to be zero. Both callers
    // then set colours by index through _RGBForeColor.
    let mut tb = Toolbox::new();
    let rgb = 0x0050_0000;
    tb.mem.write_u16(rgb, 137);
    tb.mem.write_u16(rgb + 2, 0);
    tb.mem.write_u16(rgb + 4, 0);

    // Without a search proc, {137, 0, 0} is a near-black red, not index 137.
    call(&mut tb, 0xAA14, &[Arg::L(rgb)]);
    assert_ne!(tb.qd.fore, 137, "nearest-match is the default");

    let proc = 0x0000_9000;
    let regs = call(&mut tb, 0xAA3A, &[Arg::L(proc)]);
    assert_eq!(regs.sp(), SP, "AddSearch takes one pointer");
    call(&mut tb, 0xAA14, &[Arg::L(rgb)]);
    assert_eq!(tb.qd.fore, 137, "the red word is the index");

    // Tunnel's genuine colours go through the same rule: black is index 0 and
    // white saturates at the top of the table, exactly as DummyProc leaves them.
    for (red, want) in [(0u16, 0u8), (0xFFFF, 255), (0x7FFF, 255)] {
        tb.mem.write_u16(rgb, red);
        call(&mut tb, 0xAA14, &[Arg::L(rgb)]);
        assert_eq!(tb.qd.fore, want, "red {red:#06x}");
    }

    let regs = call(&mut tb, 0xAA4C, &[Arg::L(proc)]);
    assert_eq!(regs.sp(), SP, "DelSearch takes one pointer");
    tb.mem.write_u16(rgb, 137);
    call(&mut tb, 0xAA14, &[Arg::L(rgb)]);
    assert_ne!(
        tb.qd.fore, 137,
        "removing the proc restores colour matching"
    );
}

#[test]
fn add_search_refuses_a_second_different_procedure() {
    let mut tb = Toolbox::new();
    call(&mut tb, 0xAA3A, &[Arg::L(0x9000)]);
    call(&mut tb, 0xAA3A, &[Arg::L(0x9000)]);
    let mut sp = SP;
    sp -= 4;
    tb.mem.write_u32(sp, 0xA000);
    let mut regs = FakeRegs::new(sp);
    let err = tb.trap(0xAA3A, &mut regs).expect_err("must refuse");
    assert!(
        err.detail.contains("second search procedure"),
        "{}",
        err.detail
    );
}

#[test]
fn port_reshaping_and_plane_selection_balance_their_stacks() {
    // MovePortTo, PortSize and ColorBit reshape state this framebuffer does not
    // have; getting their argument counts wrong corrupts the caller instead.
    let mut tb = Toolbox::new();
    assert_eq!(call(&mut tb, 0xA877, &[Arg::W(10), Arg::W(20)]).sp(), SP);
    assert_eq!(call(&mut tb, 0xA876, &[Arg::W(64), Arg::W(48)]).sp(), SP);
    assert_eq!(call(&mut tb, 0xA864, &[Arg::W(0)]).sp(), SP);
}

#[test]
fn dispose_ctable_releases_a_table_that_came_from_a_resource() {
    // GeoBounce pairs GetCTable with DisposCTable. GetCTable may hand back the
    // module's own 'clut', so disposal has to go through the store.
    let mut tb = Toolbox::new();
    let mut clut = vec![0u8; 8 + 2 * 8];
    clut[6..8].copy_from_slice(&1u16.to_be_bytes()); // ctSize
    tb.resources = resources::ResourceStore::new(vec![resources::StoredResource::synthetic(
        *b"clut", 128, None, clut,
    )]);
    let (_, slot) = call_fn(&mut tb, 0xAA18, &[Arg::W(128)], 4);
    let table = tb.mem.read_u32(slot);
    assert_ne!(table, 0);
    let regs = call(&mut tb, 0xAA24, &[Arg::L(table)]);
    assert_eq!(regs.sp(), SP);
    assert!(tb.mem.deref_handle(table).is_none(), "the handle is gone");
    // And the store can still reload it.
    assert!(tb.resources.bytes_of(b"clut", 128).is_some());
}

/// `_KeyTranslate` ($A9C3) maps a key code to a character through a `KCHR`.
///
/// It was named `_SystemTask` — which is `$A9B4` — and grouped with the cursor
/// no-ops, so it popped none of its ten bytes of arguments and returned nothing.
/// Lunatic Fringe calls it twelve times to label its configurable controls, and
/// every one of them read "N", for none. The trap number is derived from that call
/// site; see `traps.rs`.
#[test]
fn key_translate_returns_the_character_for_a_key_code() {
    use crate::resources::kchr;

    let mut tb = Toolbox::new();
    // The host's synthesized US layout, as a module would receive it.
    let layout = crate::resources::system_resources()
        .into_iter()
        .find(|r| r.res_type == *b"KCHR")
        .map(|r| r.data)
        .expect("a KCHR is supplied");
    let at = 0x0056_0000u32;
    tb.mem.write_bytes(at, &layout);

    // FUNCTION KeyTranslate(transData: Ptr; keycode: INTEGER;
    //                       VAR state: LONGINT): LONGINT
    // Pushed left to right after a 4-byte result slot, exactly as the game does.
    let state = 0x0057_0000u32;
    let translate = |tb: &mut Toolbox, code: u16| -> u8 {
        let mut sp = SP;
        sp -= 4; // result slot
        tb.mem.write_u32(sp, 0);
        sp -= 4;
        tb.mem.write_u32(sp, at);
        sp -= 2;
        tb.mem.write_u16(sp, code);
        sp -= 4;
        tb.mem.write_u32(sp, state);
        let mut regs = FakeRegs::new(sp);
        tb.trap(0xA9C3, &mut regs).expect("KeyTranslate");
        assert_eq!(
            regs.sp(),
            SP - 4,
            "the LONGINT result must be left on the stack"
        );
        tb.mem.read_u32(SP - 4) as u8
    };

    // The controls Lunatic Fringe actually shows in its key table.
    assert_eq!(translate(&mut tb, 0x25), b'l', "Turn Left");
    assert_eq!(translate(&mut tb, 0x27), b'\'', "Turn Right");
    assert_eq!(translate(&mut tb, 0x29), b';', "Thrust");
    assert_eq!(translate(&mut tb, 0x23), b'p', "Turbo Thrust");
    assert_eq!(translate(&mut tb, 0x31), b' ', "Power Shield");
    assert_eq!(translate(&mut tb, 0x00), b'a', "Abort Ship");
    // Keypad, the other column of that table.
    assert_eq!(translate(&mut tb, 0x56), b'4');
    assert_eq!(translate(&mut tb, 0x58), b'6');
    assert_eq!(translate(&mut tb, 0x57), b'5');
    assert_eq!(translate(&mut tb, 0x52), b'0');

    // Bit 7 is the key-up flag, not part of the code.
    assert_eq!(
        translate(&mut tb, 0x25 | 0x80),
        b'l',
        "key-up still translates"
    );

    // The pure lookup refuses a malformed layout rather than guessing table zero.
    // The selector array is indexed by the *modifier* byte, not the key code.
    let mut broken = layout.clone();
    broken[(kchr::SELECTORS + 9) as usize] = 9; // selector past the table count
    assert_eq!(crate::resources::kchr_char(&broken, 9, 0x25), None);
    // And an unmodified lookup still works, so the guard is not blanket.
    assert_eq!(crate::resources::kchr_char(&broken, 0, 0x25), Some(b'l'));
}
