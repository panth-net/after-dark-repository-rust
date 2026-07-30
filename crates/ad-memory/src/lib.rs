//! Emulated Macintosh address space and Memory Manager.
//!
//! # Address map
//!
//! There is no ROM. Nothing in this runtime executes Apple code, so the map only
//! needs what a graphics module can legitimately reach:
//!
//! ```text
//! 0x0000_0000  ┌─ exception vectors + low-memory globals ─┐  RndSeed, Ticks, …
//! 0x0000_1000  ├─ master pointers ───────────────────────┤  28 KiB = 7168 handles
//! 0x0000_8000  ├─ module code ───────────────────────────┤  the ADgm resource
//! 0x0010_0000  ├─ application heap ──────────────────────┤  grows up, ~7 MiB
//!              │  NewHandle / NewPtr, with a free list   │
//! 0x007F_0000  ├─ stack ─────────────────────────────────┤  grows down
//! 0x00A0_0000  ├─ host arena: return sentinel, trap gate ┤  see `ad_m68k`
//!              └─ GMParamBlock, QDGlobals, screen bitmap ┘
//! ```
//!
//! The gap between master pointers and the heap is not decorative. An earlier
//! layout put the heap at `0x2000` while the host loaded module code at
//! `0x10000`, so after ~56 KiB of allocation a module quietly overwrote its own
//! instructions. Keep code below the heap and leave room.
//!
//! # Faults are recorded, never fatal
//!
//! A module reading unmapped memory is a bug worth *reporting*, but panicking
//! mid-frame would lose the diagnostic context. Every stray access is recorded
//! in [`Memory::faults`] and returns a defined value, so a run can continue far
//! enough to show what went wrong.

use std::collections::BTreeMap;
use std::num::NonZeroU32;

pub mod globals;
pub mod handle;

pub use globals::LowMem;
pub use handle::{Handle, MasterPointer};

/// Total emulated RAM. Lunatic Fringe's help text asks for ~600 K, so 8 MiB is
/// generous for every module on the disk while staying trivially allocatable.
pub const RAM_SIZE: u32 = 8 * 1024 * 1024;

/// First address of the application heap.
///
/// Well above the region the host loads module code into, so a growing heap can
/// never reach the code.
pub const HEAP_BASE: u32 = 0x0010_0000;

/// Where the host may load module code: after the master pointers, before the
/// heap.
pub const CODE_REGION: u32 = 0x0000_8000;

/// Where the stack pointer starts. Grows down toward the heap.
pub const STACK_TOP: u32 = RAM_SIZE - 0x1_0000;

/// Highest address a 68000 can express.
///
/// The 68000 has a **24-bit address bus**, so the CPU masks every access to this
/// range. Anything the runtime places above it is silently truncated: a module
/// storing its handle at `0x0104_922C` writes to `0x0004_922C` instead, and the
/// symptom is a structure that reads back as zero even though the module clearly
/// wrote it.
pub const ADDRESS_MASK: u32 = 0x00FF_FFFF;

/// Base of the arena holding host-owned structures (param block, QD globals).
///
/// Above RAM but comfortably inside the 24-bit address space, with room for the
/// whole arena below [`ADDRESS_MASK`].
pub const HOST_ARENA: u32 = 0x00A0_0000;

/// Bytes at the start of the arena reserved for the runtime's own fixtures:
/// the host return sentinel and the A-line trap gate. Allocations start above
/// them so `alloc_host` can never hand out the gate.
pub const HOST_RESERVED: u32 = 0x200;

/// Size of the host arena.
///
/// Must hold everything the runtime allocates outside the module's heap: the
/// screen bitmap (640x480 at 8bpp = 300 KiB), an equally large staging buffer for
/// decoded `PICT` bitmaps, plus the param block, QuickDraw globals, monitor list,
/// callout slots and per-port PixMaps.
///
/// **Undersizing this used to be silently catastrophic, and happened twice.**
/// `alloc_host` returned 0 on exhaustion, and a caller writing through a nil
/// base address landed on the exception vectors — arbitrary corruption a long
/// way from the cause rather than an allocation failure. It now returns
/// [`Option<NonZeroU32>`], and fixed startup fixtures go through
/// [`Memory::reserve_host`], which panics naming the fixture that did not fit.
pub const HOST_ARENA_SIZE: u32 = 0x0018_0000;

