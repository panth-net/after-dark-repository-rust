//! `PICT` — QuickDraw's recorded-drawing format, and `_DrawPicture`.
//!
//! This is where the sprite modules keep their art. Flying Toasters calls
//! `_DrawPicture` five times to render toasters and toast into offscreen buffers,
//! then composites them with `CopyBits`; Boris ships 201 pictures. With
//! `_DrawPicture` stubbed out, the blitter faithfully copied empty buffers to the
//! screen — modules ran a clean lifecycle and drew nothing.
//!
//! # Format
//!
//! A picture is a size word, a bounding `Rect`, then a stream of opcodes. Version
//! 1 uses **one-byte** opcodes; version 2 announces itself with `$0011 $02FF` and
//! uses **two-byte** opcodes, word-aligned.
//!
//! # Why the size table matters more than the drawing
//!
//! Most opcodes here are skipped, not drawn — but every one must be skipped by
//! *exactly* the right number of bytes. Miss by one and the parser reads an
//! operand as an opcode and paints noise for the rest of the picture. So an
//! opcode of unknown length is a hard error ([`PictError::UnknownOpcode`]),
//! never a guess: refusing to draw is recoverable, desynchronising is not.

use ad_memory::Memory;

use crate::blit::{self, Surface};
use crate::quickdraw::Rect;

/// Why a picture could not be drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PictError {
    /// Ran off the end of the picture data.
    Truncated { at: u32 },
    /// An opcode whose length this decoder does not know. Refusing here is
    /// deliberate — see the module docs.
    UnknownOpcode { opcode: u16, at: u32 },
}

/// Drawing state carried through a picture.
struct PictState {
    /// Maps picture coordinates onto the destination.
    src_frame: Rect,
    dst_frame: Rect,
    fore: u8,
    back: u8,
    /// Last rectangle, for the "same rect" opcode variants.
    last_rect: Rect,
    pen: (i16, i16),
}

impl PictState {
    /// Map a picture-space point into destination space.
    fn map_pt(&self, h: i16, v: i16) -> (i32, i32) {
        let (sw, sh) = (
            self.src_frame.width().max(1),
            self.src_frame.height().max(1),
        );
        let (dw, dh) = (self.dst_frame.width(), self.dst_frame.height());
        let x = i32::from(self.dst_frame.left)
            + (i32::from(h) - i32::from(self.src_frame.left)) * dw / sw;
        let y = i32::from(self.dst_frame.top)
            + (i32::from(v) - i32::from(self.src_frame.top)) * dh / sh;
        (x, y)
    }

    /// Map a picture-space rect into destination space.
    fn map_rect(&self, r: &Rect) -> Rect {
        let (l, t) = self.map_pt(r.left, r.top);
        let (rr, b) = self.map_pt(r.right, r.bottom);
        Rect::new(
            i16::try_from(t).unwrap_or(i16::MAX),
            i16::try_from(l).unwrap_or(i16::MAX),
            i16::try_from(b).unwrap_or(i16::MAX),
            i16::try_from(rr).unwrap_or(i16::MAX),
        )
    }
}

/// A cursor over picture bytes held in emulated memory.
struct Reader<'a> {
    mem: &'a mut Memory,
    at: u32,
    end: u32,
}

impl Reader<'_> {
    fn u8(&mut self) -> Result<u8, PictError> {
        if self.at >= self.end {
            return Err(PictError::Truncated { at: self.at });
        }
        let v = self.mem.read_u8(self.at);
        self.at = self.at.wrapping_add(1);
        Ok(v)
    }
    fn u16(&mut self) -> Result<u16, PictError> {
        Ok((u16::from(self.u8()?) << 8) | u16::from(self.u8()?))
    }
    fn i16(&mut self) -> Result<i16, PictError> {
        Ok(self.u16()? as i16)
    }
    fn u32(&mut self) -> Result<u32, PictError> {
        Ok((u32::from(self.u16()?) << 16) | u32::from(self.u16()?))
    }
    fn rect(&mut self) -> Result<Rect, PictError> {
        Ok(Rect::new(
            self.i16()?,
            self.i16()?,
            self.i16()?,
            self.i16()?,
        ))
    }
    fn skip(&mut self, n: u32) -> Result<(), PictError> {
        self.at = self.at.wrapping_add(n);
        if self.at > self.end {
            return Err(PictError::Truncated { at: self.at });
        }
        Ok(())
    }
    /// Opcodes are word-aligned in version 2.
    fn align(&mut self) {
        if self.at % 2 == 1 {
            self.at = self.at.wrapping_add(1);
        }
    }
}

