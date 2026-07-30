//! SANE — the Standard Apple Numerics Environment.
//!
//! `$A9EB` is **FP68K** and `$A9EC` is **Elems68K**. Together they were Apple's
//! software floating point, and a static scan (see `docs/LEARNINGS.md`) found FP68K
//! called ~1,326 times across 34 of the 66 modules — more than any other trap.
//! Anything that moves along a curve uses it.
//!
//! # Calling convention
//!
//! Not the usual Pascal shape. Operand *addresses* are pushed, then a 16-bit
//! **opword**, then the trap. The routine pops everything.
//!
//! ```text
//! pea.l   src            ; source operand
//! pea.l   dst            ; destination, also the result
//! move.w  #$200E, -(a7)  ; opword
//! _FP68K                 ; $A9EB
//! ```
//!
//! The opword packs the source's data format and the operation:
//!
//! ```text
//! format = opword & 0x3800     FFEXT 0x0000  FFDBL 0x0800  FFSGL 0x1000
//!                              FFINT 0x2000  FFLNG 0x2800  FFCOMP 0x3000
//! op     = opword & 0x001F
//! ```
//!
//! So `$200E` is `FFINT | FOZ2X` — widen a 16-bit integer into an extended.
//!
//! # Precision, stated plainly
//!
//! Arithmetic here happens in `f64` (53-bit mantissa), not in SANE's 80-bit
//! extended (64-bit mantissa). Values are converted losslessly *into* and *out
//! of* the 80-bit memory format, so layout and range are exact, but a long chain
//! of operations can drift in the last few bits against real hardware.
//!
//! That is a deliberate trade, and it is the one place in this runtime where
//! "bit-exact" is knowingly not claimed. For the positions, velocities and angles
//! these modules compute it is invisible; if a module is ever found to depend on
//! extended precision, the vendored `softfloat` (already present for Musashi's
//! FPU) provides the exact path.

use ad_memory::Memory;

/// Source operand data formats.
pub mod format {
    pub const EXT: u16 = 0x0000;
    pub const DBL: u16 = 0x0800;
    pub const SGL: u16 = 0x1000;
    pub const INT: u16 = 0x2000;
    pub const LNG: u16 = 0x2800;
    pub const COMP: u16 = 0x3000;
    pub const MASK: u16 = 0x3800;
}

/// FP68K operations.
pub mod op {
    pub const ADD: u16 = 0x00;
    pub const SUB: u16 = 0x02;
    pub const MUL: u16 = 0x04;
    pub const DIV: u16 = 0x06;
    pub const CMP: u16 = 0x08;
    pub const CPX: u16 = 0x0A;
    pub const REM: u16 = 0x0C;
    pub const Z2X: u16 = 0x0E;
    pub const X2Z: u16 = 0x10;
    pub const SQRT: u16 = 0x12;
    pub const RTI: u16 = 0x14;
    pub const TTI: u16 = 0x16;
    pub const SCALB: u16 = 0x18;
    pub const LOGB: u16 = 0x1A;
    pub const CLASS: u16 = 0x1C;
    pub const MASK: u16 = 0x001F;
}

/// Elems68K operations (transcendentals).
pub mod elem {
    pub const LNX: u16 = 0x00;
    pub const LOG2X: u16 = 0x02;
    pub const LN1X: u16 = 0x04;
    pub const LOG21X: u16 = 0x06;
    pub const EXPX: u16 = 0x08;
    pub const EXP2X: u16 = 0x0A;
    pub const EXP1X: u16 = 0x0C;
    pub const EXP21X: u16 = 0x0E;
    pub const XPWRI: u16 = 0x10;
    pub const XPWRY: u16 = 0x12;
    pub const COMPOUND: u16 = 0x14;
    pub const ANNUITY: u16 = 0x16;
    pub const SINX: u16 = 0x18;
    pub const COSX: u16 = 0x1A;
    pub const TANX: u16 = 0x1C;
    pub const ATANX: u16 = 0x1E;
    pub const RANDX: u16 = 0x20;
}

/// Bytes each format occupies in memory.
#[must_use]
pub fn format_size(fmt: u16) -> u32 {
    match fmt {
        format::EXT => 10,
        format::DBL | format::COMP => 8,
        format::SGL | format::LNG => 4,
        format::INT => 2,
        _ => 10,
    }
}

