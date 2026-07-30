#![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use super::*;

#[test]
fn reads_and_writes_are_big_endian() {
    let mut m = Memory::new();
    m.write_u32(0x3000, 0xDEAD_BEEF);
    assert_eq!(m.read_u8(0x3000), 0xDE, "68000 is big-endian");
    assert_eq!(m.read_u8(0x3001), 0xAD);
    assert_eq!(m.read_u8(0x3002), 0xBE);
    assert_eq!(m.read_u8(0x3003), 0xEF);
    assert_eq!(m.read_u16(0x3000), 0xDEAD);
    assert_eq!(m.read_u16(0x3002), 0xBEEF);
    assert_eq!(m.read_u32(0x3000), 0xDEAD_BEEF);
}

#[test]
fn out_of_range_access_records_a_fault_instead_of_panicking() {
    let mut m = Memory::new();
    let wild = RAM_SIZE + 0x1000;
    assert_eq!(m.read_u8(wild), 0, "must return a defined value");
    m.write_u8(wild, 0x42);
    assert_eq!(m.faults.len(), 2);
    assert_eq!(m.faults[0].addr, wild);
    assert!(!m.faults[0].write);
    assert!(m.faults[1].write);
}

#[test]
fn fault_recording_is_capped() {
    let mut m = Memory::new();
    for i in 0..10_000 {
        m.read_u8(RAM_SIZE + i);
    }
    assert!(
        m.faults.len() <= 256,
        "a runaway loop must not exhaust host memory: {}",
        m.faults.len()
    );
}

#[test]
fn new_handle_double_indirection_resolves() {
    let mut m = Memory::new();
    let h = m.new_handle(64, true);
    assert_ne!(h, 0);

    // A handle is the address of a master pointer, which holds the block.
    let master_value = m.read_u32(h);
    let block = m.deref_handle(h).expect("handle should resolve");
    assert_eq!(
        master_value, block,
        "the master pointer in memory must match the recorded block"
    );
    assert_eq!(m.handle_size(h), Some(64));

    // The module writes through the double indirection; we must see it.
    m.write_u32(block, 0x5261_696E); // 'Rain'
    assert_eq!(m.read_u32(block), 0x5261_696E);
}

#[test]
fn new_handle_clear_zeroes_the_block() {
    let mut m = Memory::new();
    // Dirty the heap first so a stale value would show up.
    let dirty = m.new_ptr(64, false);
    for i in 0..64 {
        m.write_u8(dirty + i, 0xAA);
    }
    let h = m.new_handle(64, true);
    let block = m.deref_handle(h).expect("resolve");
    assert!(
        (0..64).all(|i| m.read_u8(block + i) == 0),
        "NewHandleClear must zero the block"
    );
}

#[test]
fn blocks_are_even_aligned() {
    let mut m = Memory::new();
    // An odd size must not leave the next block on an odd address; the 68000
    // takes an address error on odd word accesses.
    for size in [1u32, 3, 5, 7, 9, 11] {
        let p = m.new_ptr(size, false);
        assert_eq!(p % 2, 0, "block at {p:#x} is odd for size {size}");
    }
}

#[test]
fn handles_do_not_overlap() {
    let mut m = Memory::new();
    let mut spans: Vec<(u32, u32)> = Vec::new();
    for size in [16u32, 32, 100, 7, 4096] {
        let h = m.new_handle(size, false);
        let base = m.deref_handle(h).expect("resolve");
        spans.push((base, size));
    }
    for (i, (a, alen)) in spans.iter().enumerate() {
        for (b, blen) in spans.iter().skip(i + 1) {
            let disjoint = a + alen <= *b || b + blen <= *a;
            assert!(disjoint, "blocks {a:#x}+{alen} and {b:#x}+{blen} overlap");
        }
    }
}

#[test]
fn master_pointers_do_not_collide_with_blocks() {
    let mut m = Memory::new();
    for _ in 0..64 {
        let h = m.new_handle(32, false);
        let block = m.deref_handle(h).expect("resolve");
        assert!(
            h < HEAP_BASE,
            "master pointer {h:#x} should live below the heap"
        );
        assert!(
            block >= HEAP_BASE,
            "block {block:#x} should live in the heap"
        );
    }
}