/// Fixed operand sizes for picture opcodes.
///
/// `None` means variable-length and handled explicitly. Anything absent from
/// both is an error rather than a guess.
///
/// The table serves both versions because the low-range version-2 sizes were
/// designed to match version 1 — with one exception: the Version opcode `$11`
/// carries **one** data byte in a v1 picture but two (`$02FF`) in v2. Skipping
/// two in a v1 stream lands the parser one byte off, and from there it reads
/// pixel data as opcodes — Lunatic Fringe's title card died exactly that way.
fn fixed_size(opcode: u16, v2: bool) -> Option<u32> {
    if opcode == 0x0011 && !v2 {
        return Some(1);
    }
    Some(match opcode {
        0x0000 => 0,
        0x0002 | 0x0009 | 0x000A => 8, // BkPat, PnPat, FillPat
        0x0003 | 0x0005 | 0x000D => 2, // TxFont, TxMode, TxSize
        0x0004 => 1,                   // TxFace
        0x0006 | 0x0007 | 0x000B | 0x000C => 4, // SpExtra, PnSize, OvSize, Origin
        0x0008 => 2,                   // PnMode
        0x000E | 0x000F => 4,          // FgColor, BkColor (old-style)
        0x0010 => 8,                   // TxRatio
        0x0011 => 2,                   // VersionOp payload
        0x0015 | 0x0016 => 2,          // PnLocHFrac, ChExtra
        0x001A | 0x001B | 0x001D | 0x001F => 6, // RGBFgCol, RGBBkCol, HiliteColor, OpColor
        0x001C | 0x001E => 0,          // HiliteMode, DefHilite
        0x0020 => 8,                   // Line
        0x0021 => 4,                   // LineFrom
        0x0022 => 6,                   // ShortLine
        0x0023 => 2,                   // ShortLineFrom
        0x0030..=0x0034 => 8,          // frame/paint/erase/invert/fillRect
        0x0038..=0x003C => 0,          // same-rect variants
        0x0040..=0x0044 => 8,          // roundRects
        0x0048..=0x004C => 0,
        0x0050..=0x0054 => 8, // ovals
        0x0058..=0x005C => 0,
        0x0060..=0x0064 => 12, // arcs
        0x0068..=0x006C => 4,
        0x0078..=0x007C => 0, // same-poly
        0x0088..=0x008C => 0, // same-region
        0x00A0 => 2,          // ShortComment
        0x0C00 => 24,         // HeaderOp
        0x00FF => 0,          // OpEndPic
        _ => return None,
    })
}

/// Decompress one PackBits run into `out`.
fn unpack_bits(r: &mut Reader<'_>, byte_count: u32, out: &mut Vec<u8>) -> Result<(), PictError> {
    let stop = r.at.wrapping_add(byte_count);
    while r.at < stop {
        let flag = r.u8()? as i8;
        if flag >= 0 {
            // Literal run of flag+1 bytes.
            for _ in 0..=flag {
                let b = r.u8()?;
                out.push(b);
            }
        } else {
            // Repeat the next byte 1-flag times.
            let n = 1 - i32::from(flag);
            let b = r.u8()?;
            for _ in 0..n {
                out.push(b);
            }
        }
    }
    Ok(())
}

/// Read an inline `PixMap`/`BitMap` header, returning its shape.
struct InlineMap {
    row_bytes: u32,
    bounds: Rect,
    pixel_size: u16,
    is_pixmap: bool,
    /// Colour table read from the picture, as RGB triples by index.
    palette: Vec<[u8; 3]>,
}

fn read_inline_map(r: &mut Reader<'_>, with_table: bool) -> Result<InlineMap, PictError> {
    let row_word = r.u16()?;
    let is_pixmap = row_word & 0x8000 != 0;
    let row_bytes = u32::from(row_word & 0x3FFF);
    let bounds = r.rect()?;
    let mut pixel_size = 1u16;
    let mut palette = Vec::new();
    if is_pixmap {
        r.skip(2)?; // pmVersion
        r.skip(2)?; // packType
        r.skip(4)?; // packSize
        r.skip(8)?; // hRes, vRes
        r.skip(2)?; // pixelType
        pixel_size = r.u16()?;
        r.skip(4)?; // cmpCount, cmpSize
        r.skip(4)?; // planeBytes
        r.skip(4)?; // pmTable
        r.skip(4)?; // pmReserved
        if with_table {
            let _seed = r.u32()?;
            let flags = r.u16()?;
            let ct_size = r.u16()?;
            let entries = u32::from(ct_size).saturating_add(1).min(256);
            palette = vec![[0u8; 3]; 256];
            // ctFlags bit 15 means "these entries are in index order and the
            // `value` field is meaningless". PixMap colour tables written into
            // pictures set it, and their value fields are commonly all zero — so
            // honouring `value` piles every entry onto index 0, last one wins,
            // and a 16-colour sprite decodes as one flat colour. That is exactly
            // how Flying Toasters rendered as solid black rectangles.
            let indexed_by_position = flags & 0x8000 != 0;
            for i in 0..entries {
                let value = r.u16()?;
                let rgb = [
                    (r.u16()? >> 8) as u8,
                    (r.u16()? >> 8) as u8,
                    (r.u16()? >> 8) as u8,
                ];
                let idx = if indexed_by_position {
                    i as usize
                } else {
                    usize::from(value)
                };
                if let Some(slot) = palette.get_mut(idx.min(255)) {
                    *slot = rgb;
                }
            }
        }
    }
    Ok(InlineMap {
        row_bytes,
        bounds,
        pixel_size,
        is_pixmap,
        palette,
    })
}