/// A memory access that should not have happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fault {
    pub addr: u32,
    pub size: u8,
    pub write: bool,
    pub note: String,
}

/// The emulated address space.
#[derive(Debug)]
pub struct Memory {
    ram: Vec<u8>,
    /// Host structures live outside RAM so a module cannot corrupt them by
    /// walking off the end of its heap.
    arena: Vec<u8>,
    /// Bump allocator for the application heap.
    heap_next: u32,
    /// Live handles, keyed by the address of their master pointer.
    handles: BTreeMap<u32, handle::Block>,
    /// Bump pointer for fresh master pointers.
    master_next: u32,
    /// Master pointer slots returned by `_DisposeHandle`, ready to reuse.
    ///
    /// Without this a module that allocates and frees in a loop exhausts the slot
    /// space and starts reporting "out of memory" for no visible reason.
    master_free: Vec<u32>,
    /// Free heap spans, kept sorted by address and coalesced.
    free_list: Vec<(u32, u32)>,
    /// Live non-relocatable blocks, so `_DisposPtr` can return their space.
    ptrs: BTreeMap<u32, u32>,
    /// Bump allocator for the host arena.
    arena_next: u32,
    /// Every stray access, in order.
    pub faults: Vec<Fault>,
    /// Cap on recorded faults, so a runaway loop cannot exhaust memory.
    fault_cap: usize,
}

/// Master pointers occupy this region, sized for 7168 live handles.
const MASTER_BASE: u32 = 0x0000_1000;
const MASTER_LIMIT: u32 = CODE_REGION;

impl Default for Memory {
    fn default() -> Self {
        Self::new()
    }
}

impl Memory {
    #[must_use]
    pub fn new() -> Self {
        let mut m = Self {
            ram: vec![0; RAM_SIZE as usize],
            arena: vec![0; HOST_ARENA_SIZE as usize],
            heap_next: HEAP_BASE,
            handles: BTreeMap::new(),
            master_next: MASTER_BASE,
            master_free: Vec::new(),
            free_list: Vec::new(),
            ptrs: BTreeMap::new(),
            arena_next: HOST_ARENA.saturating_add(HOST_RESERVED),
            faults: Vec::new(),
            fault_cap: 256,
        };
        globals::install_defaults(&mut m);
        m
    }

    // ---- raw access ------------------------------------------------------

    fn in_arena(addr: u32) -> bool {
        addr >= HOST_ARENA && addr < HOST_ARENA.saturating_add(HOST_ARENA_SIZE)
    }

    fn record(&mut self, addr: u32, size: u8, write: bool, note: &str) {
        if self.faults.len() < self.fault_cap {
            self.faults.push(Fault {
                addr,
                size,
                write,
                note: note.into(),
            });
        }
    }

    /// Read one byte. Records a fault and returns 0 for unmapped addresses.
    pub fn read_u8(&mut self, addr: u32) -> u8 {
        if Self::in_arena(addr) {
            // `in_arena` already proved addr >= HOST_ARENA.
            let off = addr.wrapping_sub(HOST_ARENA) as usize;
            return self.arena.get(off).copied().unwrap_or(0);
        }
        match self.ram.get(addr as usize) {
            Some(b) => *b,
            None => {
                self.record(addr, 1, false, "read outside RAM");
                0
            }
        }
    }

    pub fn write_u8(&mut self, addr: u32, value: u8) {
        if Self::in_arena(addr) {
            let off = addr.wrapping_sub(HOST_ARENA) as usize;
            if let Some(slot) = self.arena.get_mut(off) {
                *slot = value;
            }
            return;
        }
        match self.ram.get_mut(addr as usize) {
            Some(slot) => *slot = value,
            None => self.record(addr, 1, true, "write outside RAM"),
        }
    }

