//! `CopyBits` — the real blitter.
//!
//! This is the difference between "runs clean" and "looks like After Dark".
//! Fifteen modules completed their whole lifecycle while rendering nothing,
//! because they composite every animation frame out of offscreen bitmaps and the
//! previous `CopyBits` ignored the source entirely.
//!
//! # Telling the three shapes apart
//!
//! A `CopyBits` argument may be any of three things, and the word at **+4**
//! decides which. Color QuickDraw chose the `CGrafPort` marker specifically so
//! this test works:
//!
//! ```text
//! (w & 0xC000) == 0xC000   CGrafPort.portBits — +0 is a PixMapHandle, deref it
//! (w & 0x8000) != 0        PixMap at this address
//! otherwise                BitMap at this address (1 bit deep)
//! ```
//!
//! Flying Toasters passes `&port->portBits` on a colour port, so `rowBytes`
//! reads as `0xC000` — its `portVersion`. Treating that as a row stride yields
//! 16384 and a bitmap with empty bounds, which is exactly the nothing that was
//! being drawn.
//!
//! # Depths
//!
//! 1, 2, 4 and 8 bits per pixel, packed big-endian within each byte (the
//! left-most pixel occupies the high-order bits). Fish! blits 4-bit sprites into
//! an 8-bit offscreen buffer, so depth conversion is not optional.

use ad_memory::Memory;

use crate::quickdraw::Rect;

/// Transfer modes. The low three bits select the boolean operation; bit 3
/// distinguishes pattern modes from source modes.
pub mod mode {
    pub const SRC_COPY: i16 = 0;
    pub const SRC_OR: i16 = 1;
    pub const SRC_XOR: i16 = 2;
    pub const SRC_BIC: i16 = 3;
    pub const NOT_SRC_COPY: i16 = 4;
    pub const NOT_SRC_OR: i16 = 5;
    pub const NOT_SRC_XOR: i16 = 6;
    pub const NOT_SRC_BIC: i16 = 7;
    /// `transparent`: the source's background colour is not copied.
    pub const TRANSPARENT: i16 = 36;
    /// Dithering is a quality hint layered onto another mode.
    pub const DITHER: i16 = 0x40;
}

/// Field offsets shared by `BitMap` and `PixMap`.
///
/// Public because `SetOrigin` has to reach the same boundary rectangle the
/// blitter reads, and through the same `portVersion` discrimination — two
/// copies of that rule would be two chances to disagree about whether a port
/// is colour.
pub mod off {
    pub const BASE_ADDR: u32 = 0;
    pub const ROW_BYTES: u32 = 4;
    pub const BOUNDS: u32 = 6;
    pub const PIXEL_SIZE: u32 = 32;
    pub const PM_TABLE: u32 = 42;
}

/// A resolved drawing surface in emulated memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Surface {
    pub base: u32,
    pub row_bytes: u32,
    pub bounds: Rect,
    /// Bits per pixel: 1, 2, 4 or 8.
    pub pixel_size: u16,
    /// `pmTable`, a `CTabHandle`, or 0 for a plain `BitMap`.
    pub color_table: u32,
}

impl Surface {
    /// Resolve a `CopyBits` argument into a surface.
    ///
    /// Returns `None` if the shape is unusable — a nil base address or a stride
    /// that could not describe a real bitmap. Callers should treat that as "skip
    /// this blit" rather than drawing garbage.
    #[must_use]
    pub fn resolve(mem: &mut Memory, addr: u32) -> Option<Self> {
        if addr == 0 {
            return None;
        }
        let marker = mem.read_u16(addr.wrapping_add(off::ROW_BYTES));

        // A CGrafPort's portVersion has both top bits set; the field before it is
        // a PixMapHandle, not a base address.
        let pm = if marker & 0xC000 == 0xC000 {
            let handle = mem.read_u32(addr.wrapping_add(off::BASE_ADDR));
            mem.deref_handle(handle)?
        } else {
            addr
        };

        let row_word = mem.read_u16(pm.wrapping_add(off::ROW_BYTES));
        let is_pixmap = row_word & 0x8000 != 0;
        let row_bytes = u32::from(row_word & 0x3FFF);
        let base = mem.read_u32(pm.wrapping_add(off::BASE_ADDR));
        let bounds = Rect::read(mem, pm.wrapping_add(off::BOUNDS));

        if base == 0 || row_bytes == 0 || bounds.is_empty() {
            return None;
        }
        let (pixel_size, color_table) = if is_pixmap {
            let ps = mem.read_u16(pm.wrapping_add(off::PIXEL_SIZE));
            let ct = mem.read_u32(pm.wrapping_add(off::PM_TABLE));
            (if matches!(ps, 1 | 2 | 4 | 8) { ps } else { 8 }, ct)
        } else {
            (1, 0)
        };
        Some(Self {
            base,
            row_bytes,
            bounds,
            pixel_size,
            color_table,
        })
    }