/// Draw a `PICT` into `dst`.
///
/// `pic` addresses the picture in emulated memory (a `Picture` record: size word,
/// `picFrame`, then opcodes). `dst_rect` is where it goes, in `dst`'s
/// coordinates; the picture is scaled from its own frame.
///
/// `scratch` is a spare region of emulated memory at least as large as one
/// decoded bitmap, used to stage pixel data so the existing blitter can do the
/// depth conversion and transfer modes.
#[allow(
    clippy::too_many_arguments,
    reason = "carries the picture, destination, colours and staging area explicitly"
)]
pub fn draw_picture(
    mem: &mut Memory,
    pic: u32,
    pic_len: u32,
    dst: &Surface,
    dst_rect: &Rect,
    fore: u8,
    back: u8,
    scratch: u32,
    scratch_len: u32,
    to_dest_index: &dyn Fn(&mut Memory, [u8; 3]) -> u8,
) -> Result<(), PictError> {
    let end = pic.wrapping_add(pic_len.max(11));
    let mut r = Reader { mem, at: pic, end };
    r.skip(2)?; // picSize, unreliable and unused
    let frame = r.rect()?;

    let mut st = PictState {
        src_frame: frame,
        dst_frame: *dst_rect,
        fore,
        back,
        last_rect: Rect::default(),
        pen: (0, 0),
    };

    // Version detection: $0011 $02FF introduces a version-2 picture.
    let mut v2 = false;
    if r.at.wrapping_add(4) <= end && r.mem.read_u16(r.at) == 0x0011 {
        let ver = r.mem.read_u16(r.at.wrapping_add(2));
        if ver == 0x02FF {
            v2 = true;
            r.skip(4)?;
        }
    }

    while r.at < end {
        if v2 {
            r.align();
            if r.at >= end {
                break;
            }
        }
        let opcode = if v2 { r.u16()? } else { u16::from(r.u8()?) };
        if opcode == 0x00FF {
            break; // OpEndPic
        }

        match opcode {
            // ---- state we honour ----
            0x001A => {
                // RGBFgCol
                let rgb = [
                    (r.u16()? >> 8) as u8,
                    (r.u16()? >> 8) as u8,
                    (r.u16()? >> 8) as u8,
                ];
                st.fore = to_dest_index(r.mem, rgb);
            }
            0x001B => {
                // RGBBkCol
                let rgb = [
                    (r.u16()? >> 8) as u8,
                    (r.u16()? >> 8) as u8,
                    (r.u16()? >> 8) as u8,
                ];
                st.back = to_dest_index(r.mem, rgb);
            }

            // ---- rectangles ----
            0x0030..=0x0034 | 0x0038..=0x003C => {
                let rect = if opcode <= 0x0034 {
                    let rr = r.rect()?;
                    st.last_rect = rr;
                    rr
                } else {
                    st.last_rect
                };
                let d = st.map_rect(&rect);
                // The "same rect" variants live at 0x38..0x3C, so the verb is in
                // the low three bits of both ranges: 0x3A & 7 == 2 == erase.
                let verb = opcode & 0x07;
                let colour = if verb == 0x02 { st.back } else { st.fore };
                fill_or_frame(r.mem, dst, &d, colour, verb == 0x00);
            }

            // ---- ovals: filled as their bounding box for now ----
            0x0050..=0x0054 | 0x0058..=0x005C => {
                let rect = if opcode <= 0x0054 {
                    let rr = r.rect()?;
                    st.last_rect = rr;
                    rr
                } else {
                    st.last_rect
                };
                let d = st.map_rect(&rect);
                let verb = opcode & 0x07;
                let colour = if verb == 0x02 { st.back } else { st.fore };
                fill_oval(r.mem, dst, &d, colour);
            }

            // ---- lines ----
            0x0020 => {
                let (h1, v1) = (r.i16()?, r.i16()?);
                let (h2, v2p) = (r.i16()?, r.i16()?);
                let a = st.map_pt(h1, v1);
                let b = st.map_pt(h2, v2p);
                line(r.mem, dst, a, b, st.fore);
                st.pen = (h2, v2p);
            }
            0x0021 => {
                let (h, v) = (r.i16()?, r.i16()?);
                let a = st.map_pt(st.pen.0, st.pen.1);
                let b = st.map_pt(h, v);
                line(r.mem, dst, a, b, st.fore);
                st.pen = (h, v);
            }
            0x0022 => {
                let (h1, v1) = (r.i16()?, r.i16()?);
                let (dh, dv) = (r.u8()? as i8, r.u8()? as i8);
                let (h2, v2p) = (
                    h1.saturating_add(i16::from(dh)),
                    v1.saturating_add(i16::from(dv)),
                );
                let a = st.map_pt(h1, v1);
                let b = st.map_pt(h2, v2p);
                line(r.mem, dst, a, b, st.fore);
                st.pen = (h2, v2p);
            }

            // ---- the important ones: embedded bitmaps ----
            0x0090 | 0x0098 | 0x0091 | 0x0099 => {
                let packed = opcode == 0x0098 || opcode == 0x0099;
                let with_rgn = opcode == 0x0091 || opcode == 0x0099;
                let map = read_inline_map(&mut r, true)?;
                let src_rect = r.rect()?;
                let mut dst_r = r.rect()?;
                let mode = r.i16()?;
                if with_rgn {
                    // A clipping region follows: a size word covering the whole
                    // structure, including itself.
                    let size = u32::from(r.u16()?);
                    r.skip(size.saturating_sub(2))?;
                }

                // Decode the rows into a contiguous buffer.
                let rows = map.bounds.height().max(0) as u32;
                let need = map.row_bytes.saturating_mul(rows);
                // Refuse rather than overrun — or, worse, write through a nil
                // staging pointer straight onto the exception vectors.
                if need == 0 || need > scratch_len || scratch == 0 {
                    return Ok(());
                }
                let mut pixels: Vec<u8> = Vec::with_capacity(need as usize);
                if packed && map.row_bytes >= 8 {
                    for _ in 0..rows {
                        let count = if map.row_bytes > 250 {
                            u32::from(r.u16()?)
                        } else {
                            u32::from(r.u8()?)
                        };
                        let before = pixels.len();
                        unpack_bits(&mut r, count, &mut pixels)?;
                        // Rows must land exactly on the stride.
                        pixels.resize(before + map.row_bytes as usize, 0);
                    }
                } else {
                    for _ in 0..need {
                        let b = r.u8()?;
                        pixels.push(b);
                    }
                }
                r.mem.write_bytes(scratch, &pixels);

                // Present the staged pixels as a Surface and reuse the blitter.
                let src = Surface {
                    base: scratch,
                    row_bytes: map.row_bytes,
                    bounds: map.bounds,
                    pixel_size: if map.is_pixmap { map.pixel_size } else { 1 },
                    color_table: 0,
                };
                // Picture-space destination maps through the picture's frame.
                dst_r = st.map_rect(&dst_r);
                let pal = map.palette.clone();
                let mapper = |m: &mut Memory, rgb: [u8; 3]| to_dest_index(m, rgb);
                if pal.is_empty() {
                    blit::copy_bits(
                        r.mem, &src, dst, &src_rect, &dst_r, mode, st.fore, st.back, None, &mapper,
                    );
                } else {
                    // Resolve indices through the picture's own colour table.
                    let indexed = move |m: &mut Memory, rgb: [u8; 3]| to_dest_index(m, rgb);
                    let table: Vec<u8> = pal.iter().map(|rgb| to_dest_index(r.mem, *rgb)).collect();
                    blit_indexed(r.mem, &src, dst, &src_rect, &dst_r, mode, &table, st.back);
                    let _ = indexed;
                }
            }

            // ---- variable-length skips ----
            0x0001 | 0x0080..=0x0084 => {
                // Clip / region ops: a leading size word covers the structure.
                let size = u32::from(r.u16()?);
                r.skip(size.saturating_sub(2))?;
            }
            0x0070..=0x0074 => {
                // Polygons: same self-describing size word.
                let size = u32::from(r.u16()?);
                r.skip(size.saturating_sub(2))?;
            }
            0x0012..=0x0014 => {
                // Pixel patterns: type word, then a pattern and possibly a PixMap.
                let pat_type = r.u16()?;
                r.skip(8)?; // the 1-bit pattern always present
                if pat_type == 1 {
                    let m = read_inline_map(&mut r, true)?;
                    let rows = m.bounds.height().max(0) as u32;
                    r.skip(m.row_bytes.saturating_mul(rows))?;
                }
            }
            0x00A1 => {
                // LongComment: kind word, then a size word and that many bytes.
                r.skip(2)?;
                let size = u32::from(r.u16()?);
                r.skip(size)?;
            }
            0x0028 => {
                // LongText: point, then a Pascal string.
                r.skip(4)?;
                let n = u32::from(r.u8()?);
                r.skip(n)?;
            }
            0x0029 | 0x002A => {
                r.skip(1)?;
                let n = u32::from(r.u8()?);
                r.skip(n)?;
            }
            0x002B => {
                r.skip(2)?;
                let n = u32::from(r.u8()?);
                r.skip(n)?;
            }
            0x009A | 0x009B => {
                // DirectBitsRect/Rgn: 16- and 32-bit direct colour. Structure is
                // read so it can be skipped exactly; the pixels are not drawn yet.
                r.skip(4)?; // baseAddr placeholder
                let m = read_inline_map(&mut r, false)?;
                r.skip(8)?; // srcRect
                r.skip(8)?; // dstRect
                r.skip(2)?; // mode
                if opcode == 0x009B {
                    let size = u32::from(r.u16()?);
                    r.skip(size.saturating_sub(2))?;
                }
                let rows = m.bounds.height().max(0) as u32;
                if m.row_bytes >= 8 {
                    for _ in 0..rows {
                        let count = if m.row_bytes > 250 {
                            u32::from(r.u16()?)
                        } else {
                            u32::from(r.u8()?)
                        };
                        r.skip(count)?;
                    }
                } else {
                    r.skip(m.row_bytes.saturating_mul(rows))?;
                }
            }

            _ => match fixed_size(opcode, v2) {
                Some(n) => r.skip(n)?,
                None => {
                    // Reserved ranges have documented lengths; anything else must
                    // stop the parse rather than desynchronise it.
                    return Err(PictError::UnknownOpcode { opcode, at: r.at });
                }
            },
        }
    }
    Ok(())
}