/// `2^e`, without `libm`.
fn ldexp(m: f64, e: i32) -> f64 {
    // powi saturates to inf / 0 at the extremes, which is the right answer.
    m * 2.0_f64.powi(e)
}

/// Read an 80-bit extended value.
///
/// Layout: sign bit, 15-bit exponent, then a 64-bit mantissa with an **explicit**
/// leading integer bit — unlike `f64`, where that bit is implied.
#[must_use]
pub fn read_ext(mem: &mut Memory, addr: u32) -> f64 {
    let se = mem.read_u16(addr);
    let sign = if se & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = i32::from(se & 0x7FFF);
    let hi = mem.read_u32(addr.wrapping_add(2));
    let lo = mem.read_u32(addr.wrapping_add(6));
    let mant = (u64::from(hi) << 32) | u64::from(lo);

    if exp == 0x7FFF {
        return if mant == 0 {
            sign * f64::INFINITY
        } else {
            f64::NAN
        };
    }
    if exp == 0 && mant == 0 {
        return sign * 0.0;
    }
    // value = mantissa * 2^(exp - 16383 - 63)
    sign * ldexp(mant as f64, exp - 16383 - 63)
}

/// Write an 80-bit extended value.
pub fn write_ext(mem: &mut Memory, addr: u32, v: f64) {
    let sign_bit: u16 = if v.is_sign_negative() { 0x8000 } else { 0 };
    let a = v.abs();

    let (exp, mant) = if v == 0.0 {
        (0u16, 0u64)
    } else if a.is_nan() {
        (0x7FFF, 1u64 << 62)
    } else if a.is_infinite() {
        (0x7FFF, 0u64)
    } else {
        // Normalise to m in [1,2), then the explicit-bit mantissa is m * 2^63.
        let mut e = 0i32;
        let mut m = a;
        while m >= 2.0 {
            m /= 2.0;
            e += 1;
        }
        while m < 1.0 {
            m *= 2.0;
            e -= 1;
        }
        let mant = ldexp(m, 63) as u64;
        let biased = e + 16383;
        if biased <= 0 {
            (0, 0) // underflow to zero rather than emit a denormal
        } else if biased >= 0x7FFF {
            (0x7FFF, 0) // overflow to infinity
        } else {
            (biased as u16, mant)
        }
    };

    mem.write_u16(addr, sign_bit | exp);
    mem.write_u32(addr.wrapping_add(2), (mant >> 32) as u32);
    mem.write_u32(addr.wrapping_add(6), mant as u32);
}

/// Read an operand in any SANE format.
#[must_use]
pub fn read_operand(mem: &mut Memory, addr: u32, fmt: u16) -> f64 {
    match fmt {
        format::EXT => read_ext(mem, addr),
        format::DBL => f64::from_bits(
            (u64::from(mem.read_u32(addr)) << 32) | u64::from(mem.read_u32(addr.wrapping_add(4))),
        ),
        format::SGL => f64::from(f32::from_bits(mem.read_u32(addr))),
        format::INT => f64::from(mem.read_u16(addr) as i16),
        format::LNG => f64::from(mem.read_u32(addr) as i32),
        format::COMP => {
            let hi = mem.read_u32(addr);
            let lo = mem.read_u32(addr.wrapping_add(4));
            (((u64::from(hi) << 32) | u64::from(lo)) as i64) as f64
        }
        _ => read_ext(mem, addr),
    }
}

/// Write an operand in any SANE format.
pub fn write_operand(mem: &mut Memory, addr: u32, fmt: u16, v: f64) {
    match fmt {
        format::EXT => write_ext(mem, addr, v),
        format::DBL => {
            let b = v.to_bits();
            mem.write_u32(addr, (b >> 32) as u32);
            mem.write_u32(addr.wrapping_add(4), b as u32);
        }
        format::SGL => mem.write_u32(addr, (v as f32).to_bits()),
        // Integer conversions round to nearest, which is SANE's default mode.
        format::INT => {
            let r = round_to_nearest(v);
            mem.write_u16(addr, (r.clamp(-32768.0, 32767.0) as i16) as u16);
        }
        format::LNG => {
            let r = round_to_nearest(v);
            mem.write_u32(addr, (r.clamp(-2147483648.0, 2147483647.0) as i32) as u32);
        }
        format::COMP => {
            let r = round_to_nearest(v) as i64;
            mem.write_u32(addr, (r >> 32) as u32);
            mem.write_u32(addr.wrapping_add(4), r as u32);
        }
        _ => write_ext(mem, addr, v),
    }
}