    /// Read the raw pixel index at a point in this surface's own coordinates.
    #[must_use]
    pub fn get(&self, mem: &mut Memory, x: i32, y: i32) -> Option<u8> {
        if !self.bounds.contains(i16::try_from(x).ok()?, i16::try_from(y).ok()?) {
            return None;
        }
        let col = u32::try_from(x - i32::from(self.bounds.left)).ok()?;
        let row = u32::try_from(y - i32::from(self.bounds.top)).ok()?;
        let bit = col.checked_mul(u32::from(self.pixel_size))?;
        let addr = self
            .base
            .wrapping_add(row.wrapping_mul(self.row_bytes))
            .wrapping_add(bit / 8);
        let byte = mem.read_u8(addr);
        Some(match self.pixel_size {
            8 => byte,
            // Left-most pixel in the high-order bits.
            4 => (byte >> (4 - (bit % 8))) & 0x0F,
            2 => (byte >> (6 - (bit % 8))) & 0x03,
            _ => (byte >> (7 - (bit % 8))) & 0x01,
        })
    }

    /// Write a raw pixel index, preserving neighbours at sub-byte depths.
    pub fn set(&self, mem: &mut Memory, x: i32, y: i32, value: u8) {
        let (Ok(xi), Ok(yi)) = (i16::try_from(x), i16::try_from(y)) else {
            return;
        };
        if !self.bounds.contains(xi, yi) {
            return;
        }
        let Ok(col) = u32::try_from(x - i32::from(self.bounds.left)) else {
            return;
        };
        let Ok(row) = u32::try_from(y - i32::from(self.bounds.top)) else {
            return;
        };
        let bit = col.saturating_mul(u32::from(self.pixel_size));
        let addr = self
            .base
            .wrapping_add(row.wrapping_mul(self.row_bytes))
            .wrapping_add(bit / 8);
        if self.pixel_size == 8 {
            mem.write_u8(addr, value);
            return;
        }
        let (width, shift) = match self.pixel_size {
            4 => (4u32, 4 - (bit % 8)),
            2 => (2, 6 - (bit % 8)),
            _ => (1, 7 - (bit % 8)),
        };
        let mask = ((1u16 << width) - 1) as u8;
        let old = mem.read_u8(addr);
        let cleared = old & !(mask << shift);
        mem.write_u8(addr, cleared | ((value & mask) << shift));
    }

    /// The RGB a source index stands for, from this surface's colour table.
    #[must_use]
    pub fn rgb_of(&self, mem: &mut Memory, index: u8) -> Option<[u8; 3]> {
        let table = mem.deref_handle(self.color_table)?;
        let size = mem.read_u16(table.wrapping_add(6)); // ctSize = entries - 1
        if u16::from(index) > size {
            return None;
        }
        // ColorSpec[]: { u16 value; u16 rgb[3] } at +8.
        let spec = table.wrapping_add(8).wrapping_add(u32::from(index) * 8);
        Some([
            (mem.read_u16(spec.wrapping_add(2)) >> 8) as u8,
            (mem.read_u16(spec.wrapping_add(4)) >> 8) as u8,
            (mem.read_u16(spec.wrapping_add(6)) >> 8) as u8,
        ])
    }
}