/// Blit with source indices resolved through a picture's own colour table.
#[allow(clippy::too_many_arguments, reason = "same shape as blit::copy_bits")]
fn blit_indexed(
    mem: &mut Memory,
    src: &Surface,
    dst: &Surface,
    src_rect: &Rect,
    dst_rect: &Rect,
    mode: i16,
    table: &[u8],
    back: u8,
) {
    let (sw, sh) = (src_rect.width().max(1), src_rect.height().max(1));
    let (dw, dh) = (dst_rect.width(), dst_rect.height());
    if dw <= 0 || dh <= 0 {
        return;
    }
    let transparent = mode & 0x07 == blit::mode::SRC_OR;
    for dy in 0..dh {
        let sy = i32::from(src_rect.top) + dy * sh / dh;
        for dx in 0..dw {
            let sx = i32::from(src_rect.left) + dx * sw / dw;
            let Some(raw) = src.get(mem, sx, sy) else {
                continue;
            };
            let value = if src.pixel_size == 1 {
                // In a 1-bit picture a set bit is ink.
                if raw != 0 {
                    table.first().copied().unwrap_or(255)
                } else {
                    back
                }
            } else {
                table.get(usize::from(raw)).copied().unwrap_or(raw)
            };
            if transparent && value == back {
                continue;
            }
            dst.set(
                mem,
                i32::from(dst_rect.left) + dx,
                i32::from(dst_rect.top) + dy,
                value,
            );
        }
    }
}