#[test]
fn dispose_handle_stops_it_resolving() {
    let mut m = Memory::new();
    let h = m.new_handle(32, false);
    assert!(m.deref_handle(h).is_some());
    m.dispose_handle(h);
    assert_eq!(
        m.deref_handle(h),
        None,
        "a disposed handle must not resolve — that is what catches use-after-free"
    );
    assert_eq!(m.read_u32(h), 0, "the master pointer should be cleared");
}

#[test]
fn disposing_an_unknown_handle_records_a_fault() {
    let mut m = Memory::new();
    m.dispose_handle(0x4242);
    assert_eq!(m.faults.len(), 1);
    assert!(m.faults[0].note.contains("unknown handle"));
}

#[test]
fn lock_state_is_recorded() {
    let mut m = Memory::new();
    let h = m.new_handle(32, false);
    assert!(!m.handle_info(h).expect("info").locked);
    m.set_handle_locked(h, true);
    assert!(m.handle_info(h).expect("info").locked);
    assert_eq!(
        m.handle_info(h).expect("info").state_byte() & handle::state::LOCK,
        handle::state::LOCK
    );
    m.set_handle_locked(h, false);
    assert!(!m.handle_info(h).expect("info").locked);
}

#[test]
fn block_move_handles_overlap_in_both_directions() {
    let mut m = Memory::new();
    let base = 0x4000u32;
    for i in 0..16u32 {
        m.write_u8(base + i, i as u8);
    }
    // Forward overlap.
    m.block_move(base, base + 4, 8);
    assert_eq!(
        m.read_bytes(base + 4, 8),
        vec![0, 1, 2, 3, 4, 5, 6, 7],
        "overlapping BlockMove must not smear bytes"
    );

    // Backward overlap.
    for i in 0..16u32 {
        m.write_u8(base + i, i as u8);
    }
    m.block_move(base + 4, base, 8);
    assert_eq!(m.read_bytes(base, 8), vec![4, 5, 6, 7, 8, 9, 10, 11]);
}

#[test]
fn block_move_of_zero_or_same_address_is_a_no_op() {
    let mut m = Memory::new();
    m.write_u8(0x5000, 0x11);
    m.block_move(0x5000, 0x5000, 4);
    m.block_move(0x5000, 0x6000, 0);
    assert_eq!(m.read_u8(0x5000), 0x11);
    assert_eq!(m.read_u8(0x6000), 0x00);
}

#[test]
fn heap_exhaustion_returns_zero_rather_than_panicking() {
    let mut m = Memory::new();
    let huge = m.new_ptr(RAM_SIZE, false);
    assert_eq!(huge, 0, "an impossible allocation must fail cleanly");
    assert!(m.faults.iter().any(|f| f.note.contains("heap exhausted")));
}

#[test]
fn host_arena_is_outside_ram_so_modules_cannot_reach_it() {
    let mut m = Memory::new();
    let a = m.alloc_host(64).expect("64 bytes of arena").get();
    assert!(a >= HOST_ARENA);
    m.write_u32(a, 0xCAFE_F00D);
    assert_eq!(m.read_u32(a), 0xCAFE_F00D);

    // Walking off the end of the heap must not land in the arena.
    let p = m.new_ptr(16, false);
    assert!(p < HOST_ARENA);
    assert!(m.faults.is_empty(), "arena access must not fault");
}

#[test]
fn low_memory_globals_have_sane_defaults() {
    let mut m = Memory::new();
    assert_eq!(
        globals::LowMem::rnd_seed(&mut m),
        globals::DEFAULT_RND_SEED,
        "RndSeed must be seeded so _Random is reproducible"
    );
    assert_ne!(m.read_u32(globals::TIME), 0, "clock should be fixed, not zero");
    assert_eq!(m.read_u8(globals::QD_EXIST), 0xFF);
    assert_eq!(m.read_u32(globals::CUR_STACK_BASE), STACK_TOP);
}

