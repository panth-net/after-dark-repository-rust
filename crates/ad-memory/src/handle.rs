//! Memory Manager handles.
//!
//! A classic Mac `Handle` is a pointer to a **master pointer**: a stable
//! four-byte slot that itself holds the block's current address. That double
//! indirection is what let the Memory Manager compact the heap by moving blocks
//! and rewriting master pointers.
//!
//! This runtime does not compact, so blocks never move. Locking is therefore
//! recorded but has no effect on placement. That is safe in one direction only —
//! a module that *would* have broken under real compaction will work here — so
//! it can mask a latent bug in a module rather than create one.

/// A relocatable block behind a master pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    /// Current address of the block's data.
    pub data: u32,
    /// Size requested at allocation.
    pub size: u32,
    /// Set by `_HLock`. Recorded for diagnostics; blocks never move regardless.
    pub locked: bool,
    /// Set by `_HPurge`. Nothing is ever actually purged.
    pub purgeable: bool,
}

/// A handle: the address of a master pointer.
pub type Handle = u32;

/// A master pointer: the address of a block.
pub type MasterPointer = u32;

/// `_HGetState` / `_HSetState` flag bits, as the Memory Manager defined them.
pub mod state {
    /// Block is locked.
    pub const LOCK: u8 = 0x80;
    /// Block is purgeable.
    pub const PURGE: u8 = 0x40;
    /// Block came from the resource file.
    pub const RESOURCE: u8 = 0x20;
}

impl Block {
    /// Pack this block's flags the way `_HGetState` reports them.
    #[must_use]
    pub fn state_byte(&self) -> u8 {
        let mut s = 0u8;
        if self.locked {
            s |= state::LOCK;
        }
        if self.purgeable {
            s |= state::PURGE;
        }
        s
    }
}