fn fill_or_frame(mem: &mut Memory, dst: &Surface, r: &Rect, colour: u8, frame_only: bool) {
    if r.is_empty() {
        return;
    }
    if frame_only {
        for x in i32::from(r.left)..i32::from(r.right) {
            dst.set(mem, x, i32::from(r.top), colour);
            dst.set(mem, x, i32::from(r.bottom) - 1, colour);
        }
        for y in i32::from(r.top)..i32::from(r.bottom) {
            dst.set(mem, i32::from(r.left), y, colour);
            dst.set(mem, i32::from(r.right) - 1, y, colour);
        }
    } else {
        for y in i32::from(r.top)..i32::from(r.bottom) {
            for x in i32::from(r.left)..i32::from(r.right) {
                dst.set(mem, x, y, colour);
            }
        }
    }
}

fn fill_oval(mem: &mut Memory, dst: &Surface, r: &Rect, colour: u8) {
    let (a, b) = (r.width() / 2, r.height() / 2);
    if a <= 0 || b <= 0 {
        fill_or_frame(mem, dst, r, colour, false);
        return;
    }
    let (cx, cy) = (i32::from(r.left) + a, i32::from(r.top) + b);
    for y in -b..=b {
        let num = (b * b - y * y).max(0);
        let mut x = 0i32;
        while (x + 1) * (x + 1) * b * b <= a * a * num {
            x += 1;
        }
        for px in (cx - x)..=(cx + x) {
            dst.set(mem, px, cy + y, colour);
        }
    }
}