#[test]
fn accounting_tracks_growth_for_leak_checks() {
    let mut m = Memory::new();
    let before = m.heap_used();
    let h = m.new_handle(1024, false);
    assert!(m.heap_used() > before);
    assert_eq!(m.live_handles(), 1);
    m.dispose_handle(h);
    assert_eq!(
        m.live_handles(),
        0,
        "handle count is what a soak test watches"
    );
}

#[test]
fn host_arena_can_hold_a_screen_bitmap() {
    // 640x480 at 8bpp. The capacity is asserted here rather than discovered as
    // corruption later; `alloc_host` now returns None instead of address 0, so
    // the old `assert_ne!(_, 0)` guards are carried by the type.
    let mut m = Memory::new();
    let screen = m
        .alloc_host(640 * 480)
        .expect("the arena must fit a full screen bitmap")
        .get();
    assert!(screen >= HOST_ARENA + HOST_RESERVED);
    // And still leave room for the host structures.
    for size in [44u32, 0x100, 0x40] {
        assert!(
            m.alloc_host(size).is_some(),
            "arena exhausted after the screen"
        );
    }
}

#[test]
fn reserved_arena_prefix_is_never_handed_out() {
    // The trap gate and host return sentinel live in the reserved prefix.
    let mut m = Memory::new();
    for _ in 0..8 {
        let a = m.alloc_host(16).expect("arena space").get();
        assert!(
            a >= HOST_ARENA + HOST_RESERVED,
            "alloc_host handed out the reserved prefix at {a:#x}"
        );
    }
}

#[test]
fn every_host_address_fits_the_68000_address_bus() {
    // The 68000 masks addresses to 24 bits. Anything the runtime places above
    // that is truncated by the CPU, so a module's write lands somewhere else
    // entirely and the structure reads back as zero.
    // These relate constants only, so they belong at compile time: a broken
    // address map should fail the build, not wait for someone to run the tests.
    const { assert!(HOST_ARENA > RAM_SIZE, "the arena must not overlap RAM") };
    const {
        assert!(
            HOST_ARENA + HOST_ARENA_SIZE <= ADDRESS_MASK,
            "the arena ends past the 68000's 24-bit limit"
        )
    };

    // And every allocation the runtime actually makes must stay addressable.
    let mut m = Memory::new();
    let screen = m.alloc_host(640 * 480);
    for a in [screen, m.alloc_host(44), m.alloc_host(4), m.alloc_host(0x100)] {
        let a = a.expect("allocation failed").get();
        assert_eq!(a & ADDRESS_MASK, a, "{a:#x} is not addressable by a 68000");
    }
    assert_eq!(STACK_TOP & ADDRESS_MASK, STACK_TOP);
    assert_eq!(HEAP_BASE & ADDRESS_MASK, HEAP_BASE);
}

#[test]
fn trap_gate_lives_in_the_reserved_arena_prefix() {
    // ad_m68k::TRAP_GATE is a separate constant; if the two drift apart the gate
    // is either unbacked memory or something an allocator can hand out.
    const TRAP_GATE: u32 = 0x00A0_0100;
    const {
        assert!(
            TRAP_GATE >= HOST_ARENA && TRAP_GATE < HOST_ARENA + HOST_RESERVED,
            "the trap gate is outside the reserved arena prefix"
        )
    };
}

#[test]
fn code_region_sits_below_the_heap() {
    // An earlier layout put the heap at 0x2000 while module code loaded at
    // 0x10000, so ~56 KiB of allocation silently overwrote the module's own
    // instructions. Assert the ordering so it cannot come back.
    const { assert!(CODE_REGION < HEAP_BASE, "code must be below the heap") };
    const { assert!(HEAP_BASE < STACK_TOP, "the heap must be below the stack") };
    // And a megabyte of allocation must not reach the code.
    let mut m = Memory::new();
    for _ in 0..64 {
        let p = m.new_ptr(16 * 1024, false);
        assert!(p >= HEAP_BASE, "allocation at {p:#x} escaped the heap");
    }
}