    pub fn read_u16(&mut self, addr: u32) -> u16 {
        // Whole-width RAM fast path. This is the instruction-fetch function —
        // Musashi calls it for every opcode and extension word — and composing
        // it from two `read_u8`s meant two arena tests and two bounds checks a
        // fetch. The slow path below keeps the exact byte-wise semantics for
        // the arena, the RAM edge, and misses.
        if !Self::in_arena(addr) {
            let a = addr as usize;
            if let Some(b) = self.ram.get(a..a.wrapping_add(2)) {
                return u16::from_be_bytes([b[0], b[1]]);
            }
        }
        u16::from(self.read_u8(addr)) << 8 | u16::from(self.read_u8(addr.wrapping_add(1)))
    }

    pub fn read_u32(&mut self, addr: u32) -> u32 {
        if !Self::in_arena(addr) {
            let a = addr as usize;
            if let Some(b) = self.ram.get(a..a.wrapping_add(4)) {
                return u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
            }
        }
        u32::from(self.read_u16(addr)) << 16 | u32::from(self.read_u16(addr.wrapping_add(2)))
    }

    pub fn write_u16(&mut self, addr: u32, value: u16) {
        if !Self::in_arena(addr) && !Self::in_arena(addr.wrapping_add(1)) {
            let a = addr as usize;
            if let Some(b) = self.ram.get_mut(a..a.wrapping_add(2)) {
                b.copy_from_slice(&value.to_be_bytes());
                return;
            }
        }
        self.write_u8(addr, (value >> 8) as u8);
        self.write_u8(addr.wrapping_add(1), value as u8);
    }

    pub fn write_u32(&mut self, addr: u32, value: u32) {
        if !Self::in_arena(addr) && !Self::in_arena(addr.wrapping_add(3)) {
            let a = addr as usize;
            if let Some(b) = self.ram.get_mut(a..a.wrapping_add(4)) {
                b.copy_from_slice(&value.to_be_bytes());
                return;
            }
        }
        self.write_u16(addr, (value >> 16) as u16);
        self.write_u16(addr.wrapping_add(2), value as u16);
    }

    /// Copy a slice into emulated memory.
    pub fn write_bytes(&mut self, addr: u32, bytes: &[u8]) {
        for (i, b) in bytes.iter().enumerate() {
            self.write_u8(addr.wrapping_add(i as u32), *b);
        }
    }

    /// Read `len` bytes out of emulated memory.
    #[must_use]
    pub fn read_bytes(&mut self, addr: u32, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        self.copy_out(addr, &mut out);
        out
    }

    /// Copy a span of emulated memory into `dst`, as one block where possible.
    ///
    /// Exists for the screen. The framebuffer cache is refreshed from emulated
    /// memory after every module call, and doing that a byte at a time — 307,200
    /// bounds-checked dispatches for a 640x480 screen — cost about 480 µs per
    /// call. On an idle module the host calls `DrawFrame` a couple of thousand
    /// times a second, so that one loop consumed the entire wall clock and held
    /// the emulator to 1.07M cycles per second out of a budget of 8M: the picture
    /// updated eight times a second and the whole application felt broken.
    ///
    /// Falls back to the per-byte path for a range that is not wholly inside RAM,
    /// so behaviour at a boundary is unchanged.
    /// Both backing stores get a fast path. The screen is in the **arena**, not
    /// in RAM — `HOST_ARENA` is `0x00A0_0000` and RAM ends at 8 MB — so covering
    /// only RAM would have left the case this exists for on the slow path.
    pub fn copy_out(&mut self, addr: u32, dst: &mut [u8]) {
        let len = dst.len();
        let last = addr.wrapping_add(len.saturating_sub(1) as u32);
        let contiguous = if Self::in_arena(addr) {
            // Both ends in the arena, so the span cannot straddle a boundary.
            Self::in_arena(last)
                .then(|| (addr.wrapping_sub(HOST_ARENA) as usize, &self.arena))
        } else if !Self::in_arena(last) {
            Some((addr as usize, &self.ram))
        } else {
            None
        };
        if let Some((off, store)) = contiguous {
            if let Some(src) = store.get(off..off.saturating_add(len)) {
                dst.copy_from_slice(src);
                return;
            }
        }
        for (i, slot) in dst.iter_mut().enumerate() {
            *slot = self.read_u8(addr.wrapping_add(i as u32));
        }
    }