/// Round half to even, SANE's default rounding mode.
#[must_use]
pub fn round_to_nearest(v: f64) -> f64 {
    let f = v.floor();
    let diff = v - f;
    // Exactly .5 goes to the even neighbour — SANE's default, and the reason
    // 2.5 rounds to 2 while 3.5 rounds to 4.
    let round_up = diff > 0.5 || (diff == 0.5 && (f as i64) % 2 != 0);
    if round_up { f + 1.0 } else { f }
}

/// FP68K's odd-numbered operations manage the floating-point environment
/// (rounding mode, exception flags) rather than values.
pub mod envop {
    pub const SETENV: u16 = 0x01;
    pub const GETENV: u16 = 0x03;
    pub const SETHV: u16 = 0x05;
    pub const GETHV: u16 = 0x07;
    pub const NEG: u16 = 0x0D;
    pub const ABS: u16 = 0x0F;
    pub const CPYSGN: u16 = 0x11;
    pub const SETXCP: u16 = 0x15;
    pub const PROCENTRY: u16 = 0x17;
    pub const PROCEXIT: u16 = 0x19;
    pub const TESTXCP: u16 = 0x1B;
}

/// How many address operands an operation takes.
#[must_use]
pub fn operand_count(operation: u16) -> u8 {
    match operation {
        op::SQRT | op::RTI | op::TTI | op::LOGB => 1,
        envop::SETENV | envop::GETENV | envop::SETHV | envop::GETHV | envop::NEG
        | envop::ABS | envop::SETXCP | envop::PROCENTRY | envop::PROCEXIT
        | envop::TESTXCP => 1,
        _ => 2,
    }
}

/// Result of dispatching a SANE call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaneResult {
    /// Handled; nothing else to do.
    Done,
    /// Handled, and the comparison outcome must go into the CCR/`D0`.
    Compared(std::cmp::Ordering),
    /// Handled; the operands were unordered, which only a comparison can report.
    Unordered,
}

/// Condition codes for a SANE comparison result.
///
/// The caller's next instruction is an ordinary `bgt` / `blt` / `beq`, so the
/// flags have to be what a signed `sub` would have left. On the 68000 `bgt` is
/// `Z=0 && N==V` and `blt` is `N!=V`, which fixes the encoding:
///
/// | ordering | Z | N | V | reads as |
/// |---|---|---|---|---|
/// | greater | 0 | 0 | 0 | `bgt`, `bge`, `bne` |
/// | less | 0 | 1 | 0 | `blt`, `ble`, `bne` |
/// | equal | 1 | 0 | 0 | `beq`, `ble`, `bge` |
/// | unordered | 0 | 0 | 1 | `blt`, `ble`, `bne` — never `bgt` |
///
/// Unordered sets `V` alone, so `N != V` makes every "less" branch taken and
/// every "greater" branch not taken. That is the safe direction: a loop written
/// as `while (x > limit)` terminates on a NaN rather than spinning on it.
#[must_use]
pub fn comparison_ccr(result: SaneResult) -> u8 {
    use ad_m68k::ccr;
    use std::cmp::Ordering;
    match result {
        SaneResult::Compared(Ordering::Greater) | SaneResult::Done => 0,
        SaneResult::Compared(Ordering::Less) => ccr::NEGATIVE,
        SaneResult::Compared(Ordering::Equal) => ccr::ZERO,
        SaneResult::Unordered => ccr::OVERFLOW,
    }
}

