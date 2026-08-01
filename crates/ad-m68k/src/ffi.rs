//! Raw bindings to the vendored Musashi core.
//!
//! Everything `unsafe` in this crate lives here. Musashi's API is a set of free
//! functions over C globals, so these wrappers are thin.

#![allow(clippy::missing_safety_doc)]

/// Which 680x0 to emulate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuType {
    /// After Dark 2.0x modules target the 68000.
    M68000,
    M68010,
    M68020,
    M68030,
    M68040,
}

impl CpuType {
    /// Bytes this CPU pushes for a group-1/2 exception.
    ///
    /// The 68000 pushes SR and PC and nothing else. Every later member appends a
    /// format word, which `RTE` consults to size its own pop — so the trap gate
    /// must round-trip it, not just skip it.
    #[must_use]
    pub const fn exception_frame_size(self) -> u32 {
        match self {
            Self::M68000 => crate::EXCEPTION_FRAME_SIZE,
            _ => crate::EXCEPTION_FRAME_SIZE_68010,
        }
    }

    const fn to_raw(self) -> u32 {
        // Values from M68K_CPU_TYPE_* in m68k.h.
        match self {
            Self::M68000 => 1,
            Self::M68010 => 2,
            Self::M68020 => 4,
            Self::M68030 => 5,
            Self::M68040 => 6,
        }
    }
}

/// Register ids from `m68k_register_t` in m68k.h.
mod reg {
    pub const D0: i32 = 0;
    pub const A0: i32 = 8;
    pub const PC: i32 = 16;
    pub const SR: i32 = 17;
    pub const SP: i32 = 18;
}

unsafe extern "C" {
    fn m68k_init();
    fn m68k_set_cpu_type(cpu_type: u32);
    fn m68k_pulse_reset();
    fn m68k_execute(num_cycles: i32) -> i32;
    fn m68k_end_timeslice();
    fn m68k_get_reg(context: *mut core::ffi::c_void, regnum: i32) -> u32;
    fn m68k_set_reg(regnum: i32, value: u32);
    fn m68k_set_instr_hook_callback(callback: Option<extern "C" fn(pc: u32)>);
    fn m68k_disassemble(str_buff: *mut core::ffi::c_char, pc: u32, cpu_type: u32) -> u32;
}

pub fn init() {
    unsafe { m68k_init() }
}

pub fn set_cpu_type(t: CpuType) {
    unsafe { m68k_set_cpu_type(t.to_raw()) }
}

pub fn set_instr_hook() {
    unsafe { m68k_set_instr_hook_callback(Some(crate::ad_m68k_instr_hook)) }
}

pub fn pulse_reset() {
    unsafe { m68k_pulse_reset() }
}

pub fn execute(cycles: u32) -> u32 {
    // Musashi takes a signed count; clamp so a huge budget cannot go negative.
    let n = i32::try_from(cycles).unwrap_or(i32::MAX);
    let used = unsafe { m68k_execute(n) };
    used.max(0).unsigned_abs()
}

pub fn end_timeslice() {
    unsafe { m68k_end_timeslice() }
}

fn get_reg(n: i32) -> u32 {
    unsafe { m68k_get_reg(core::ptr::null_mut(), n) }
}

pub fn get_pc() -> u32 {
    get_reg(reg::PC)
}
pub fn set_pc(v: u32) {
    unsafe { m68k_set_reg(reg::PC, v) }
}
pub fn get_sp() -> u32 {
    get_reg(reg::SP)
}
pub fn set_sp(v: u32) {
    unsafe { m68k_set_reg(reg::SP, v) }
}
pub fn get_sr() -> u32 {
    get_reg(reg::SR)
}
pub fn set_sr(v: u32) {
    unsafe { m68k_set_reg(reg::SR, v) }
}

pub fn get_data_reg(n: u8) -> u32 {
    get_reg(reg::D0 + i32::from(n.min(7)))
}
pub fn set_data_reg(n: u8, v: u32) {
    unsafe { m68k_set_reg(reg::D0 + i32::from(n.min(7)), v) }
}
pub fn get_addr_reg(n: u8) -> u32 {
    get_reg(reg::A0 + i32::from(n.min(7)))
}
pub fn set_addr_reg(n: u8, v: u32) {
    unsafe { m68k_set_reg(reg::A0 + i32::from(n.min(7)), v) }
}

/// Disassemble one instruction at `pc`, returning the text and its length.
pub fn disassemble(pc: u32) -> (String, u32) {
    // Musashi documents a 100-byte maximum for the output buffer.
    let mut buf = [0i8; 256];
    let len = unsafe {
        m68k_disassemble(
            buf.as_mut_ptr().cast::<core::ffi::c_char>(),
            pc,
            CpuType::M68000.to_raw(),
        )
    };
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    (String::from_utf8_lossy(&bytes).into_owned(), len)
}