#[test]
fn disposed_handles_return_their_space() {
    // Without reclamation a module that allocates and frees in a loop exhausts
    // the heap and reports "out of memory" for no visible reason.
    let mut m = Memory::new();
    let before = m.free_bytes();
    for _ in 0..2_000 {
        let h = m.new_handle(4096, false);
        assert_ne!(h, 0, "allocation failed while cycling");
        m.dispose_handle(h);
    }
    let after = m.free_bytes();
    assert!(
        after >= before.saturating_sub(8192),
        "free memory fell from {before} to {after} across alloc/free cycles"
    );
    assert_eq!(m.live_handles(), 0);
}

#[test]
fn master_pointer_slots_are_recycled() {
    // The slot region holds ~7000 handles; cycling far more than that must work.
    let mut m = Memory::new();
    for _ in 0..20_000 {
        let h = m.new_handle(32, false);
        assert_ne!(h, 0, "ran out of master pointers");
        m.dispose_handle(h);
    }
}

#[test]
fn free_list_coalesces_adjacent_spans() {
    let mut m = Memory::new();
    let a = m.new_ptr(1024, false);
    let b = m.new_ptr(1024, false);
    let c = m.new_ptr(1024, false);
    assert_eq!(b, a + 1024);
    assert_eq!(c, b + 1024);
    m.dispose_ptr(a);
    m.dispose_ptr(b);
    m.dispose_ptr(c);
    // The three spans must merge, so a single larger request fits in them.
    let big = m.new_ptr(3072, false);
    assert_eq!(big, a, "coalescing failed; got {big:#x} instead of {a:#x}");
}

#[test]
fn set_handle_size_grows_by_moving_and_rewrites_the_master_pointer() {
    let mut m = Memory::new();
    let h = m.new_handle(16, true);
    let first = m.deref_handle(h).expect("resolve");
    for i in 0..16 {
        m.write_u8(first + i, 0xC0 | (i as u8));
    }
    // Block something directly after it so growth cannot happen in place.
    let _wall = m.new_ptr(16, false);
    assert!(m.resize_handle(h, 4096));
    let moved = m.deref_handle(h).expect("resolve");
    assert_eq!(m.read_u32(h), moved, "master pointer must be rewritten");
    assert_eq!(m.handle_size(h), Some(4096));
    // Contents must survive the move.
    for i in 0..16 {
        assert_eq!(m.read_u8(moved + i), 0xC0 | (i as u8), "byte {i} lost");
    }
}

#[test]
fn free_bytes_counts_the_free_list_not_just_the_tail() {
    let mut m = Memory::new();
    let p = m.new_ptr(1024 * 1024, false);
    let after_alloc = m.free_bytes();
    m.dispose_ptr(p);
    let after_free = m.free_bytes();
    assert!(
        after_free > after_alloc,
        "reclaimed space must be reported as free: {after_alloc} -> {after_free}"
    );
    assert!(m.max_block() >= 1024 * 1024);
}

#[test]
fn arena_fits_everything_the_runtime_allocates() {
    // Undersizing the arena has caused two separate corruption bugs: a screen
    // based at address 0, and later a 300 KiB PICT staging buffer written over
    // the vector table. Assert the budget instead of rediscovering it.
    const SCREEN: u32 = 640 * 480;
    const PICT_SCRATCH: u32 = 640 * 480;
    const STRUCTS: u32 = 64 * 1024; // param block, QD globals, ports, callouts
    let needed = HOST_RESERVED + SCREEN + PICT_SCRATCH + STRUCTS;
    assert!(
        HOST_ARENA_SIZE >= needed,
        "arena is {HOST_ARENA_SIZE:#x} but needs {needed:#x}"
    );
    const {
        assert!(
            HOST_ARENA + HOST_ARENA_SIZE <= ADDRESS_MASK,
            "the arena must stay inside the 68000's 24-bit address space"
        )
    };

    let mut m = Memory::new();
    let screen = m.alloc_host(SCREEN).expect("screen allocation").get();
    let scratch = m
        .alloc_host(PICT_SCRATCH)
        .expect("PICT staging allocation")
        .get();
    assert!(m.arena_free() > 32 * 1024, "no headroom left for structures");
    // And they must not overlap.
    assert!(screen + SCREEN <= scratch || scratch + PICT_SCRATCH <= screen);
}

