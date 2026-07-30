//! Low-memory globals.
//!
//! Classic Mac OS kept system state at fixed low addresses, and both the Toolbox
//! and applications read and wrote them directly. Only the ones After Dark
//! modules actually touch are modelled.
//!
//! `RndSeed` is the important one: `_Random` is a documented LCG over it, so
//! seeding it deterministically is what makes a module's behaviour reproducible
//! frame for frame.

use crate::Memory;

/// `RndSeed` — the `_Random` seed. **long**, at `0x0156`.
pub const RND_SEED: u32 = 0x0156;
/// `Ticks` — 60ths of a second since boot. **long**, at `0x016A`.
pub const TICKS: u32 = 0x016A;
/// `Time` — seconds since 1904. **long**, at `0x020C`.
pub const TIME: u32 = 0x020C;
/// `KeyMap` — 16 bytes of key state, one bit per virtual key code, at `0x0174`.
/// Byte `KEY_MAP + (code >> 3)`, bit `code & 7`. Fast games (Lunatic Fringe)
/// poll this directly instead of calling `_GetKeys`.
pub const KEY_MAP: u32 = 0x0174;
/// `MemErr` — last Memory Manager result code. **word**, at `0x0220`.
pub const MEM_ERR: u32 = 0x0220;
/// `ResErr` — last Resource Manager result code. **word**, at `0x0A60`.
pub const RES_ERR: u32 = 0x0A60;
/// `ScrnBase` — base of the main screen buffer. **long**, at `0x0824`.
pub const SCRN_BASE: u32 = 0x0824;
/// `MainDevice` — handle to the main `GDevice`. **long**, at `0x08A4`.
pub const MAIN_DEVICE: u32 = 0x08A4;
/// `DeviceList` — handle to the first `GDevice`. **long**, at `0x08A8`.
pub const DEVICE_LIST: u32 = 0x08A8;
/// `TheGDevice` — handle to the current `GDevice`. **long**, at `0x0CC8`.
pub const THE_GDEVICE: u32 = 0x0CC8;
/// `QDExist` — nonzero once QuickDraw is initialised. **byte**, at `0x08F3`.
pub const QD_EXIST: u32 = 0x08F3;
/// `CurrentA5` — the A5 world base. **long**, at `0x0904`.
pub const CURRENT_A5: u32 = 0x0904;
/// `CurStackBase` — base of the stack. **long**, at `0x0908`.
pub const CUR_STACK_BASE: u32 = 0x0908;
/// `ROM85` — high bit clear on a Mac Plus-era ROM; modules test it to decide
/// whether Color QuickDraw might exist. **word**, at `0x028E`.
pub const ROM85: u32 = 0x028E;
/// `HWCfgFlags` — hardware configuration bits. **word**, at `0x0B22`.
pub const HW_CFG_FLAGS: u32 = 0x0B22;

/// Seed `_Random` reproducibly. Any nonzero value works; 1 is conventional.
pub const DEFAULT_RND_SEED: u32 = 1;

/// Install the low-memory values a module can reasonably expect at startup.
pub fn install_defaults(mem: &mut Memory) {
    mem.write_u32(RND_SEED, DEFAULT_RND_SEED);
    mem.write_u32(TICKS, 0);
    // 1 Jan 1994 00:00:00, in seconds since 1 Jan 1904. Fixed so runs are
    // reproducible; a module that reads the clock must not make runs diverge.
    mem.write_u32(TIME, 2_840_140_800);
    mem.write_u16(MEM_ERR, 0);
    mem.write_u16(RES_ERR, 0);
    mem.write_u8(QD_EXIST, 0xFF);
    // A Quadra-era machine: 32-bit QuickDraw present, so the high bit is set.
    mem.write_u16(ROM85, 0x7FFF);
    mem.write_u32(CUR_STACK_BASE, crate::STACK_TOP);
}

/// Convenience accessors for the globals the Toolbox layer updates every frame.
#[derive(Debug)]
pub struct LowMem;

impl LowMem {
    /// Read `RndSeed`.
    #[must_use]
    pub fn rnd_seed(mem: &mut Memory) -> u32 {
        mem.read_u32(RND_SEED)
    }

    /// Write `RndSeed`.
    pub fn set_rnd_seed(mem: &mut Memory, seed: u32) {
        mem.write_u32(RND_SEED, seed);
    }

    /// Read `Ticks`.
    #[must_use]
    pub fn ticks(mem: &mut Memory) -> u32 {
        mem.read_u32(TICKS)
    }

    /// Write `Ticks`.
    pub fn set_ticks(mem: &mut Memory, ticks: u32) {
        mem.write_u32(TICKS, ticks);
    }

    /// Write `MemErr`, which `_MemError` returns.
    pub fn set_mem_err(mem: &mut Memory, err: i16) {
        mem.write_u16(MEM_ERR, err as u16);
    }

    /// Write `ResErr`, which `_ResError` returns.
    pub fn set_res_err(mem: &mut Memory, err: i16) {
        mem.write_u16(RES_ERR, err as u16);
    }
}