    /// `_BlockMove`: copy `len` bytes, tolerating overlap.
    pub fn block_move(&mut self, src: u32, dst: u32, len: u32) {
        if src == dst || len == 0 {
            return;
        }
        // Copy direction matters only for overlap; read it all first, which is
        // simpler and cannot get the direction wrong.
        let bytes = self.read_bytes(src, len as usize);
        self.write_bytes(dst, &bytes);
    }

    // ---- allocation ------------------------------------------------------

    /// Align up to an even address. The 68000 faults on odd word accesses, and
    /// the Memory Manager always returned even blocks.
    const fn align(n: u32) -> u32 {
        n.saturating_add(1) & !1
    }

    /// Take `size` bytes from the free list, or bump if nothing fits.
    ///
    /// First fit, splitting any remainder. A real Memory Manager compacted; this
    /// does not, but it does *reuse*, which is what stops long-running modules
    /// running out of heap.
    fn take_span(&mut self, size: u32) -> u32 {
        if let Some(i) = self.free_list.iter().position(|(_, sz)| *sz >= size) {
            let (start, sz) = self.free_list[i];
            if sz == size {
                self.free_list.remove(i);
            } else {
                self.free_list[i] = (start.saturating_add(size), sz.saturating_sub(size));
            }
            return start;
        }
        let addr = self.heap_next;
        let end = addr.saturating_add(size);
        if end >= STACK_TOP {
            self.record(addr, 0, true, "heap exhausted");
            return 0;
        }
        self.heap_next = end;
        addr
    }

    /// Return a span to the free list, coalescing with its neighbours.
    fn give_span(&mut self, start: u32, size: u32) {
        if size == 0 {
            return;
        }
        let at = self
            .free_list
            .binary_search_by_key(&start, |(s, _)| *s)
            .unwrap_or_else(|i| i);
        self.free_list.insert(at, (start, size));
        // Coalesce forwards from the previous entry.
        let mut i = at.saturating_sub(1);
        while i.saturating_add(1) < self.free_list.len() {
            let (s0, z0) = self.free_list[i];
            let (s1, z1) = self.free_list[i + 1];
            if s0.saturating_add(z0) == s1 {
                self.free_list[i] = (s0, z0.saturating_add(z1));
                self.free_list.remove(i + 1);
            } else {
                i = i.saturating_add(1);
            }
        }
    }

    /// `_NewPtr`: allocate a non-relocatable block. Returns 0 on failure.
    pub fn new_ptr(&mut self, size: u32, clear: bool) -> u32 {
        let size = Self::align(size.max(1));
        let addr = self.take_span(size);
        if addr == 0 {
            return 0;
        }
        self.ptrs.insert(addr, size);
        if clear {
            for i in 0..size {
                self.write_u8(addr.saturating_add(i), 0);
            }
        }
        addr
    }

    /// `_GetPtrSize`. `None` if this was never a live pointer.
    ///
    /// The size has always been recorded by `new_ptr`; the trap simply never
    /// read it and answered 0 for every pointer.
    #[must_use]
    pub fn ptr_size(&self, addr: u32) -> Option<u32> {
        self.ptrs.get(&addr).copied()
    }

    /// `_DisposPtr`.
    pub fn dispose_ptr(&mut self, addr: u32) {
        if let Some(size) = self.ptrs.remove(&addr) {
            self.give_span(addr, size);
        }
    }