#[test]
#[should_panic(expected = "host arena exhausted reserving")]
fn reserve_host_panics_naming_the_fixture_that_did_not_fit() {
    // The failure mode this replaces: `alloc_host` returned 0, the caller based
    // a 300 KiB screen at address 0, and 300 KiB landed on the 68000 exception
    // vector table. That happened twice and cost ten working modules once. A
    // panic naming the fixture is strictly better than corrupting low memory.
    let mut m = Memory::new();
    let _ = m.reserve_host(HOST_ARENA_SIZE, "deliberately oversized fixture");
}

#[test]
fn arena_exhaustion_is_reported_as_none_not_as_address_zero() {
    // The whole point of the Option: there is no longer a value a caller can
    // mistake for a valid low-memory address.
    let mut m = Memory::new();
    assert!(
        m.alloc_host(HOST_ARENA_SIZE).is_none(),
        "an impossible request must fail"
    );
    // A failed request must not have consumed the arena either.
    assert!(
        m.alloc_host(1024).is_some(),
        "a failed allocation must leave the arena usable"
    );
}

// ---- W5: conservation of the guest heap -------------------------------------
//
// The audit asked for "after all objects are released, available memory returns
// to the exact starting value". That is unachievable as stated, because
// `alloc_host` is a deliberate bump allocator for fixtures that live as long as
// the process. It holds exactly for the *guest heap*, which is what modules
// allocate from and what `_FreeMem` reports, so that is what these assert.

#[test]
fn guest_heap_returns_to_exactly_its_starting_size() {
    let mut m = Memory::new();
    let start = m.free_bytes();

    let ptrs: Vec<u32> = (0..32).map(|i| m.new_ptr(64 + i * 16, false)).collect();
    let handles: Vec<u32> = (0..32).map(|i| m.new_handle(128 + i * 8, true)).collect();
    assert!(m.free_bytes() < start, "allocation must consume heap");

    for p in ptrs {
        m.dispose_ptr(p);
    }
    for h in handles {
        m.dispose_handle(h);
    }
    assert_eq!(
        m.free_bytes(),
        start,
        "every byte must come back once all blocks are disposed"
    );
}

#[test]
fn growing_a_handle_does_not_leak_the_old_block() {
    // resize_handle allocated a new block, copied into it, rebound the handle —
    // and dropped the old block on the floor. Every _SetHandleSize that grew a
    // handle lost the entire previous allocation.
    let mut m = Memory::new();
    let start = m.free_bytes();
    let h = m.new_handle(256, false);
    for size in [512u32, 1024, 4096, 16384] {
        assert!(m.resize_handle(h, size), "grow to {size}");
    }
    m.dispose_handle(h);
    assert_eq!(
        m.free_bytes(),
        start,
        "four grows leaked their predecessors"
    );
}

#[test]
fn shrinking_a_handle_returns_the_tail() {
    // Shrinking only edited the recorded size, so the unused tail stayed
    // allocated forever. A grow/shrink cycle bled the difference each time.
    let mut m = Memory::new();
    let h = m.new_handle(16384, false);
    let big = m.free_bytes();
    assert!(m.resize_handle(h, 256));
    assert!(
        m.free_bytes() > big,
        "shrinking 16 KiB to 256 bytes must give the tail back"
    );
    m.dispose_handle(h);
}

#[test]
fn repeated_grow_shrink_cycles_do_not_drift() {
    // The shape a module actually produces: one buffer resized every frame.
    let mut m = Memory::new();
    let start = m.free_bytes();
    let h = m.new_handle(1024, false);
    for _ in 0..200 {
        assert!(m.resize_handle(h, 8192));
        assert!(m.resize_handle(h, 1024));
    }
    m.dispose_handle(h);
    assert_eq!(m.free_bytes(), start, "200 cycles drifted");
}