/// Apply a boolean transfer mode to a destination pixel.
fn combine(mode: i16, src: u8, dst: u8, fore: u8, back: u8) -> Option<u8> {
    Some(match mode & 0x07 {
        mode::SRC_COPY => src,
        // "Or" leaves the destination where the source is background.
        mode::SRC_OR => {
            if src == back {
                return None;
            }
            src
        }
        mode::SRC_XOR => dst ^ src,
        mode::SRC_BIC => {
            if src == back {
                return None;
            }
            back
        }
        mode::NOT_SRC_COPY => !src,
        mode::NOT_SRC_OR => {
            if src != back {
                return None;
            }
            fore
        }
        mode::NOT_SRC_XOR => dst ^ !src,
        _ => {
            if src != back {
                return None;
            }
            back
        }
    })
}

/// Copy a rectangle between two surfaces, scaling and converting depth.
///
/// `fore`/`back` are the destination port's colours, used for 1-bit sources —
/// where a set bit means foreground and a clear bit background, per the
/// Toolbox's documented behaviour.
///
/// `to_dest_index` maps an RGB triple to the destination's palette, used when a
/// source carries its own colour table and the depths differ.
#[allow(
    clippy::too_many_arguments,
    reason = "mirrors the Toolbox CopyBits signature plus the depth-conversion hook"
)]
pub fn copy_bits(
    mem: &mut Memory,
    src: &Surface,
    dst: &Surface,
    src_rect: &Rect,
    dst_rect: &Rect,
    mode: i16,
    fore: u8,
    back: u8,
    mask: Option<&Rect>,
    to_dest_index: &dyn Fn(&mut Memory, [u8; 3]) -> u8,
) {
    let mode = mode & !mode::DITHER;
    let (sw, sh) = (src_rect.width().max(1), src_rect.height().max(1));
    let (dw, dh) = (dst_rect.width(), dst_rect.height());
    if dw <= 0 || dh <= 0 {
        return;
    }

    for dy in 0..dh {
        // Nearest-neighbour: integer maths only, so a replay is reproducible.
        let sy = i32::from(src_rect.top) + dy * sh / dh;
        let out_y = i32::from(dst_rect.top) + dy;
        for dx in 0..dw {
            let sx = i32::from(src_rect.left) + dx * sw / dw;
            let out_x = i32::from(dst_rect.left) + dx;
            if let Some(m) = mask {
                if !m.contains(out_x as i16, out_y as i16) {
                    continue;
                }
            }
            let Some(raw) = src.get(mem, sx, sy) else {
                continue; // outside the source: QuickDraw clips
            };

            // Translate the source index into the destination's terms.
            let value = if src.pixel_size == 1 {
                if raw != 0 { fore } else { back }
            } else if src.pixel_size == dst.pixel_size {
                raw
            } else if let Some(rgb) = src.rgb_of(mem, raw) {
                to_dest_index(mem, rgb)
            } else {
                // No colour table and depths differ: scale the index into range.
                let from = (1u16 << src.pixel_size).saturating_sub(1).max(1);
                let to = (1u16 << dst.pixel_size).saturating_sub(1);
                ((u16::from(raw) * to) / from) as u8
            };

            let old = dst.get(mem, out_x, out_y).unwrap_or(0);
            if let Some(v) = combine(mode, value, old, fore, back) {
                dst.set(mem, out_x, out_y, v);
            }
        }
    }
}