fn line(mem: &mut Memory, dst: &Surface, a: (i32, i32), b: (i32, i32), colour: u8) {
    let (mut x, mut y) = a;
    let dx = (b.0 - x).abs();
    let dy = -(b.1 - y).abs();
    let sx = if x < b.0 { 1 } else { -1 };
    let sy = if y < b.1 { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        dst.set(mem, x, y, colour);
        if x == b.0 && y == b.1 {
            break;
        }
        let e2 = err * 2;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dest(m: &mut Memory, w: i16, h: i16) -> (Surface, u32) {
        let base = 0x0040_0000u32;
        let hdr = 0x0040_8000u32;
        m.write_u32(hdr, base);
        m.write_u16(hdr + 4, 0x8000 | (w as u16));
        Rect::new(0, 0, h, w).write(m, hdr + 6);
        m.write_u16(hdr + 32, 8);
        (Surface::resolve(m, hdr).expect("dst"), base)
    }

    /// Assemble a version-2 picture with a frame and the given opcode bytes.
    fn pict_v2(m: &mut Memory, at: u32, frame: Rect, body: &[u8]) -> u32 {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(&[0, 0]); // picSize
        for v in [frame.top, frame.left, frame.bottom, frame.right] {
            bytes.extend_from_slice(&(v as u16).to_be_bytes());
        }
        bytes.extend_from_slice(&[0x00, 0x11, 0x02, 0xFF]); // VersionOp v2
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(&[0x00, 0xFF]); // OpEndPic
        m.write_bytes(at, &bytes);
        bytes.len() as u32
    }

    fn ident(_: &mut Memory, rgb: [u8; 3]) -> u8 {
        // Stand-in palette mapper: use the red channel as the index.
        rgb[0]
    }

    #[test]
    fn draws_a_filled_rect_and_maps_coordinates() {
        let mut m = Memory::new();
        let (dst, _) = dest(&mut m, 32, 32);
        // paintRect (0x0031) over the left half of a 16x16 frame.
        let mut body = vec![0x00, 0x31];
        for v in [0i16, 0, 16, 8] {
            body.extend_from_slice(&(v as u16).to_be_bytes());
        }
        let at = 0x0041_0000;
        let len = pict_v2(&mut m, at, Rect::new(0, 0, 16, 16), &body);

        draw_picture(
            &mut m,
            at,
            len,
            &dst,
            &Rect::new(0, 0, 32, 32),
            200,
            0,
            0x0042_0000,
            0x1000,
            &ident,
        )
        .expect("draw");
        // The frame is 16 wide and the destination 32, so a 2x magnification.
        assert_eq!(dst.get(&mut m, 0, 0), Some(200));
        assert_eq!(dst.get(&mut m, 15, 31), Some(200), "left half scaled up");
        assert_eq!(dst.get(&mut m, 16, 0), Some(0), "right half untouched");
    }

    #[test]
    fn rgb_foreground_changes_the_colour_used() {
        let mut m = Memory::new();
        let (dst, _) = dest(&mut m, 16, 16);
        let mut body = vec![0x00, 0x1A]; // RGBFgCol
        body.extend_from_slice(&[0x77, 0x00, 0x00, 0x00, 0x00, 0x00]); // red = 0x77
        body.extend_from_slice(&[0x00, 0x31]); // paintRect
        for v in [0i16, 0, 16, 16] {
            body.extend_from_slice(&(v as u16).to_be_bytes());
        }
        let at = 0x0043_0000;
        let len = pict_v2(&mut m, at, Rect::new(0, 0, 16, 16), &body);
        draw_picture(
            &mut m,
            at,
            len,
            &dst,
            &Rect::new(0, 0, 16, 16),
            1,
            0,
            0x0044_0000,
            0x1000,
            &ident,
        )
        .expect("draw");
        assert_eq!(
            dst.get(&mut m, 5, 5),
            Some(0x77),
            "RGBFgCol should replace the default foreground"
        );
    }

    #[test]
    fn same_rect_variants_reuse_the_previous_rectangle() {
        let mut m = Memory::new();
        let (dst, _) = dest(&mut m, 16, 16);
        let mut body = vec![0x00, 0x31]; // paintRect with a rect
        for v in [0i16, 0, 4, 4] {
            body.extend_from_slice(&(v as u16).to_be_bytes());
        }
        body.extend_from_slice(&[0x00, 0x3A]); // eraseRect, same rect, no operand
        let at = 0x0045_0000;
        let len = pict_v2(&mut m, at, Rect::new(0, 0, 16, 16), &body);
        draw_picture(
            &mut m,
            at,
            len,
            &dst,
            &Rect::new(0, 0, 16, 16),
            200,
            9,
            0x0046_0000,
            0x1000,
            &ident,
        )
        .expect("draw");
        // Painted then erased over the same area: background wins.
        assert_eq!(dst.get(&mut m, 1, 1), Some(9));
    }

    #[test]
    fn unknown_opcode_stops_rather_than_desynchronising() {
        // Guessing a length would make the parser read operands as opcodes and
        // paint noise for the rest of the picture.
        let mut m = Memory::new();
        let (dst, _) = dest(&mut m, 16, 16);
        let body = vec![0x00, 0x77, 0xDE, 0xAD]; // 0x0077 has no defined length
        let at = 0x0047_0000;
        let len = pict_v2(&mut m, at, Rect::new(0, 0, 16, 16), &body);
        let err = draw_picture(
            &mut m,
            at,
            len,
            &dst,
            &Rect::new(0, 0, 16, 16),
            1,
            0,
            0x0048_0000,
            0x1000,
            &ident,
        )
        .expect_err("must refuse");
        assert!(matches!(
            err,
            PictError::UnknownOpcode { opcode: 0x0077, .. }
        ));
    }

    #[test]
    fn packbits_expands_literal_and_repeat_runs() {
        let mut m = Memory::new();
        // flag 2 -> 3 literals; flag -3 (0xFD) -> 4 copies of the next byte.
        let data = [0x02u8, 0xAA, 0xBB, 0xCC, 0xFD, 0x11];
        m.write_bytes(0x0049_0000, &data);
        let mut r = Reader {
            mem: &mut m,
            at: 0x0049_0000,
            end: 0x0049_0000 + data.len() as u32,
        };
        let mut out = Vec::new();
        unpack_bits(&mut r, data.len() as u32, &mut out).expect("unpack");
        assert_eq!(out, vec![0xAA, 0xBB, 0xCC, 0x11, 0x11, 0x11, 0x11]);
    }

    #[test]
    fn draws_an_embedded_one_bit_bitmap() {
        let mut m = Memory::new();
        let (dst, _) = dest(&mut m, 16, 16);
        // BitsRect (0x0090) with a 8x2 one-bit BitMap, uncompressed
        // (rowBytes < 8 means no PackBits).
        let mut body = vec![0x00, 0x90];
        body.extend_from_slice(&2u16.to_be_bytes()); // rowBytes = 2, no high bit
        for v in [0i16, 0, 2, 8] {
            body.extend_from_slice(&(v as u16).to_be_bytes()); // bounds
        }
        for v in [0i16, 0, 2, 8] {
            body.extend_from_slice(&(v as u16).to_be_bytes()); // srcRect
        }
        for v in [0i16, 0, 2, 8] {
            body.extend_from_slice(&(v as u16).to_be_bytes()); // dstRect
        }
        body.extend_from_slice(&0u16.to_be_bytes()); // srcCopy
        body.extend_from_slice(&[0b1010_1010, 0x00, 0b0101_0101, 0x00]); // 2 rows

        let at = 0x004A_0000;
        let len = pict_v2(&mut m, at, Rect::new(0, 0, 2, 8), &body);
        draw_picture(
            &mut m,
            at,
            len,
            &dst,
            &Rect::new(0, 0, 2, 8),
            200,
            7,
            0x004B_0000,
            0x1000,
            &ident,
        )
        .expect("draw");
        assert_eq!(dst.get(&mut m, 0, 0), Some(200), "set bit -> ink");
        assert_eq!(dst.get(&mut m, 1, 0), Some(7), "clear bit -> background");
        assert_eq!(dst.get(&mut m, 0, 1), Some(7), "row 2 starts clear");
        assert_eq!(dst.get(&mut m, 1, 1), Some(200));
    }

    #[test]
    fn refuses_a_bitmap_larger_than_the_staging_area() {
        // Overrunning the scratch region would corrupt whatever follows it.
        let mut m = Memory::new();
        let (dst, _) = dest(&mut m, 16, 16);
        let mut body = vec![0x00, 0x90];
        body.extend_from_slice(&4u16.to_be_bytes()); // rowBytes 4
        for v in [0i16, 0, 4000, 16] {
            body.extend_from_slice(&(v as u16).to_be_bytes()); // 4000 rows
        }
        for _ in 0..2 {
            for v in [0i16, 0, 4, 4] {
                body.extend_from_slice(&(v as u16).to_be_bytes());
            }
        }
        body.extend_from_slice(&0u16.to_be_bytes());
        let at = 0x004C_0000;
        let len = pict_v2(&mut m, at, Rect::new(0, 0, 16, 16), &body);
        // 4 * 4000 = 16000 bytes needed, scratch is 256.
        draw_picture(
            &mut m,
            at,
            len,
            &dst,
            &Rect::new(0, 0, 16, 16),
            1,
            0,
            0x004D_0000,
            256,
            &ident,
        )
        .expect("should skip, not overrun");
        assert_eq!(dst.get(&mut m, 0, 0), Some(0), "nothing drawn");
    }

    #[test]
    fn pixmap_colour_tables_are_indexed_by_position_when_ctflags_says_so() {
        // Real PixMap cluts in pictures set ctFlags bit 15 and leave every
        // `value` field at zero. Honouring `value` collapses the whole table onto
        // index 0 and a 16-colour sprite decodes as one flat colour — which is
        // precisely how Flying Toasters rendered as solid black rectangles.
        let mut m = Memory::new();
        let at = 0x0050_0000u32;
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(&(0x8000u16 | 4).to_be_bytes()); // PixMap, rowBytes 4
        for v in [0i16, 0, 2, 8] {
            bytes.extend_from_slice(&(v as u16).to_be_bytes()); // bounds
        }
        // 36 bytes of PixMap fields, with pixelSize = 4 at its proper offset.
        // Field block after rowBytes+bounds: pmVersion(2) packType(2) packSize(4)
        // hRes(4) vRes(4) pixelType(2) pixelSize(2) ... so pixelSize is at 18.
        let mut fields = vec![0u8; 36];
        fields[18] = 0;
        fields[19] = 4; // pixelSize = 4
        bytes.extend_from_slice(&fields);
        // ColorTable: ctFlags bit 15 set, 4 entries, all with value = 0.
        bytes.extend_from_slice(&0u32.to_be_bytes()); // ctSeed
        bytes.extend_from_slice(&0x8000u16.to_be_bytes()); // ctFlags
        bytes.extend_from_slice(&3u16.to_be_bytes()); // ctSize = entries - 1
        for shade in [0xFFu16, 0xAA, 0x55, 0x00] {
            bytes.extend_from_slice(&0u16.to_be_bytes()); // value: meaningless
            for _ in 0..3 {
                bytes.extend_from_slice(&((shade << 8) | shade).to_be_bytes());
            }
        }
        m.write_bytes(at, &bytes);

        let mut r = Reader {
            mem: &mut m,
            at,
            end: at + bytes.len() as u32,
        };
        let map = read_inline_map(&mut r, true).expect("parse");
        assert_eq!(map.pixel_size, 4);
        assert_eq!(map.palette[0], [0xFF, 0xFF, 0xFF], "entry 0 by position");
        assert_eq!(map.palette[1], [0xAA, 0xAA, 0xAA], "entry 1 by position");
        assert_eq!(map.palette[2], [0x55, 0x55, 0x55]);
        assert_eq!(map.palette[3], [0x00, 0x00, 0x00]);
    }

    #[test]
    fn colour_tables_honour_the_value_field_when_ctflags_is_clear() {
        let mut m = Memory::new();
        let at = 0x0051_0000u32;
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(&(0x8000u16 | 4).to_be_bytes());
        for v in [0i16, 0, 2, 8] {
            bytes.extend_from_slice(&(v as u16).to_be_bytes());
        }
        bytes.extend_from_slice(&[0u8; 36]);
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes()); // ctFlags clear
        bytes.extend_from_slice(&1u16.to_be_bytes()); // 2 entries
        // Entry claims index 7, then index 3.
        for (value, shade) in [(7u16, 0xFFu16), (3, 0x11)] {
            bytes.extend_from_slice(&value.to_be_bytes());
            for _ in 0..3 {
                bytes.extend_from_slice(&((shade << 8) | shade).to_be_bytes());
            }
        }
        m.write_bytes(at, &bytes);
        let mut r = Reader {
            mem: &mut m,
            at,
            end: at + bytes.len() as u32,
        };
        let map = read_inline_map(&mut r, true).expect("parse");
        assert_eq!(map.palette[7], [0xFF, 0xFF, 0xFF], "placed by value");
        assert_eq!(map.palette[3], [0x11, 0x11, 0x11]);
        assert_eq!(map.palette[0], [0, 0, 0], "index 0 untouched");
    }

    #[test]
    fn a_version_one_version_opcode_carries_one_data_byte() {
        // Lunatic Fringe's title card opens `11 01 A0 00 82 01 ...` — Version,
        // ShortComment, Clip. Skipping two bytes after $11 (the v2 size) reads
        // the comment kind as an opcode and desynchronises the whole stream.
        let mut m = Memory::new();
        let (dst, _) = dest(&mut m, 16, 16);
        let mut bytes: Vec<u8> = vec![0, 0];
        for v in [0i16, 0, 16, 16] {
            bytes.extend_from_slice(&(v as u16).to_be_bytes());
        }
        bytes.extend_from_slice(&[0x11, 0x01]); // Version 1: ONE data byte
        bytes.extend_from_slice(&[0xA0, 0x00, 0x82]); // ShortComment
        bytes.extend_from_slice(&[0x01, 0x00, 0x0A, 0, 0, 0, 0, 0, 16, 0, 16]); // Clip
        bytes.push(0x31); // paintRect
        for v in [0i16, 0, 8, 8] {
            bytes.extend_from_slice(&(v as u16).to_be_bytes());
        }
        bytes.push(0xFF);
        let at = 0x0052_0000;
        m.write_bytes(at, &bytes);
        draw_picture(
            &mut m,
            at,
            bytes.len() as u32,
            &dst,
            &Rect::new(0, 0, 16, 16),
            77,
            0,
            0x0053_0000,
            0x1000,
            &ident,
        )
        .expect("a v1 header must not desynchronise the parse");
        assert_eq!(dst.get(&mut m, 2, 2), Some(77));
    }

    #[test]
    fn a_version_one_picture_uses_single_byte_opcodes() {
        let mut m = Memory::new();
        let (dst, _) = dest(&mut m, 16, 16);
        let mut bytes: Vec<u8> = vec![0, 0];
        for v in [0i16, 0, 16, 16] {
            bytes.extend_from_slice(&(v as u16).to_be_bytes());
        }
        // No VersionOp: one-byte opcodes. 0x31 = paintRect.
        bytes.push(0x31);
        for v in [0i16, 0, 8, 8] {
            bytes.extend_from_slice(&(v as u16).to_be_bytes());
        }
        bytes.push(0xFF);
        let at = 0x004E_0000;
        m.write_bytes(at, &bytes);
        draw_picture(
            &mut m,
            at,
            bytes.len() as u32,
            &dst,
            &Rect::new(0, 0, 16, 16),
            123,
            0,
            0x004F_0000,
            0x1000,
            &ident,
        )
        .expect("draw");
        assert_eq!(dst.get(&mut m, 2, 2), Some(123));
        assert_eq!(dst.get(&mut m, 12, 12), Some(0));
    }
}