#[test]
fn a_failed_new_handle_leaves_no_orphaned_data_block() {
    // new_handle allocates the data block first. When master-pointer space runs
    // out it returned nil — without freeing that block. The failure path is
    // precisely where a module is already short of memory, so leaking there is
    // the worst possible moment.
    let mut m = Memory::new();
    // Exhaust master pointers, keeping every handle alive so none are recycled.
    let mut live = Vec::new();
    loop {
        let h = m.new_handle(16, false);
        if h == 0 {
            break;
        }
        live.push(h);
    }
    let after_exhaustion = m.free_bytes();
    // Further attempts must fail without consuming heap.
    for _ in 0..16 {
        assert_eq!(m.new_handle(4096, false), 0, "must fail once slots are gone");
    }
    assert_eq!(
        m.free_bytes(),
        after_exhaustion,
        "failed allocations leaked their data blocks"
    );
    for h in live {
        m.dispose_handle(h);
    }
}

#[test]
fn master_pointers_are_recycled_not_merely_bumped() {
    // Allocate and free in a loop; the slot space must not creep, or a module
    // that churns handles eventually gets "out of memory" for no visible reason.
    let mut m = Memory::new();
    let first = m.new_handle(32, false);
    m.dispose_handle(first);
    for _ in 0..5000 {
        let h = m.new_handle(32, false);
        assert_ne!(h, 0, "slot space crept despite disposal");
        m.dispose_handle(h);
    }
}

#[test]
fn ptr_size_reports_what_was_allocated() {
    // _GetPtrSize answered 0 for every pointer even though new_ptr had always
    // recorded the size.
    let mut m = Memory::new();
    let p = m.new_ptr(300, false);
    // Sizes are rounded up to the allocator's alignment, never down: a caller
    // that writes `size` bytes must stay inside its own block.
    let size = m.ptr_size(p).expect("a live pointer has a size");
    assert!(
        (300..=304).contains(&size),
        "expected ~300 bytes, got {size}"
    );
    m.dispose_ptr(p);
    assert_eq!(m.ptr_size(p), None, "a disposed pointer has no size");
}

/// The bulk copy must agree with the byte-at-a-time path everywhere.
///
/// It exists purely for speed — refreshing the 640x480 screen cache a byte at a
/// time held the emulator to an eighth of its cycle budget and the picture to
/// eight frames a second — so it is only worth having if it is indistinguishable.
/// The screen lives in the **arena**, not in RAM, which is the case a RAM-only
/// fast path would have silently missed.
#[test]
fn copy_out_matches_reading_byte_by_byte() {
    let mut m = Memory::new();

    // Three spans: inside RAM, inside the arena (where the screen is), and one
    // that runs off the end of RAM.
    let spans = [
        (0x0002_0000u32, 300usize),
        (HOST_ARENA + 0x200, 4096),
        (RAM_SIZE - 8, 32),
    ];
    // Something recognisable at every address the spans touch.
    for &(base, len) in &spans {
        for i in 0..len {
            let a = base.wrapping_add(i as u32);
            m.write_u8(a, (a ^ (a >> 8)) as u8);
        }
    }
    for &(base, len) in &spans {
        let slow: Vec<u8> = (0..len)
            .map(|i| m.read_u8(base.wrapping_add(i as u32)))
            .collect();
        let mut fast = vec![0u8; len];
        m.copy_out(base, &mut fast);
        assert_eq!(fast, slow, "span at {base:#x} len {len}");
    }

    // The screen span the framebuffer actually uses.
    let mut screen = vec![0u8; 640 * 480];
    m.copy_out(HOST_ARENA + 0x200, &mut screen);
    assert_eq!(screen.len(), 640 * 480);

    // A zero-length copy must not touch anything or panic.
    let mut none: [u8; 0] = [];
    m.copy_out(0, &mut none);
}