/// Perform an FP68K operation.
///
/// `dst` is the destination and, for arithmetic, also the left operand. Returns
/// `None` for an operation this runtime does not implement, so the caller can
/// report the opword rather than silently computing the wrong thing.
pub fn fp68k(
    mem: &mut Memory,
    opword: u16,
    dst: u32,
    src: Option<u32>,
) -> Option<SaneResult> {
    let fmt = opword & format::MASK;
    let operation = opword & op::MASK;

    // Environment operations first: they move a 16-bit environment word, not a
    // floating-point value. This runtime keeps the default environment (round
    // to nearest, no halts), so saves read as zero and restores are accepted.
    match operation {
        envop::GETENV | envop::PROCENTRY => {
            // PROCENTRY additionally resets to defaults — which we always are.
            mem.write_u16(dst, 0);
            return Some(SaneResult::Done);
        }
        envop::SETENV | envop::SETHV | envop::SETXCP | envop::PROCEXIT => {
            // Accept and discard: the environment cannot leave its defaults.
            return Some(SaneResult::Done);
        }
        // Same response for both: this runtime has no halt vector and never
        // raises an exception, so the saved word is zero either way.
        envop::GETHV | envop::TESTXCP => {
            // No exception is ever signalled: the environment stays at defaults.
            mem.write_u16(dst, 0);
            return Some(SaneResult::Done);
        }
        envop::NEG => {
            let v = read_ext(mem, dst);
            write_ext(mem, dst, -v);
            return Some(SaneResult::Done);
        }
        envop::ABS => {
            let v = read_ext(mem, dst);
            write_ext(mem, dst, v.abs());
            return Some(SaneResult::Done);
        }
        envop::CPYSGN => {
            // CpySgn(dst, src): destination takes the sign of the source.
            let s = src?;
            let sv = read_operand(mem, s, fmt);
            let d = read_ext(mem, dst);
            let out = if sv.is_sign_negative() { -d.abs() } else { d.abs() };
            write_ext(mem, dst, out);
            return Some(SaneResult::Done);
        }
        _ => {}
    }

    // Z2X and X2Z convert between formats; the format bits describe the
    // non-extended side, so they need handling before the arithmetic cases.
    match operation {
        op::Z2X => {
            let s = src?;
            let v = read_operand(mem, s, fmt);
            write_ext(mem, dst, v);
            return Some(SaneResult::Done);
        }
        op::X2Z => {
            let s = src?;
            let v = read_ext(mem, s);
            write_operand(mem, dst, fmt, v);
            return Some(SaneResult::Done);
        }
        _ => {}
    }

    let d = read_ext(mem, dst);
    let s = src.map(|a| read_operand(mem, a, fmt));

    let out = match operation {
        op::ADD => d + s?,
        op::SUB => d - s?,
        op::MUL => d * s?,
        op::DIV => d / s?,
        op::SQRT => d.sqrt(),
        op::REM => {
            let s = s?;
            if s == 0.0 { f64::NAN } else { d % s }
        }
        op::RTI => round_to_nearest(d),
        op::TTI => d.trunc(),
        op::SCALB => {
            // SCALB(n, x): scale x by 2^n, where n is the *source* integer.
            let n = s? as i32;
            ldexp(d, n)
        }
        op::LOGB => {
            if d == 0.0 || !d.is_finite() {
                d
            } else {
                d.abs().log2().floor()
            }
        }
        op::CMP | op::CPX => {
            let s = s?;
            // `partial_cmp` is `None` exactly when an operand is NaN, which is
            // SANE's *unordered* — a fourth outcome, not a flavour of "greater".
            // Reporting it as greater is what a `while (x > limit)` loop needs
            // least: it would spin forever on a NaN.
            return Some(match d.partial_cmp(&s) {
                Some(ord) => SaneResult::Compared(ord),
                None => SaneResult::Unordered,
            });
        }
        op::CLASS => {
            // Class of the source, reported into the destination as an integer.
            let s = s?;
            let class: i16 = if s.is_nan() {
                if s.to_bits() & (1 << 51) != 0 { 1 } else { 2 }
            } else if s.is_infinite() {
                3
            } else if s == 0.0 {
                5
            } else if s.abs() < f64::MIN_POSITIVE {
                6
            } else {
                4
            };
            let signed = if s.is_sign_negative() { -class } else { class };
            mem.write_u16(dst, signed as u16);
            return Some(SaneResult::Done);
        }
        _ => return None,
    };

    write_ext(mem, dst, out);
    Some(SaneResult::Done)
}