    /// `_NewHandle`: allocate a relocatable block and a master pointer to it.
    ///
    /// Returns the handle (the address *of* the master pointer), or 0 on failure.
    /// This implementation never actually relocates blocks — there is no
    /// compaction — so `_MoveHHi` and friends are no-ops and a locked handle is
    /// indistinguishable from an unlocked one. That is a deliberate
    /// simplification: modules must not depend on a block's address changing.
    pub fn new_handle(&mut self, size: u32, clear: bool) -> u32 {
        let data = self.new_ptr(size, clear);
        if data == 0 {
            return 0;
        }
        let master = match self.master_free.pop() {
            Some(m) => m,
            None => {
                let m = self.master_next;
                if m.saturating_add(4) >= MASTER_LIMIT {
                    self.record(m, 0, true, "master pointer space exhausted");
                    // Roll back: the data block was allocated before the master
                    // pointer, so returning nil here without freeing it leaks
                    // the data on every failure — and this failure path is
                    // exactly where a module is already short of memory.
                    self.dispose_ptr(data);
                    return 0;
                }
                self.master_next = m.saturating_add(4);
                m
            }
        };
        self.write_u32(master, data);
        self.handles.insert(
            master,
            handle::Block {
                data,
                size,
                locked: false,
                purgeable: false,
            },
        );
        master
    }

    /// `_DisposeHandle`. Returns both the block and the master pointer slot.
    pub fn dispose_handle(&mut self, h: u32) {
        match self.handles.remove(&h) {
            Some(b) => {
                self.write_u32(h, 0);
                self.give_span(b.data, b.size);
                self.ptrs.remove(&b.data);
                self.master_free.push(h);
            }
            None => self.record(h, 4, true, "DisposeHandle on an unknown handle"),
        }
    }

    /// Dereference a handle to its block address, or `None` if unknown.
    #[must_use]
    pub fn deref_handle(&self, h: u32) -> Option<u32> {
        self.handles.get(&h).map(|b| b.data)
    }

    /// Metadata for a handle.
    #[must_use]
    pub fn handle_info(&self, h: u32) -> Option<&handle::Block> {
        self.handles.get(&h)
    }

    /// `_GetHandleSize`.
    #[must_use]
    pub fn handle_size(&self, h: u32) -> Option<u32> {
        self.handles.get(&h).map(|b| b.size)
    }

    /// `_RecoverHandle`: the handle whose block contains `ptr`.
    #[must_use]
    pub fn recover_handle(&self, ptr: u32) -> u32 {
        for (h, b) in &self.handles {
            if ptr >= b.data && ptr < b.data.saturating_add(b.size.max(1)) {
                return *h;
            }
        }
        0
    }

    /// `_HLock` / `_HUnlock`.
    pub fn set_handle_locked(&mut self, h: u32, locked: bool) {
        match self.handles.get_mut(&h) {
            Some(b) => b.locked = locked,
            None => self.record(h, 4, true, "HLock/HUnlock on an unknown handle"),
        }
    }

    /// `_SetHandleSize`.
    ///
    /// A bump allocator cannot grow a block in place, so growth allocates a fresh
    /// block and copies; shrinking is recorded without moving anything. The
    /// master pointer is rewritten, which is exactly the indirection handles
    /// exist for.
    pub fn resize_handle(&mut self, h: u32, new_size: u32) -> bool {
        let Some(block) = self.handles.get(&h).copied() else {
            self.record(h, 4, true, "SetHandleSize on an unknown handle");
            return false;
        };
        if new_size <= block.size {
            // Shrinking returns the tail to the free list. Without this a
            // module that repeatedly grows and shrinks a buffer bleeds the
            // difference every cycle, and nothing ever reports a problem —
            // allocation just starts failing later for no visible reason.
            let kept = Self::align(new_size.max(1));
            if kept < block.size {
                self.give_span(block.data.wrapping_add(kept), block.size - kept);
                if let Some(size) = self.ptrs.get_mut(&block.data) {
                    *size = kept;
                }
            }
            if let Some(b) = self.handles.get_mut(&h) {
                b.size = new_size;
            }
            return true;
        }
        let fresh = self.new_ptr(new_size, false);
        if fresh == 0 {
            return false;
        }
        self.block_move(block.data, fresh, block.size);
        self.write_u32(h, fresh);
        if let Some(b) = self.handles.get_mut(&h) {
            b.data = fresh;
            b.size = new_size;
        }
        // Growing moved the data; the old block is now garbage and must go
        // back. Leaking it here was the single largest leak in the allocator:
        // every _SetHandleSize that grew a handle lost the whole old block.
        self.dispose_ptr(block.data);
        true
    }