/// `_CopyMask`: copy `src_rect` into `dst_rect`, but only where the mask allows.
///
/// The mask is sampled over `mask_rect` in step with the destination, so the
/// three rectangles need not be the same size. A mask pixel of zero protects the
/// destination; any other value lets the source through. Colour depth is handled
/// exactly as `copy_bits` does, and the transfer is always `srcCopy` — there is
/// no mode argument, because the mask *is* the mode.
#[allow(
    clippy::too_many_arguments,
    reason = "mirrors the Toolbox CopyMask signature plus the depth-conversion hook"
)]
pub fn copy_mask(
    mem: &mut Memory,
    src: &Surface,
    mask: &Surface,
    dst: &Surface,
    src_rect: &Rect,
    mask_rect: &Rect,
    dst_rect: &Rect,
    fore: u8,
    back: u8,
    to_dest_index: &dyn Fn(&mut Memory, [u8; 3]) -> u8,
) {
    let (dw, dh) = (dst_rect.width(), dst_rect.height());
    if dw <= 0 || dh <= 0 {
        return;
    }
    let (sw, sh) = (src_rect.width().max(1), src_rect.height().max(1));
    let (mw, mh) = (mask_rect.width().max(1), mask_rect.height().max(1));

    for dy in 0..dh {
        let out_y = i32::from(dst_rect.top) + dy;
        let sy = i32::from(src_rect.top) + dy * sh / dh;
        let my = i32::from(mask_rect.top) + dy * mh / dh;
        for dx in 0..dw {
            let out_x = i32::from(dst_rect.left) + dx;
            let mx = i32::from(mask_rect.left) + dx * mw / dw;
            if mask.get(mem, mx, my).unwrap_or(0) == 0 {
                continue;
            }
            let sx = i32::from(src_rect.left) + dx * sw / dw;
            let Some(raw) = src.get(mem, sx, sy) else {
                continue;
            };
            let value = if src.pixel_size == 1 {
                if raw != 0 { fore } else { back }
            } else if src.pixel_size == dst.pixel_size {
                raw
            } else if let Some(rgb) = src.rgb_of(mem, raw) {
                to_dest_index(mem, rgb)
            } else {
                let from = (1u16 << src.pixel_size).saturating_sub(1).max(1);
                let to = (1u16 << dst.pixel_size).saturating_sub(1);
                ((u16::from(raw) * to) / from) as u8
            };
            dst.set(mem, out_x, out_y, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Memory {
        Memory::new()
    }

    /// Lay out a PixMap header and return its address.
    fn pixmap(m: &mut Memory, at: u32, base: u32, row_bytes: u16, w: i16, h: i16, depth: u16) -> u32 {
        m.write_u32(at, base);
        m.write_u16(at + 4, 0x8000 | row_bytes);
        Rect::new(0, 0, h, w).write(m, at + 6);
        m.write_u16(at + 32, depth);
        m.write_u32(at + 42, 0);
        at
    }

    #[test]
    fn resolves_a_plain_bitmap_as_one_bit_deep() {
        let mut m = mem();
        let at = 0x0020_0000;
        m.write_u32(at, 0x0021_0000);
        m.write_u16(at + 4, 8); // no high bit: a BitMap
        Rect::new(0, 0, 16, 64).write(&mut m, at + 6);
        let s = Surface::resolve(&mut m, at).expect("resolve");
        assert_eq!(s.pixel_size, 1, "a BitMap is one bit deep");
        assert_eq!(s.row_bytes, 8);
        assert_eq!(s.color_table, 0);
    }

    #[test]
    fn resolves_a_cgrafport_portbits_through_its_handle() {
        // This is the shape Flying Toasters passes. Read naively, rowBytes comes
        // back as 0xC000 — the portVersion — and the surface looks empty, which
        // is why nothing was drawn.
        let mut m = mem();
        let pm_store = 0x0022_0000;
        let h = m.new_handle(64, true);
        let pm = m.deref_handle(h).expect("block");
        pixmap(&mut m, pm, 0x0023_0000, 640, 640, 480, 8);

        let port = 0x0024_0000;
        m.write_u32(port + 2, h); // portPixMap
        m.write_u16(port + 6, 0xC000); // portVersion marks a CGrafPort

        let s = Surface::resolve(&mut m, port + 2).expect("resolve via handle");
        assert_eq!(s.pixel_size, 8);
        assert_eq!(s.row_bytes, 640);
        assert_eq!(s.base, 0x0023_0000);
        assert_eq!(s.bounds, Rect::new(0, 0, 480, 640));
        let _ = pm_store;
    }

    #[test]
    fn rejects_unusable_shapes_instead_of_drawing_garbage() {
        let mut m = mem();
        assert_eq!(Surface::resolve(&mut m, 0), None, "nil pointer");
        let at = 0x0025_0000;
        m.write_u32(at, 0); // nil base address
        m.write_u16(at + 4, 8);
        Rect::new(0, 0, 8, 8).write(&mut m, at + 6);
        assert_eq!(Surface::resolve(&mut m, at), None, "nil base");
    }

    #[test]
    fn sub_byte_depths_pack_left_to_right() {
        let mut m = mem();
        let base = 0x0026_0000;
        let hdr = pixmap(&mut m, 0x0026_1000, base, 4, 8, 1, 4);
        let s = Surface::resolve(&mut m, hdr).expect("resolve");
        assert_eq!(s.pixel_size, 4);

        // Two 4-bit pixels per byte, left-most in the high nibble.
        s.set(&mut m, 0, 0, 0xA);
        s.set(&mut m, 1, 0, 0x3);
        assert_eq!(m.read_u8(base), 0xA3, "high nibble is the left pixel");
        assert_eq!(s.get(&mut m, 0, 0), Some(0xA));
        assert_eq!(s.get(&mut m, 1, 0), Some(0x3));

        // Writing one pixel must not disturb its neighbour.
        s.set(&mut m, 1, 0, 0xF);
        assert_eq!(s.get(&mut m, 0, 0), Some(0xA), "neighbour survived");
    }

    #[test]
    fn one_bit_pixels_read_most_significant_first() {
        let mut m = mem();
        let base = 0x0027_0000;
        let at = 0x0027_1000;
        m.write_u32(at, base);
        m.write_u16(at + 4, 2);
        Rect::new(0, 0, 1, 16).write(&mut m, at + 6);
        let s = Surface::resolve(&mut m, at).expect("resolve");
        m.write_u8(base, 0b1000_0001);
        assert_eq!(s.get(&mut m, 0, 0), Some(1), "bit 7 is pixel 0");
        assert_eq!(s.get(&mut m, 1, 0), Some(0));
        assert_eq!(s.get(&mut m, 7, 0), Some(1));
    }

    #[test]
    fn one_bit_source_becomes_foreground_and_background() {
        let mut m = mem();
        // 8x1 one-bit source, alternating.
        let sbase = 0x0028_0000;
        let sh = 0x0028_1000;
        m.write_u32(sh, sbase);
        m.write_u16(sh + 4, 2);
        Rect::new(0, 0, 1, 8).write(&mut m, sh + 6);
        m.write_u8(sbase, 0b1010_1010);
        let src = Surface::resolve(&mut m, sh).expect("src");

        let dbase = 0x0029_0000;
        let dh = pixmap(&mut m, 0x0029_1000, dbase, 8, 8, 1, 8);
        let dst = Surface::resolve(&mut m, dh).expect("dst");

        let ident = |_: &mut Memory, _: [u8; 3]| 0u8;
        copy_bits(
            &mut m,
            &src,
            &dst,
            &Rect::new(0, 0, 1, 8),
            &Rect::new(0, 0, 1, 8),
            mode::SRC_COPY,
            200, // fore
            7,   // back
            None,
            &ident,
        );
        assert_eq!(dst.get(&mut m, 0, 0), Some(200), "set bit -> foreground");
        assert_eq!(dst.get(&mut m, 1, 0), Some(7), "clear bit -> background");
        assert_eq!(dst.get(&mut m, 2, 0), Some(200));
    }

    #[test]
    fn src_or_leaves_the_destination_where_the_source_is_background() {
        // srcOr is how sprites composite without erasing what is behind them.
        let mut m = mem();
        let sbase = 0x002A_0000;
        let sh = 0x002A_1000;
        m.write_u32(sh, sbase);
        m.write_u16(sh + 4, 2);
        Rect::new(0, 0, 1, 8).write(&mut m, sh + 6);
        m.write_u8(sbase, 0b1000_0000); // only pixel 0 set
        let src = Surface::resolve(&mut m, sh).expect("src");

        let dbase = 0x002B_0000;
        let dh = pixmap(&mut m, 0x002B_1000, dbase, 8, 8, 1, 8);
        let dst = Surface::resolve(&mut m, dh).expect("dst");
        for x in 0..8 {
            dst.set(&mut m, x, 0, 99); // existing content
        }

        let ident = |_: &mut Memory, _: [u8; 3]| 0u8;
        copy_bits(
            &mut m, &src, &dst,
            &Rect::new(0, 0, 1, 8), &Rect::new(0, 0, 1, 8),
            mode::SRC_OR, 200, 7, None, &ident,
        );
        assert_eq!(dst.get(&mut m, 0, 0), Some(200), "set bit drew");
        assert_eq!(
            dst.get(&mut m, 1, 0),
            Some(99),
            "background left the destination alone"
        );
    }

    #[test]
    fn src_xor_inverts_and_is_its_own_inverse() {
        // XOR erase-and-redraw is the animation idiom of this era: blitting the
        // same sprite twice must restore the background exactly.
        let mut m = mem();
        let sbase = 0x002C_0000;
        let sh = pixmap(&mut m, 0x002C_1000, sbase, 8, 8, 1, 8);
        let src = Surface::resolve(&mut m, sh).expect("src");
        for x in 0..8 {
            src.set(&mut m, x, 0, 0x0F);
        }
        let dbase = 0x002D_0000;
        let dh = pixmap(&mut m, 0x002D_1000, dbase, 8, 8, 1, 8);
        let dst = Surface::resolve(&mut m, dh).expect("dst");
        for x in 0..8 {
            dst.set(&mut m, x, 0, 0x33);
        }

        let ident = |_: &mut Memory, _: [u8; 3]| 0u8;
        let r = Rect::new(0, 0, 1, 8);
        for _ in 0..2 {
            copy_bits(&mut m, &src, &dst, &r, &r, mode::SRC_XOR, 255, 0, None, &ident);
        }
        assert_eq!(
            dst.get(&mut m, 3, 0),
            Some(0x33),
            "two XOR passes must restore the original"
        );
    }

    #[test]
    fn depth_conversion_uses_the_source_colour_table() {
        // Fish! blits 4-bit sprites into an 8-bit buffer.
        let mut m = mem();
        let ct = m.new_handle(8 + 16 * 8, true);
        let ctb = m.deref_handle(ct).expect("ct");
        m.write_u16(ctb + 6, 15); // ctSize = entries - 1
        // Entry 5 is pure green.
        let spec = ctb + 8 + 5 * 8;
        m.write_u16(spec, 5);
        m.write_u16(spec + 2, 0x0000);
        m.write_u16(spec + 4, 0xFFFF);
        m.write_u16(spec + 6, 0x0000);

        let sbase = 0x002E_0000;
        let sh = pixmap(&mut m, 0x002E_1000, sbase, 4, 8, 1, 4);
        m.write_u32(sh + 42, ct); // pmTable
        let src = Surface::resolve(&mut m, sh).expect("src");
        assert_eq!(src.color_table, ct);
        src.set(&mut m, 0, 0, 5);
        assert_eq!(src.rgb_of(&mut m, 5), Some([0, 255, 0]));

        let dbase = 0x002F_0000;
        let dh = pixmap(&mut m, 0x002F_1000, dbase, 8, 8, 1, 8);
        let dst = Surface::resolve(&mut m, dh).expect("dst");

        // Stand-in mapper: green becomes index 42.
        let mapper = |_: &mut Memory, rgb: [u8; 3]| if rgb == [0, 255, 0] { 42 } else { 0 };
        copy_bits(
            &mut m, &src, &dst,
            &Rect::new(0, 0, 1, 1), &Rect::new(0, 0, 1, 1),
            mode::SRC_COPY, 255, 0, None, &mapper,
        );
        assert_eq!(
            dst.get(&mut m, 0, 0),
            Some(42),
            "4-bit index mapped through its colour table"
        );
    }

    #[test]
    fn scales_when_the_rectangles_differ() {
        let mut m = mem();
        let sbase = 0x0030_0000;
        let sh = pixmap(&mut m, 0x0030_1000, sbase, 2, 2, 2, 8);
        let src = Surface::resolve(&mut m, sh).expect("src");
        src.set(&mut m, 0, 0, 11);
        src.set(&mut m, 1, 0, 22);
        src.set(&mut m, 0, 1, 33);
        src.set(&mut m, 1, 1, 44);

        let dbase = 0x0031_0000;
        let dh = pixmap(&mut m, 0x0031_1000, dbase, 4, 4, 4, 8);
        let dst = Surface::resolve(&mut m, dh).expect("dst");

        let ident = |_: &mut Memory, _: [u8; 3]| 0u8;
        copy_bits(
            &mut m, &src, &dst,
            &Rect::new(0, 0, 2, 2), &Rect::new(0, 0, 4, 4),
            mode::SRC_COPY, 255, 0, None, &ident,
        );
        // 2x magnification: each source pixel covers a 2x2 block.
        assert_eq!(dst.get(&mut m, 0, 0), Some(11));
        assert_eq!(dst.get(&mut m, 1, 1), Some(11));
        assert_eq!(dst.get(&mut m, 2, 0), Some(22));
        assert_eq!(dst.get(&mut m, 0, 2), Some(33));
        assert_eq!(dst.get(&mut m, 3, 3), Some(44));
    }

    #[test]
    fn clips_a_source_rect_that_overruns_the_bitmap() {
        // Flying Toasters asks for srcRect top = -6, above its own bounds.
        let mut m = mem();
        let sbase = 0x0032_0000;
        let sh = pixmap(&mut m, 0x0032_1000, sbase, 8, 8, 8, 8);
        let src = Surface::resolve(&mut m, sh).expect("src");
        for y in 0..8 {
            for x in 0..8 {
                src.set(&mut m, x, y, 5);
            }
        }
        let dbase = 0x0033_0000;
        let dh = pixmap(&mut m, 0x0033_1000, dbase, 16, 16, 16, 8);
        let dst = Surface::resolve(&mut m, dh).expect("dst");

        let ident = |_: &mut Memory, _: [u8; 3]| 0u8;
        copy_bits(
            &mut m, &src, &dst,
            &Rect::new(-6, 0, 8, 8), &Rect::new(0, 0, 14, 8),
            mode::SRC_COPY, 255, 0, None, &ident,
        );
        // Rows mapping above the source are skipped, not wrapped or faked.
        assert_eq!(dst.get(&mut m, 0, 0), Some(0), "clipped row untouched");
        assert_eq!(dst.get(&mut m, 0, 13), Some(5), "in-range row copied");
    }

    #[test]
    fn a_mask_rect_confines_the_blit() {
        let mut m = mem();
        let sbase = 0x0034_0000;
        let sh = pixmap(&mut m, 0x0034_1000, sbase, 8, 8, 1, 8);
        let src = Surface::resolve(&mut m, sh).expect("src");
        for x in 0..8 {
            src.set(&mut m, x, 0, 9);
        }
        let dbase = 0x0035_0000;
        let dh = pixmap(&mut m, 0x0035_1000, dbase, 8, 8, 1, 8);
        let dst = Surface::resolve(&mut m, dh).expect("dst");

        let ident = |_: &mut Memory, _: [u8; 3]| 0u8;
        let r = Rect::new(0, 0, 1, 8);
        copy_bits(
            &mut m, &src, &dst, &r, &r,
            mode::SRC_COPY, 255, 0, Some(&Rect::new(0, 2, 1, 5)), &ident,
        );
        assert_eq!(dst.get(&mut m, 1, 0), Some(0), "outside the mask");
        assert_eq!(dst.get(&mut m, 3, 0), Some(9), "inside the mask");
        assert_eq!(dst.get(&mut m, 6, 0), Some(0), "outside the mask");
    }
}