/// Perform an Elems68K (transcendental) operation.
pub fn elems68k(mem: &mut Memory, opword: u16, dst: u32, src: Option<u32>) -> Option<SaneResult> {
    let operation = opword & 0x003F;
    let d = read_ext(mem, dst);
    let s = src.map(|a| read_ext(mem, a));

    let out = match operation {
        elem::LNX => d.ln(),
        elem::LOG2X => d.log2(),
        elem::LN1X => d.ln_1p(),
        elem::LOG21X => (1.0 + d).log2(),
        elem::EXPX => d.exp(),
        elem::EXP2X => d.exp2(),
        elem::EXP1X => d.exp_m1(),
        elem::EXP21X => d.exp2() - 1.0,
        elem::SINX => d.sin(),
        elem::COSX => d.cos(),
        elem::TANX => d.tan(),
        elem::ATANX => d.atan(),
        // XPWRI(i, x) = x^i and XPWRY(y, x) = x^y, with the exponent in src.
        elem::XPWRI => d.powi(s? as i32),
        elem::XPWRY => d.powf(s?),
        elem::COMPOUND => (1.0 + s?).powf(d),
        elem::ANNUITY => {
            let r = s?;
            if r == 0.0 { d } else { (1.0 - (1.0 + r).powf(-d)) / r }
        }
        // RANDX advances SANE's own seed; modules that want reproducible noise use
        // _Random instead, so this is deliberately left unimplemented rather than
        // guessed at.
        _ => return None,
    };

    write_ext(mem, dst, out);
    Some(SaneResult::Done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Memory {
        Memory::new()
    }

    #[test]
    fn extended_round_trips_exactly_for_representable_values() {
        let mut m = mem();
        // Every one of these is exact in both f64 and 80-bit extended.
        for v in [
            0.0, 1.0, -1.0, 2.0, 0.5, -0.25, 3.0, 1024.0, -4096.0, 1e10, -1e-10,
            0.1, 12345.678,
        ] {
            write_ext(&mut m, 0x3000, v);
            let back = read_ext(&mut m, 0x3000);
            assert!(
                (back - v).abs() <= v.abs() * 1e-15,
                "{v} round-tripped to {back}"
            );
        }
    }

    #[test]
    fn extended_layout_is_sign_exponent_explicit_mantissa() {
        let mut m = mem();
        write_ext(&mut m, 0x3000, 1.0);
        // 1.0 = mantissa 0x8000000000000000 with biased exponent 16383.
        assert_eq!(m.read_u16(0x3000), 16383);
        assert_eq!(m.read_u32(0x3002), 0x8000_0000);
        assert_eq!(m.read_u32(0x3006), 0);

        write_ext(&mut m, 0x3000, -2.0);
        assert_eq!(m.read_u16(0x3000), 0x8000 | 16384, "sign bit plus exponent");
        assert_eq!(m.read_u32(0x3002), 0x8000_0000);
    }

    #[test]
    fn zero_and_infinity_have_their_special_encodings() {
        let mut m = mem();
        write_ext(&mut m, 0x3000, 0.0);
        assert_eq!(m.read_u16(0x3000), 0);
        assert_eq!(m.read_u32(0x3002), 0);
        assert_eq!(read_ext(&mut m, 0x3000), 0.0);

        write_ext(&mut m, 0x3010, f64::INFINITY);
        assert_eq!(m.read_u16(0x3010), 0x7FFF);
        assert_eq!(m.read_u32(0x3012), 0);
        assert!(read_ext(&mut m, 0x3010).is_infinite());

        write_ext(&mut m, 0x3020, f64::NAN);
        assert!(read_ext(&mut m, 0x3020).is_nan());
    }

    #[test]
    fn z2x_widens_an_integer_the_way_the_modules_call_it() {
        let mut m = mem();
        // This is Rainstorm's actual call: opword $200E = FFINT | FOZ2X.
        m.write_u16(0x4000, 300u16);
        let r = fp68k(&mut m, 0x200E, 0x4010, Some(0x4000));
        assert_eq!(r, Some(SaneResult::Done));
        assert_eq!(read_ext(&mut m, 0x4010), 300.0);

        // Negative integers must sign-extend.
        m.write_u16(0x4000, (-7i16) as u16);
        fp68k(&mut m, 0x200E, 0x4010, Some(0x4000)).expect("handled");
        assert_eq!(read_ext(&mut m, 0x4010), -7.0);
    }

    #[test]
    fn x2z_narrows_back_with_round_to_nearest_even() {
        let mut m = mem();
        for (v, want) in [(2.5f64, 2i16), (3.5, 4), (-2.5, -2), (1.4, 1), (1.6, 2)] {
            write_ext(&mut m, 0x4000, v);
            fp68k(&mut m, format::INT | op::X2Z, 0x4010, Some(0x4000)).expect("handled");
            assert_eq!(m.read_u16(0x4010) as i16, want, "for {v}");
        }
    }

    #[test]
    fn arithmetic_operates_on_the_destination() {
        let mut m = mem();
        write_ext(&mut m, 0x4000, 10.0); // dst
        write_ext(&mut m, 0x4010, 3.0); // src
        fp68k(&mut m, op::ADD, 0x4000, Some(0x4010)).expect("handled");
        assert_eq!(read_ext(&mut m, 0x4000), 13.0);

        write_ext(&mut m, 0x4000, 10.0);
        fp68k(&mut m, op::SUB, 0x4000, Some(0x4010)).expect("handled");
        assert_eq!(read_ext(&mut m, 0x4000), 7.0, "dst - src, not src - dst");

        write_ext(&mut m, 0x4000, 10.0);
        fp68k(&mut m, op::MUL, 0x4000, Some(0x4010)).expect("handled");
        assert_eq!(read_ext(&mut m, 0x4000), 30.0);

        write_ext(&mut m, 0x4000, 12.0);
        fp68k(&mut m, op::DIV, 0x4000, Some(0x4010)).expect("handled");
        assert_eq!(read_ext(&mut m, 0x4000), 4.0);
    }

    #[test]
    fn sqrt_is_a_one_operand_operation() {
        let mut m = mem();
        assert_eq!(operand_count(op::SQRT), 1);
        write_ext(&mut m, 0x4000, 144.0);
        // NightLines' actual call: opword $0012 = FFEXT | FOSQRT.
        fp68k(&mut m, 0x0012, 0x4000, None).expect("handled");
        assert_eq!(read_ext(&mut m, 0x4000), 12.0);
    }

    #[test]
    fn compare_reports_an_ordering() {
        use std::cmp::Ordering;
        let mut m = mem();
        write_ext(&mut m, 0x4000, 1.0);
        write_ext(&mut m, 0x4010, 2.0);
        assert_eq!(
            fp68k(&mut m, op::CMP, 0x4000, Some(0x4010)),
            Some(SaneResult::Compared(Ordering::Less))
        );
        write_ext(&mut m, 0x4000, 5.0);
        assert_eq!(
            fp68k(&mut m, op::CMP, 0x4000, Some(0x4010)),
            Some(SaneResult::Compared(Ordering::Greater))
        );
    }

    #[test]
    fn mixed_format_sources_are_read_in_their_own_format() {
        let mut m = mem();
        write_ext(&mut m, 0x4000, 1.0);
        // Add a single-precision 2.5 to an extended 1.0.
        m.write_u32(0x4010, (2.5f32).to_bits());
        fp68k(&mut m, format::SGL | op::ADD, 0x4000, Some(0x4010)).expect("handled");
        assert_eq!(read_ext(&mut m, 0x4000), 3.5);

        // And a double.
        write_ext(&mut m, 0x4000, 1.0);
        let b = (0.5f64).to_bits();
        m.write_u32(0x4010, (b >> 32) as u32);
        m.write_u32(0x4014, b as u32);
        fp68k(&mut m, format::DBL | op::ADD, 0x4000, Some(0x4010)).expect("handled");
        assert_eq!(read_ext(&mut m, 0x4000), 1.5);
    }

    #[test]
    fn transcendentals_are_accurate_enough_to_animate_with() {
        let mut m = mem();
        for (opc, x, want) in [
            (elem::SINX, 0.0f64, 0.0f64),
            (elem::COSX, 0.0, 1.0),
            (elem::EXPX, 1.0, std::f64::consts::E),
            (elem::LNX, std::f64::consts::E, 1.0),
            (elem::ATANX, 1.0, std::f64::consts::FRAC_PI_4),
        ] {
            write_ext(&mut m, 0x4000, x);
            elems68k(&mut m, opc, 0x4000, None).expect("handled");
            let got = read_ext(&mut m, 0x4000);
            assert!((got - want).abs() < 1e-12, "op {opc:#x}({x}) = {got}, want {want}");
        }
    }

    #[test]
    fn unknown_opword_is_reported_not_guessed() {
        let mut m = mem();
        // A silently-wrong result here would show up as subtly wrong motion much
        // later, so an unimplemented operation must refuse.
        assert_eq!(fp68k(&mut m, 0x001E, 0x4000, Some(0x4010)), None);
        assert_eq!(elems68k(&mut m, 0x0020, 0x4000, None), None);
    }

    #[test]
    fn format_sizes_match_the_spec() {
        assert_eq!(format_size(format::EXT), 10);
        assert_eq!(format_size(format::DBL), 8);
        assert_eq!(format_size(format::SGL), 4);
        assert_eq!(format_size(format::INT), 2);
        assert_eq!(format_size(format::LNG), 4);
        assert_eq!(format_size(format::COMP), 8);
    }
}