    /// `_HPurge` / `_HNoPurge`.
    pub fn set_handle_purgeable(&mut self, h: u32, purgeable: bool) {
        if let Some(b) = self.handles.get_mut(&h) {
            b.purgeable = purgeable;
        }
    }

    /// Bytes still available in the host arena.
    ///
    /// Exists so callers can assert capacity up front rather than discover
    /// exhaustion as memory corruption.
    #[must_use]
    pub fn arena_free(&self) -> u32 {
        HOST_ARENA
            .saturating_add(HOST_ARENA_SIZE)
            .saturating_sub(self.arena_next)
    }

    /// Bytes of heap still available: the untouched tail plus everything on the
    /// free list. This is what `_FreeMem` and `_MaxMem` report, and modules
    /// refuse to start if it looks too small.
    #[must_use]
    pub fn free_bytes(&self) -> u32 {
        let tail = STACK_TOP.saturating_sub(self.heap_next);
        let listed: u32 = self.free_list.iter().map(|(_, s)| *s).sum();
        tail.saturating_add(listed)
    }

    /// Largest single allocation that could succeed, for `_MaxBlock`.
    #[must_use]
    pub fn max_block(&self) -> u32 {
        let tail = STACK_TOP.saturating_sub(self.heap_next);
        self.free_list
            .iter()
            .map(|(_, s)| *s)
            .max()
            .unwrap_or(0)
            .max(tail)
    }

    /// Live handle count, for leak checks across a soak run.
    #[must_use]
    pub fn live_handles(&self) -> usize {
        self.handles.len()
    }

    /// Bytes handed out so far, for growth checks.
    #[must_use]
    pub fn heap_used(&self) -> u32 {
        self.heap_next.saturating_sub(HEAP_BASE)
    }

    // ---- host arena ------------------------------------------------------

    /// Allocate a host-owned structure outside the module's heap.
    pub fn alloc_host(&mut self, size: u32) -> Option<NonZeroU32> {
        let size = Self::align(size.max(1));
        let addr = self.arena_next;
        let end = addr.saturating_add(size);
        if end >= HOST_ARENA.saturating_add(HOST_ARENA_SIZE) {
            self.record(addr, 0, true, "host arena exhausted");
            return None;
        }
        self.arena_next = end;
        NonZeroU32::new(addr)
    }

    /// Reserve arena space that the runtime cannot function without.
    ///
    /// Every host fixture — the screen bitmap, the param block, the `PICT`
    /// staging buffer — is a fixed-size allocation made once at startup. If one
    /// of those does not fit, the arena is misconfigured: a *programming* error,
    /// not a condition to handle. Panicking here names the failing fixture and
    /// the constant to raise.
    ///
    /// The alternative is what this code used to do — hand back address 0 and
    /// let the caller write 300 KiB through a nil base onto the 68000 exception
    /// vector table. That happened twice, cost ten working modules once, and was
    /// invisible until the whole survey was re-run. Failing loudly at startup is
    /// strictly better than corrupting low memory at frame 40.
    ///
    /// # Panics
    /// If the arena cannot satisfy `size`.
    #[must_use]
    pub fn reserve_host(&mut self, size: u32, what: &str) -> u32 {
        match self.alloc_host(size) {
            Some(a) => a.get(),
            None => panic!(
                "host arena exhausted reserving {size} bytes for {what}: \
                 {} of {HOST_ARENA_SIZE} bytes free. Raise \
                 ad_memory::HOST_ARENA_SIZE.",
                self.arena_free()
            ),
        }
    }
}

#[cfg(test)]
mod tests;
