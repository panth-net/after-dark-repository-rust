//! QuickDraw.
//!
//! The canonical output is a **CPU framebuffer**, not a GPU surface. QuickDraw is
//! fundamentally a software drawing model, so rendering it in software gives
//! deterministic pixels, exact palette behaviour, and screenshot tests that
//! compare byte for byte. Scaling and any CRT effect happen afterwards, in the
//! host, and never touch these pixels.
//!
//! Geometry follows the classic Mac layout, which matters for marshalling:
//! a `Point` is `(v, h)` — **vertical first** — and a `Rect` is
//! `(top, left, bottom, right)`. Getting that order wrong silently transposes
//! everything.

// The rasterisers (Bresenham, midpoint ellipse) do plain `i32` arithmetic on
// coordinates that are already clamped to a 640x480 screen, so overflow is not
// reachable. Rewriting them with checked arithmetic would obscure well-known
// algorithms without making them safer.
#![allow(clippy::arithmetic_side_effects)]

use ad_m68k::Registers;
use ad_memory::Memory;

use crate::traps::Trap;
use crate::Stack;

/// Screen size. 640×480 matches the Quadra-era default the modules were built
/// against, and is what the oracle captures at.
pub const SCREEN_WIDTH: u16 = 640;
pub const SCREEN_HEIGHT: u16 = 480;

/// A classic Mac `Rect`, stored in memory as four 16-bit words in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub top: i16,
    pub left: i16,
    pub bottom: i16,
    pub right: i16,
}

impl Rect {
    #[must_use]
    pub const fn new(top: i16, left: i16, bottom: i16, right: i16) -> Self {
        Self {
            top,
            left,
            bottom,
            right,
        }
    }

    /// Read a `Rect` from emulated memory.
    #[must_use]
    pub fn read(mem: &mut Memory, addr: u32) -> Self {
        Self {
            top: mem.read_u16(addr) as i16,
            left: mem.read_u16(addr.wrapping_add(2)) as i16,
            bottom: mem.read_u16(addr.wrapping_add(4)) as i16,
            right: mem.read_u16(addr.wrapping_add(6)) as i16,
        }
    }

    /// Write a `Rect` into emulated memory.
    pub fn write(&self, mem: &mut Memory, addr: u32) {
        mem.write_u16(addr, self.top as u16);
        mem.write_u16(addr.wrapping_add(2), self.left as u16);
        mem.write_u16(addr.wrapping_add(4), self.bottom as u16);
        mem.write_u16(addr.wrapping_add(6), self.right as u16);
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.top >= self.bottom || self.left >= self.right
    }

    #[must_use]
    pub fn width(&self) -> i32 {
        i32::from(self.right) - i32::from(self.left)
    }

    #[must_use]
    pub fn height(&self) -> i32 {
        i32::from(self.bottom) - i32::from(self.top)
    }

    /// `_OffsetRect`.
    #[must_use]
    pub fn offset(&self, dh: i16, dv: i16) -> Self {
        Self {
            top: self.top.saturating_add(dv),
            left: self.left.saturating_add(dh),
            bottom: self.bottom.saturating_add(dv),
            right: self.right.saturating_add(dh),
        }
    }

    /// `_InsetRect`. Shrinks by `dh`/`dv` on each side; negative values grow it.
    #[must_use]
    pub fn inset(&self, dh: i16, dv: i16) -> Self {
        Self {
            top: self.top.saturating_add(dv),
            left: self.left.saturating_add(dh),
            bottom: self.bottom.saturating_sub(dv),
            right: self.right.saturating_sub(dh),
        }
    }

    /// `_SectRect`. Returns the intersection, which may be empty.
    #[must_use]
    pub fn intersect(&self, other: &Self) -> Self {
        Self {
            top: self.top.max(other.top),
            left: self.left.max(other.left),
            bottom: self.bottom.min(other.bottom),
            right: self.right.min(other.right),
        }
    }

    /// `_UnionRect`.
    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        Self {
            top: self.top.min(other.top),
            left: self.left.min(other.left),
            bottom: self.bottom.max(other.bottom),
            right: self.right.max(other.right),
        }
    }

    /// `_PtInRect`.
    #[must_use]
    pub fn contains(&self, h: i16, v: i16) -> bool {
        h >= self.left && h < self.right && v >= self.top && v < self.bottom
    }
}

/// An 8-bit indexed framebuffer plus its palette.
///
/// Indexed rather than RGBA because that is what the original hardware and the
/// `clut` resources describe; converting to RGB is a presentation step.
///
/// # The screen lives in emulated memory
///
/// `pixels` is a **cache**, refreshed by [`crate::Toolbox::sync_screen`]. The
/// authoritative pixels are inside the emulated address space at
/// [`QuickDraw::screen_base`], because many modules bypass QuickDraw entirely
/// and write straight to the screen bitmap for speed — Hard Rain draws every
/// raindrop that way and issues no drawing traps at all. A framebuffer that only
/// QuickDraw could reach would stay blank for those modules.
#[derive(Debug, Clone)]
pub struct Framebuffer {
    pub width: u16,
    pub height: u16,
    /// One byte per pixel, row-major, `width * height` long.
    pub pixels: Vec<u8>,
    /// 256 RGB triples.
    pub palette: Vec<[u8; 3]>,
}

impl Framebuffer {
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; usize::from(width) * usize::from(height)],
            palette: default_palette(),
        }
    }

    fn index(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x >= i32::from(self.width) || y >= i32::from(self.height) {
            return None;
        }
        usize::try_from(y)
            .ok()?
            .checked_mul(usize::from(self.width))?
            .checked_add(usize::try_from(x).ok()?)
    }

    /// Read a pixel from the cache.
    #[must_use]
    pub fn get(&self, x: i32, y: i32) -> u8 {
        self.index(x, y)
            .and_then(|i| self.pixels.get(i).copied())
            .unwrap_or(0)
    }

    /// Fill the whole surface with one index.
    pub fn clear(&mut self, mem: &mut Memory, base: u32, colour: u8) {
        for i in 0..self.pixels.len() {
            mem.write_u8(base.wrapping_add(u32::try_from(i).unwrap_or(0)), colour);
        }
        self.pixels.fill(colour);
    }

    /// Refresh the cache from emulated screen memory.
    /// Refresh the cache from the emulated screen bitmap.
    ///
    /// One block copy, not 307,200 dispatches. This runs after every module call
    /// — modules write screen memory directly, so nothing else keeps the cache
    /// current — and an idle module gets called a couple of thousand times a
    /// second. Byte-at-a-time, this single loop was the whole frame budget; see
    /// [`Memory::copy_out`].
    pub fn sync_from(&mut self, mem: &mut Memory, base: u32) {
        mem.copy_out(base, &mut self.pixels);
    }

    /// Convert to RGB for presentation or PNG output.
    #[must_use]
    pub fn to_rgb(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixels.len().saturating_mul(3));
        for &i in &self.pixels {
            let c = self
                .palette
                .get(usize::from(i))
                .copied()
                .unwrap_or([0, 0, 0]);
            out.extend_from_slice(&c);
        }
        out
    }

    /// Count of non-zero pixels.
    ///
    /// Deliberately *not* used as the "did it draw?" signal: a screen blanked to
    /// black is index 255 everywhere, which scores 100% here while being visually
    /// empty. Use [`Self::ink`] instead.
    #[must_use]
    pub fn non_zero(&self) -> usize {
        self.pixels.iter().filter(|&&p| p != 0).count()
    }

    /// Distinct colour indices present.
    #[must_use]
    pub fn distinct(&self) -> usize {
        let mut seen = [false; 256];
        for &p in &self.pixels {
            if let Some(s) = seen.get_mut(usize::from(p)) {
                *s = true;
            }
        }
        seen.iter().filter(|s| **s).count()
    }

    /// Pixels that differ from the most common colour — i.e. actual drawn content
    /// rather than whatever the screen was blanked to.
    ///
    /// A uniform screen scores 0 no matter which colour it is, which is the point:
    /// "completed the lifecycle" and "drew something" are different claims and
    /// must not share a number.
    #[must_use]
    pub fn ink(&self) -> usize {
        let mut hist = [0usize; 256];
        for &p in &self.pixels {
            if let Some(h) = hist.get_mut(usize::from(p)) {
                *h = h.saturating_add(1);
            }
        }
        let dominant = hist.iter().copied().max().unwrap_or(0);
        self.pixels.len().saturating_sub(dominant)
    }
}

/// The classic Mac 8-bit system palette, abbreviated.
///
/// Index 0 is white and index 255 is black on real hardware — the opposite of
/// what most modern code assumes. Modules that "erase to white" write index 0.
fn default_palette() -> Vec<[u8; 3]> {
    let mut p = vec![[0u8, 0, 0]; 256];
    // A 6×6×6 colour cube over indices 0..216, then greys, with 0 = white and
    // 255 = black to match the Mac convention.
    let mut i = 0usize;
    for r in 0..6u16 {
        for g in 0..6u16 {
            for b in 0..6u16 {
                if let Some(slot) = p.get_mut(i) {
                    *slot = [
                        (255 - r * 51) as u8,
                        (255 - g * 51) as u8,
                        (255 - b * 51) as u8,
                    ];
                }
                i = i.saturating_add(1);
            }
        }
    }
    for (n, slot) in p.iter_mut().enumerate().skip(216) {
        let v = 255u16.saturating_sub(((n - 216) as u16).saturating_mul(6));
        *slot = [v as u8, v as u8, v as u8];
    }
    if let Some(last) = p.last_mut() {
        *last = [0, 0, 0];
    }
    p
}

/// What a line is drawn with: one colour, or the pen pattern over fore/back.
#[derive(Debug, Clone, Copy)]
enum Ink {
    Flat(u8),
    Pattern([u8; 8], u8, u8),
}

/// Bytes per screen row at 8 bits per pixel.
pub const SCREEN_ROW_BYTES: u32 = SCREEN_WIDTH as u32;

/// How many drawn strings [`QuickDraw::record_drawn_text`] keeps before dropping
/// the oldest. A module that draws text every frame and a host that never drains
/// must still cost a fixed amount.
pub const TEXT_LOG_LINES: usize = 32;

/// How many characters of each are kept. Long enough for any prompt a module
/// puts on screen; short enough that the whole log is a few kilobytes.
pub const TEXT_LOG_CHARS: usize = 80;

/// Palette index for black.
///
/// In the Macintosh 8-bit system palette index 0 is **white** and 255 is black —
/// the opposite of the modern assumption, and the reason a "blank" screen came
/// out white here.
pub const BLACK_INDEX: u8 = 255;

/// QuickDraw state: the pen, the port bounds, and the framebuffer.
#[derive(Debug)]
pub struct QuickDraw {
    pub fb: Framebuffer,
    /// Pen position, in global coordinates.
    pub pen_h: i16,
    pub pen_v: i16,
    /// Current foreground colour index.
    pub fore: u8,
    /// Current background colour index.
    pub back: u8,
    /// Pen pattern: 8 rows of 8 bits. A set bit means foreground.
    ///
    /// Patterns are not decoration. Modules blank the screen to black and then
    /// draw with a *white or grey pattern*; a runtime that ignores the pattern and
    /// always uses the foreground colour renders black on black — a full lifecycle
    /// with nothing to see.
    pub pen_pat: [u8; 8],
    /// Background pattern, used by `_EraseRect` and friends.
    pub back_pat: [u8; 8],
    /// Address of the emulated `blankRgn` handle handed to modules.
    pub blank_rgn: u32,
    /// Trace where drawing lands. Set from `Diagnostics::qd_log` by the Toolbox
    /// so the rasteriser never has to reach for global state.
    pub log: bool,
    /// Strikes the host loaded from the user's System file; see [`crate::fonts`].
    pub fonts: crate::fonts::FontBank,
    /// Bounds of the screen, as `blankRgn.rgnBBox`.
    pub bounds: Rect,
    /// Address of the screen bitmap inside the emulated address space.
    ///
    /// This is the authoritative surface. Modules may write to it directly.
    pub screen_base: u32,
    /// Regions the module allocated, so `_DisposeRgn` can validate.
    regions: Vec<u32>,
    /// The current `GrafPort`, or 0 for the screen.
    ///
    /// Every drawing primitive resolves its destination through this. Writing
    /// directly to the screen instead — as an earlier version did — sends output
    /// from a module's offscreen port onto the display: Flying Toasters' `FillRect`
    /// blanked the real screen while its sprite buffers stayed empty.
    pub cur_port: u32,
    /// Set between `_OpenRgn` and `_CloseRgn`.
    ///
    /// While recording, drawing calls contribute to a region's shape instead of
    /// marking the screen. Regions here are rectangular, so recording accumulates
    /// a bounding box — exact for the rect and line shapes modules actually build
    /// regions from, and an over-approximation for anything curved.
    recording: bool,
    /// Bounding box accumulated while recording.
    record_box: Option<Rect>,
    /// The `PolyHandle` between `_OpenPoly` and `_ClosePoly`, if any.
    ///
    /// While set, `LineTo`/`Line` append vertices instead of drawing — that is
    /// how QuickDraw records a polygon, and drawing during the record would
    /// leave stray outlines on screen.
    open_poly: Option<u32>,
    /// Vertices collected for the polygon being recorded.
    poly_points: Vec<(i16, i16)>,
    /// The colour-search procedure installed by `_AddSearch`, if any.
    ///
    /// See the `$AA3A` arm: while one is installed, an `RGBColor`'s red word is
    /// a palette index rather than an intensity. The address is kept so
    /// `_DelSearch` can check it is removing the proc that was added, and so the
    /// log can say which one is in force.
    search_proc: Option<u32>,
    /// Each port's fore/back pair, stashed while the port is not current.
    ///
    /// See [`QuickDraw::switch_port_colours`]. The live pair for the *current*
    /// port stays in `fore`/`back`, where the rasteriser reads it.
    port_colours: std::collections::BTreeMap<u32, (u8, u8)>,
    /// Strings the module has drawn since a host last drained them.
    ///
    /// See [`QuickDraw::record_drawn_text`] for why a host wants these.
    text_log: Vec<String>,
    /// Colour each `_MakeRGBPat` pixel pattern stands for, keyed by handle.
    ///
    /// Colour QuickDraw keeps this inside the `PixPat`'s expanded data, whose
    /// layout is private to the ROM and which no module in this corpus reads —
    /// GeoBounce only passes the handle back to `_PenPixPat`. Recording the
    /// colour here says exactly what is known and invents no memory layout, and
    /// a pattern this map has never seen is a hard failure rather than a guess.
    rgb_pats: std::collections::BTreeMap<u32, [u8; 3]>,
}

/// An `ICON` resource is a 32x32 one-bit image: 128 bytes, four per row.
pub const ICON_SIDE: i16 = 32;
/// Row stride of an `ICON`.
pub const ICON_ROW_BYTES: u16 = 4;
/// Offset of the mask `BitMap` within a `cicn`, after the 50-byte `PixMap`.
const CICN_MASK: u32 = 50;
/// Offset of the black-and-white `BitMap` within a `cicn`.
const CICN_BMAP: u32 = 64;
/// Bytes of fixed header in a `cicn`: `PixMap`, two `BitMap`s and a `Handle`.
const CICN_HEADER: u32 = 82;
/// A `PixPat` record: `patType(2) patMap(4) patData(4) patXData(4)
/// patXValid(2) patXMap(4) pat1Data(8)`.
const PIXPAT_SIZE: u32 = 28;
/// Offset of `patXValid` within a `PixPat`; -1 means "no expanded copy".
const PIXPAT_X_VALID: u32 = 14;
/// Offset of the old-style 8-byte `pat1Data` within a `PixPat`.
const PIXPAT_1_DATA: u32 = 20;
/// `ColorTable` header: `ctSeed(4) ctFlags(2) ctSize(2)`.
const CT_HEADER: u32 = 8;
/// One `ColorSpec`: `value(2) rgb(6)`.
const CT_SPEC: u32 = 8;

/// `_SlopeFromAngle`: the Fixed slope of a ray at `angle` degrees.
///
/// Angles run clockwise from twelve o'clock, so the ray points `(sin, -cos)`
/// and the slope is `-tan`. Where the ray is horizontal the slope is unbounded;
/// Fixed saturates instead, which is what callers dividing by it rely on. See
/// the `$A8BC` dispatch arm for how Rainstorm pins this down.
#[must_use]
pub fn slope_from_angle(angle: i16) -> i32 {
    let deg = i32::from(angle).rem_euclid(180);
    if deg == 0 {
        return 0;
    }
    if deg == 90 {
        return i32::MAX;
    }
    // Rounded, not truncated: `tan(45°)` comes back as 0.999... in binary
    // floating point, and truncation would return $FFFF instead of a clean 1.0.
    let t = (-(f64::from(deg)).to_radians().tan() * 65536.0).round();
    t.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

/// Nearest entry in `palette` to `rgb`, by squared distance.
#[must_use]
pub fn nearest_in(palette: &[[u8; 3]], rgb: [u8; 3]) -> u8 {
    let mut best = 0u8;
    let mut best_d = i32::MAX;
    for (i, c) in palette.iter().enumerate() {
        let d = (i32::from(c[0]) - i32::from(rgb[0])).pow(2)
            + (i32::from(c[1]) - i32::from(rgb[1])).pow(2)
            + (i32::from(c[2]) - i32::from(rgb[2])).pow(2);
        if d < best_d {
            best_d = d;
            best = u8::try_from(i).unwrap_or(0);
        }
    }
    best
}

/// A `Polygon` is `{ u16 polySize; Rect polyBBox; Point pts[] }`.
pub const POLY_HEADER_SIZE: u32 = 10;
/// Byte offset of `polyBBox` within a `Polygon`.
pub const POLY_BBOX_OFFSET: u32 = 2;

/// A classic `Region` is `{ u16 rgnSize; Rect rgnBBox; ... }`. Rectangular
/// regions are exactly 10 bytes, which is what modules read `rgnBBox` out of.
pub const RGN_HEADER_SIZE: u32 = 10;
/// Byte offset of `rgnBBox` within a `Region`.
pub const RGN_BBOX_OFFSET: u32 = 2;

impl QuickDraw {
    #[must_use]
    pub fn new(mem: &mut Memory) -> Self {
        let bounds = Rect::new(0, 0, SCREEN_HEIGHT as i16, SCREEN_WIDTH as i16);
        let blank_rgn = Self::make_region(mem, &bounds);
        let screen_base = mem.reserve_host(
            SCREEN_ROW_BYTES.saturating_mul(u32::from(SCREEN_HEIGHT)),
            "screen bitmap",
        );
        // ScrnBase is where pre-Color-QuickDraw code finds the screen.
        mem.write_u32(ad_memory::globals::SCRN_BASE, screen_base);
        Self {
            screen_base,
            fb: Framebuffer::new(SCREEN_WIDTH, SCREEN_HEIGHT),
            pen_h: 0,
            pen_v: 0,
            // QuickDraw's own defaults: black ink on a white background. Two
            // earlier inversions of these were compensations for the zeroed
            // patterns in the param block's QD globals copy — see
            // `build_param_block` in ad-host-v2 for that story.
            fore: BLACK_INDEX,
            back: 0,
            pen_pat: [0xFF; 8],  // solid: every bit foreground
            back_pat: [0x00; 8], // solid background
            blank_rgn,
            log: false,
            fonts: crate::fonts::FontBank::default(),
            bounds,
            regions: Vec::new(),
            cur_port: 0,
            recording: false,
            record_box: None,
            open_poly: None,
            poly_points: Vec::new(),
            search_proc: None,
            port_colours: std::collections::BTreeMap::new(),
            text_log: Vec::new(),
            rgb_pats: std::collections::BTreeMap::new(),
        }
    }

    /// Is the pattern bit at this pixel set?
    fn pat_bit(pat: &[u8; 8], x: i32, y: i32) -> bool {
        let row = pat.get((y.rem_euclid(8)) as usize).copied().unwrap_or(0xFF);
        let bit = 7 - (x.rem_euclid(8));
        row & (1 << bit) != 0
    }

    /// Fill a rect through a pattern, choosing foreground or background per pixel.
    fn fill_rect_pat(&self, mem: &mut Memory, r: &Rect, pat: &[u8; 8], fore: u8, back: u8) {
        let dst = self.dest(mem);
        if self.log {
            eprintln!(
                "[qd] fill_pat {r:?} pat={pat:02x?} fore={fore} back={back} -> base={:#x}",
                dst.base
            );
        }
        for y in i32::from(r.top)..i32::from(r.bottom) {
            for x in i32::from(r.left)..i32::from(r.right) {
                let c = if Self::pat_bit(pat, x, y) { fore } else { back };
                Self::plot_on(&dst, mem, x, y, c);
            }
        }
    }

    /// Read an 8-byte `Pattern` from memory.
    fn read_pat(mem: &mut Memory, addr: u32) -> [u8; 8] {
        let mut p = [0u8; 8];
        for (i, b) in p.iter_mut().enumerate() {
            *b = mem.read_u8(addr.wrapping_add(i as u32));
        }
        p
    }

    /// Fold a rect into the region being recorded.
    fn record_rect(&mut self, r: &Rect) {
        self.record_box = Some(match self.record_box {
            Some(b) => b.union(r),
            None => *r,
        });
    }

    /// The surface drawing currently targets.
    ///
    /// The current port's pixels if it has resolvable bits, otherwise the screen.
    /// Resolved per trap rather than cached, because a module may repoint a port
    /// by writing its `PixMap` directly.
    pub fn dest(&self, mem: &mut Memory) -> crate::blit::Surface {
        if self.cur_port != 0 {
            let bits = self.cur_port.wrapping_add(crate::port::port::PORT_BITS);
            if let Some(s) = crate::blit::Surface::resolve(mem, bits) {
                return s;
            }
        }
        self.screen_surface()
    }

    /// A surface describing the physical screen.
    #[must_use]
    pub fn screen_surface(&self) -> crate::blit::Surface {
        crate::blit::Surface {
            base: self.screen_base,
            row_bytes: SCREEN_ROW_BYTES,
            bounds: Rect::new(0, 0, SCREEN_HEIGHT as i16, SCREEN_WIDTH as i16),
            pixel_size: 8,
            color_table: 0,
        }
    }

    /// Write one pixel to a surface.
    fn plot_on(dst: &crate::blit::Surface, mem: &mut Memory, x: i32, y: i32, colour: u8) {
        dst.set(mem, x, y, colour);
    }

    fn fill_rect_mem(&self, mem: &mut Memory, r: &Rect, colour: u8) {
        let dst = self.dest(mem);
        if self.log {
            eprintln!("[qd] fill {r:?} colour={colour} -> base={:#x}", dst.base);
        }
        for y in i32::from(r.top)..i32::from(r.bottom) {
            for x in i32::from(r.left)..i32::from(r.right) {
                Self::plot_on(&dst, mem, x, y, colour);
            }
        }
    }

    /// Invert a rect's pixels, palette-wise: swap each index with its complement.
    fn invert_rect_mem(&self, mem: &mut Memory, r: &Rect) {
        let dst = self.dest(mem);
        for y in i32::from(r.top)..i32::from(r.bottom) {
            for x in i32::from(r.left)..i32::from(r.right) {
                if let Some(v) = dst.get(mem, x, y) {
                    dst.set(mem, x, y, !v);
                }
            }
        }
    }

    /// Bounding box of a point list, in QuickDraw's exclusive-edge convention.
    fn bbox_of(pts: &[(i16, i16)]) -> Rect {
        let Some(&(h0, v0)) = pts.first() else {
            return Rect::default();
        };
        let (mut l, mut t, mut rr, mut b) = (h0, v0, h0, v0);
        for &(h, v) in pts {
            l = l.min(h);
            t = t.min(v);
            rr = rr.max(h);
            b = b.max(v);
        }
        Rect::new(t, l, b.saturating_add(1), rr.saturating_add(1))
    }

    /// Read a `Polygon`'s vertices out of memory.
    fn read_poly(mem: &mut Memory, handle: u32) -> Vec<(i16, i16)> {
        let Some(block) = mem.deref_handle(handle) else {
            return Vec::new();
        };
        let size = u32::from(mem.read_u16(block));
        if size < POLY_HEADER_SIZE {
            return Vec::new();
        }
        let mut pts = Vec::new();
        let mut at = block.wrapping_add(POLY_HEADER_SIZE);
        let end = block.wrapping_add(size);
        while at.wrapping_add(4) <= end {
            let v = mem.read_u16(at) as i16;
            let h = mem.read_u16(at.wrapping_add(2)) as i16;
            pts.push((h, v));
            at = at.wrapping_add(4);
        }
        pts
    }

    /// Scanline-fill a polygon, even-odd rule.
    fn fill_poly(
        &self,
        mem: &mut Memory,
        pts: &[(i16, i16)],
        pat: &[u8; 8],
        fore: u8,
        back: u8,
    ) {
        if pts.len() < 3 {
            return;
        }
        let dst = self.dest(mem);
        let bbox = Self::bbox_of(pts);
        for y in i32::from(bbox.top)..i32::from(bbox.bottom) {
            // Crossings of this scanline, at pixel centres so a vertex exactly
            // on an integer row does not count twice.
            let yc = y as f64 + 0.5;
            let mut xs: Vec<f64> = Vec::new();
            for i in 0..pts.len() {
                let (x1, y1) = (f64::from(pts[i].0), f64::from(pts[i].1));
                let j = (i + 1) % pts.len();
                let (x2, y2) = (f64::from(pts[j].0), f64::from(pts[j].1));
                if (y1 <= yc) != (y2 <= yc) {
                    xs.push(x1 + (yc - y1) / (y2 - y1) * (x2 - x1));
                }
            }
            xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            for pair in xs.chunks(2) {
                if let [from, to] = pair {
                    #[allow(
                        clippy::cast_possible_truncation,
                        reason = "screen coordinates are small"
                    )]
                    for x in (from.ceil() as i32)..(to.ceil() as i32) {
                        let c = if Self::pat_bit(pat, x, y) { fore } else { back };
                        Self::plot_on(&dst, mem, x, y, c);
                    }
                }
            }
        }
    }

    /// Draw an arc or wedge of the ellipse inscribed in `r`.
    ///
    /// `start` and `extent` are degrees clockwise from twelve o'clock, which is
    /// QuickDraw's convention: zero is up and angles increase to the right.
    #[allow(clippy::too_many_arguments, reason = "one parameter per QuickDraw verb")]
    fn arc(
        &self,
        mem: &mut Memory,
        r: &Rect,
        start: i16,
        extent: i16,
        pat: &[u8; 8],
        fore: u8,
        back: u8,
        frame_only: bool,
        invert: bool,
    ) {
        let (a, b) = (f64::from(r.width()) / 2.0, f64::from(r.height()) / 2.0);
        if a <= 0.0 || b <= 0.0 || extent == 0 {
            return;
        }
        let cx = f64::from(r.left) + a;
        let cy = f64::from(r.top) + b;
        let to_xy = |deg: f64| {
            let rad = (deg - 90.0).to_radians();
            (cx + a * rad.cos(), cy + b * rad.sin())
        };
        let steps = extent.unsigned_abs().max(1) as usize * 2;
        let step = f64::from(extent) / steps as f64;
        if frame_only {
            let dst = self.dest(mem);
            for i in 0..=steps {
                let (x, y) = to_xy(f64::from(start) + step * i as f64);
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "screen coordinates are small"
                )]
                Self::plot_on(&dst, mem, x.round() as i32, y.round() as i32, fore);
            }
            return;
        }
        // A filled arc is the wedge: centre plus the sampled edge.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "screen coordinates are small"
        )]
        let mut pts: Vec<(i16, i16)> = vec![(cx.round() as i16, cy.round() as i16)];
        for i in 0..=steps {
            let (x, y) = to_xy(f64::from(start) + step * i as f64);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "screen coordinates are small"
            )]
            pts.push((x.round() as i16, y.round() as i16));
        }
        if invert {
            let bbox = Self::bbox_of(&pts);
            self.invert_rect_mem(mem, &bbox);
        } else {
            self.fill_poly(mem, &pts, pat, fore, back);
        }
    }

    /// Draw a line between two `Point`s, for the polygon verbs.
    fn line_pts(&self, mem: &mut Memory, from: (i16, i16), to: (i16, i16), colour: u8) {
        self.line_mem(
            mem,
            i32::from(from.0),
            i32::from(from.1),
            i32::from(to.0),
            i32::from(to.1),
            colour,
        );
    }

    fn frame_rect_mem(&self, mem: &mut Memory, r: &Rect, colour: u8) {
        if r.is_empty() {
            return;
        }
        let dst = self.dest(mem);
        for x in i32::from(r.left)..i32::from(r.right) {
            Self::plot_on(&dst, mem, x, i32::from(r.top), colour);
            Self::plot_on(&dst, mem, x, i32::from(r.bottom).saturating_sub(1), colour);
        }
        for y in i32::from(r.top)..i32::from(r.bottom) {
            Self::plot_on(&dst, mem, i32::from(r.left), y, colour);
            Self::plot_on(&dst, mem, i32::from(r.right).saturating_sub(1), y, colour);
        }
    }

    /// Bresenham line, inclusive of the start and exclusive of the end, which is
    /// QuickDraw's `LineTo` rule.
    fn line_mem(&self, mem: &mut Memory, x0: i32, y0: i32, x1: i32, y1: i32, colour: u8) {
        self.line_mem_pat(mem, x0, y0, x1, y1, Ink::Flat(colour));
    }

    /// A line whose colour comes through the pen pattern when one is given.
    ///
    /// The pen draws its *pattern*, not its foreground: a set bit paints fore, a
    /// clear bit paints **back** — so a module that sets the white pattern and a
    /// background colour draws lines in that colour. Lissajous is the module
    /// this matters for: `PenPat(white)` plus an animated `RGBBackColor` is how
    /// every one of its curves picks its hue, and a fore-only line rasteriser
    /// drew the lot in the screen port's black.
    fn line_mem_pat(&self, mem: &mut Memory, x0: i32, y0: i32, x1: i32, y1: i32, ink: Ink) {
        let dst = self.dest(mem);
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        let (mut x, mut y) = (x0, y0);
        loop {
            if x == x1 && y == y1 {
                break;
            }
            let c = match ink {
                Ink::Pattern(p, f, b) => {
                    if Self::pat_bit(&p, x, y) { f } else { b }
                }
                Ink::Flat(c) => c,
            };
            Self::plot_on(&dst, mem, x, y, c);
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

    /// Midpoint ellipse inscribed in `r`, outline only.
    fn frame_oval_mem(&self, mem: &mut Memory, r: &Rect, colour: u8) {
        if r.is_empty() {
            return;
        }
        let (a, b) = (r.width() / 2, r.height() / 2);
        if a == 0 || b == 0 {
            self.fill_rect_mem(mem, r, colour);
            return;
        }
        let cx = i32::from(r.left) + a;
        let cy = i32::from(r.top) + b;
        let dst = self.dest(mem);
        let mut plot4 = |dx: i32, dy: i32| {
            Self::plot_on(&dst, mem, cx + dx, cy + dy, colour);
            Self::plot_on(&dst, mem, cx - dx, cy + dy, colour);
            Self::plot_on(&dst, mem, cx + dx, cy - dy, colour);
            Self::plot_on(&dst, mem, cx - dx, cy - dy, colour);
        };
        // The decision variables are quartic in the radii: `b2 * (x*x + x)` for a
        // full-screen oval is about 1.0e10, five times `i32::MAX`. Tunnel is the
        // first module to frame an oval that big, and in a debug build the
        // overflow aborts the process. Widening to `i64` costs nothing here and
        // keeps the rasteriser exact.
        let (a, b) = (i64::from(a), i64::from(b));
        let (a2, b2) = (a * a, b * b);
        // Region 1: slope > -1.
        let mut x = 0i64;
        let mut y = b;
        let mut d1 = b2 - a2 * b + a2 / 4;
        let (mut dx, mut dy) = (2 * b2 * x, 2 * a2 * y);
        while dx < dy {
            plot4(x as i32, y as i32);
            if d1 < 0 {
                x += 1;
                dx += 2 * b2;
                d1 += dx + b2;
            } else {
                x += 1;
                y -= 1;
                dx += 2 * b2;
                dy -= 2 * a2;
                d1 += dx - dy + b2;
            }
        }
        // Region 2: slope <= -1.
        let mut d2 = b2 * (x * x + x) + a2 * (y - 1) * (y - 1) - a2 * b2;
        while y >= 0 {
            plot4(x as i32, y as i32);
            if d2 > 0 {
                y -= 1;
                dy -= 2 * a2;
                d2 += a2 - dy;
            } else {
                y -= 1;
                x += 1;
                dx += 2 * b2;
                dy -= 2 * a2;
                d2 += dx - dy + a2;
            }
        }
    }

    /// Solid ellipse inscribed in `r`, by scanline.
    fn fill_oval_mem(&self, mem: &mut Memory, r: &Rect, colour: u8) {
        if r.is_empty() {
            return;
        }
        let (a, b) = (r.width() / 2, r.height() / 2);
        if a == 0 || b == 0 {
            self.fill_rect_mem(mem, r, colour);
            return;
        }
        let cx = i32::from(r.left) + a;
        let cy = i32::from(r.top) + b;
        let dst = self.dest(mem);
        // Quartic in the radii, exactly as in `frame_oval_mem` above — and missed
        // here when that one was widened, because Tunnel *frames* its big oval and
        // nothing had yet *painted* one. Pearls does, and the two builds failed
        // differently: debug aborted the process on the overflow, release wrapped
        // and compared against a negative product, so the scanline ended at the
        // wrong x and the ellipse was quietly the wrong shape. The silent version
        // is the worse one, and it is why this is widened rather than clamped.
        let (a, b) = (i64::from(a), i64::from(b));
        let (a2, b2) = (a * a, b * b);
        for y in -b..=b {
            // x = a * sqrt(1 - y^2/b^2), integer-only to stay deterministic.
            let num = (b2 - y * y).max(0);
            let mut x = 0i64;
            // Worst case with an i16 rect is ~1.2e18, inside i64's 9.2e18.
            while (x + 1) * (x + 1) * b2 <= a2 * num {
                x += 1;
            }
            let (x, y) = (x as i32, y as i32);
            for px in (cx - x)..=(cx + x) {
                Self::plot_on(&dst, mem, px, cy + y, colour);
            }
        }
    }

    /// Copy a rectangle of screen to another position, for `_CopyBits` and
    /// `_ScrollRect`. Reads the source first so overlap cannot smear.
    fn blit(&self, mem: &mut Memory, src: &Rect, dst: &Rect) {
        let surf = self.dest(mem);
        if src.is_empty() || dst.is_empty() {
            return;
        }
        let (w, h) = (src.width().min(dst.width()), src.height().min(dst.height()));
        let mut row = vec![0u8; w.max(0) as usize];
        for dy in 0..h {
            for (i, cell) in row.iter_mut().enumerate() {
                let sx = i32::from(src.left) + i as i32;
                let sy = i32::from(src.top) + dy;
                *cell = surf.get(mem, sx, sy).unwrap_or(0);
            }
            for (i, v) in row.iter().enumerate() {
                Self::plot_on(
                    &surf,
                    mem,
                    i32::from(dst.left) + i as i32,
                    i32::from(dst.top) + dy,
                    *v,
                );
            }
        }
    }

    /// Read a region's bounding box.
    fn rgn_box(mem: &mut Memory, rgn: u32) -> Rect {
        match mem.deref_handle(rgn) {
            Some(block) => Rect::read(mem, block.wrapping_add(RGN_BBOX_OFFSET)),
            None => Rect::default(),
        }
    }

    /// Write a region's bounding box.
    fn set_rgn_box(mem: &mut Memory, rgn: u32, r: &Rect) {
        if let Some(block) = mem.deref_handle(rgn) {
            mem.write_u16(block, RGN_HEADER_SIZE as u16);
            r.write(mem, block.wrapping_add(RGN_BBOX_OFFSET));
        }
    }

    /// The current port's text font, size and mode.
    ///
    /// Read from the **port**, not cached here, because that is where QuickDraw
    /// keeps them: a module that opens an offscreen port, sets a size, and
    /// switches back must not find the offscreen size still in effect.
    fn text_state(&self, mem: &mut Memory) -> (i16, i16, i16) {
        if self.cur_port == 0 {
            return (0, 12, 1);
        }
        use crate::port::port as pf;
        (
            mem.read_u16(self.cur_port.wrapping_add(pf::TX_FONT)) as i16,
            mem.read_u16(self.cur_port.wrapping_add(pf::TX_SIZE)) as i16,
            mem.read_u16(self.cur_port.wrapping_add(pf::TX_MODE)) as i16,
        )
    }

    /// The strike the current port's font and size select.
    fn current_font(&self, mem: &mut Memory) -> Option<ad_resource::BitmapFont<'_>> {
        let (family, size) = {
            let (f, s, _) = self.text_state(mem);
            (f, if s <= 0 { 12 } else { s })
        };
        self.fonts.best(family, size)
    }

    /// Ink positions for `bytes` drawn from `(h, v)`, and the total advance.
    ///
    /// Returns owned points rather than drawing directly because the strike
    /// borrows `self.fonts` while plotting needs `&mut self` for the pen. A few
    /// thousand points for a line of text is cheaper than fighting the borrow
    /// checker with a temporary move, and it keeps both halves readable.
    fn text_ink(&self, mem: &mut Memory, bytes: &[u8], h: i16, v: i16) -> (Vec<(i32, i32)>, i32) {
        let Some(font) = self.current_font(mem) else {
            return (Vec::new(), 0);
        };
        let mut points = Vec::new();
        let mut pen = i32::from(h);
        let top = i32::from(v).saturating_sub(i32::from(font.ascent));
        for &ch in bytes {
            let Some(g) = font.glyph(ch) else { continue };
            for bit in 0..g.bits {
                for row in 0..font.rect_height {
                    if font.strike_bit(g.strike_bit.saturating_add(bit), row) {
                        points.push((
                            pen.saturating_add(i32::from(g.left)).saturating_add(i32::from(bit)),
                            top.saturating_add(i32::from(row)),
                        ));
                    }
                }
            }
            pen = pen.saturating_add(i32::from(g.advance));
        }
        (points, pen.saturating_sub(i32::from(h)))
    }

    /// Draw `bytes` at the pen and advance it — `_DrawChar`/`_DrawString`/`_DrawText`.
    ///
    /// Ink only, in the foreground colour, whatever the text mode. `srcCopy` would
    /// also paint each glyph's box in the background colour; every module on this
    /// disk draws light text onto its own dark scene, so painting boxes would be a
    /// visible regression justified only by a mode word this project cannot yet
    /// check against the original. Recorded rather than assumed.
    fn draw_text_bytes(&mut self, mem: &mut Memory, bytes: &[u8]) {
        let (h, v) = (self.pen_h, self.pen_v);
        let (points, advance) = self.text_ink(mem, bytes, h, v);
        if points.is_empty() && advance == 0 {
            // No font loaded at all: still move the pen by a plausible amount so
            // a module laying out columns does not stack everything at x.
            self.pen_h = self.pen_h.saturating_add(
                i16::try_from(bytes.len().saturating_mul(6)).unwrap_or(i16::MAX),
            );
            return;
        }
        let dst = self.dest(mem);
        let colour = self.fore;
        if self.log {
            eprintln!(
                "[qd] text {:?} at ({h},{v}) {} px fore={colour} -> base={:#x}",
                String::from_utf8_lossy(bytes),
                points.len(),
                dst.base
            );
        }
        self.record_drawn_text(bytes);
        for (x, y) in points {
            Self::plot_on(&dst, mem, x, y, colour);
        }
        self.pen_h = self
            .pen_h
            .saturating_add(i16::try_from(advance).unwrap_or(i16::MAX));
        if self.cur_port != 0 {
            let at = self.cur_port.wrapping_add(crate::port::port::PN_LOC);
            mem.write_u16(at, self.pen_v as u16);
            mem.write_u16(at.wrapping_add(2), self.pen_h as u16);
        }
    }

    /// Swap the live fore/back pair as the current port changes.
    ///
    /// QuickDraw keeps colours **in the port**; this runtime keeps the live pair
    /// in `self.fore`/`self.back` for the rasteriser, so a port switch must
    /// stash the old port's pair and load the new one. A port never seen before
    /// gets `InitPort`'s defaults — black ink on white — which is what
    /// `_OpenCPort` gives a module's own offscreen port on a real Mac. The
    /// screen port is the deliberate exception (black on black, After Dark's
    /// own handover state, see `Screen::init_port`), and it is seeded by
    /// [`Self::seed_port_colours`] rather than special-cased here.
    pub fn switch_port_colours(&mut self, old_port: u32, new_port: u32) {
        if old_port == new_port {
            return;
        }
        self.port_colours.insert(old_port, (self.fore, self.back));
        let (f, b) = self
            .port_colours
            .get(&new_port)
            .copied()
            .unwrap_or((BLACK_INDEX, 0));
        self.fore = f;
        self.back = b;
    }

    /// Note a string the module put on screen, for a host that needs to know.
    ///
    /// A module's own words are the only account it gives of what it is *asking
    /// for* — "Enter your name:" is Lunatic Fringe telling the player it wants
    /// typing rather than flying, and nothing else about the machine's state
    /// distinguishes those two moments: name entry happens inside the same
    /// `DrawFrame` call as the whole game, polling the same `KeyMap`.
    ///
    /// Bounded on both axes and dropped rather than grown, because this is a
    /// convenience for a host and must never become a way for a long-running
    /// module to consume memory. A host that does not drain it pays for at most
    /// [`TEXT_LOG_LINES`] short strings.
    fn record_drawn_text(&mut self, bytes: &[u8]) {
        if self.text_log.len() >= TEXT_LOG_LINES {
            self.text_log.remove(0);
        }
        // MacRoman, not UTF-8: the module's bytes are a Mac encoding, and the
        // shared decoder is what everything else on this path uses.
        let mut text = ad_resource::macroman::decode(bytes);
        // Truncate on a character boundary; `truncate` on a byte index would
        // panic mid-character for anything the decoder produced above U+007F.
        if text.chars().count() > TEXT_LOG_CHARS {
            text = text.chars().take(TEXT_LOG_CHARS).collect();
        }
        self.text_log.push(text);
    }

    /// Take the strings drawn since the last call. See [`Self::record_drawn_text`].
    pub fn drain_drawn_text(&mut self) -> Vec<String> {
        std::mem::take(&mut self.text_log)
    }

    /// Advance width of a run, for `_StringWidth`/`_TextWidth`/`_CharWidth`.
    ///
    /// Falls back to six pixels per character when no font is loaded, which is
    /// what this used to answer unconditionally.
    fn measure(&self, mem: &mut Memory, bytes: &[u8]) -> i16 {
        match self.current_font(mem) {
            Some(font) => i16::try_from(font.text_width(bytes)).unwrap_or(i16::MAX),
            None => i16::try_from(bytes.len().saturating_mul(6)).unwrap_or(i16::MAX),
        }
    }

    /// A Str255 at `addr` as raw bytes.
    fn pascal_bytes(mem: &mut Memory, addr: u32) -> Vec<u8> {
        if addr == 0 {
            return Vec::new();
        }
        let len = usize::from(mem.read_u8(addr));
        mem.read_bytes(addr.wrapping_add(1), len)
    }

    /// The current port's `clipRgn` handle, or 0 if there is no current port.
    fn port_clip_rgn(&self, mem: &mut Memory) -> u32 {
        if self.cur_port == 0 {
            return 0;
        }
        mem.read_u32(self.cur_port.wrapping_add(crate::port::port::CLIP_RGN))
    }

    /// A fresh region covering the whole screen.
    ///
    /// Each caller gets its **own** handle. Sharing one would make a write
    /// through any of them a write through all of them, which is how the screen
    /// port's clipRgn came to be the same object as After Dark's `blankRgn`.
    pub fn full_screen_region(mem: &mut Memory) -> u32 {
        Self::make_region(mem, &Rect::new(0, 0, SCREEN_HEIGHT as i16, SCREEN_WIDTH as i16))
    }

    /// Allocate a rectangular region and return its handle.
    fn make_region(mem: &mut Memory, r: &Rect) -> u32 {
        let h = mem.new_handle(RGN_HEADER_SIZE, true);
        if let Some(block) = mem.deref_handle(h) {
            mem.write_u16(block, RGN_HEADER_SIZE as u16);
            r.write(mem, block.wrapping_add(RGN_BBOX_OFFSET));
        }
        h
    }

    /// Try to service a QuickDraw trap.
    ///
    /// `None` means "not mine", so the caller can report it as unimplemented.
    /// `Some(Err(detail))` means it is a QuickDraw trap that failed.
    pub fn dispatch(
        &mut self,
        t: Trap,
        regs: &mut dyn Registers,
        mem: &mut Memory,
    ) -> Option<Result<(), String>> {
        // Bound to a local rather than written as
        // `is_quickdraw(..).then_some(self.dispatch_inner(..))`, which is what
        // clippy::some_filter suggests: `dispatch_inner` services the trap and
        // mutates `self`, `regs` and `mem`, so it must keep running *before*
        // the test, exactly as it did when this was `Some(..).filter(..)`.
        // `is_quickdraw` only reads `t`, so the result is the same either way —
        // but the order this runs the emulator in should not turn on a lint.
        let serviced = self.dispatch_inner(t, regs, mem);
        is_quickdraw(t.canonical()).then_some(serviced)
    }

    fn dispatch_inner(
        &mut self,
        t: Trap,
        regs: &mut dyn Registers,
        mem: &mut Memory,
    ) -> Result<(), String> {
        match t.canonical() {
            // PROCEDURE SetRect(VAR r: Rect; left,top,right,bottom: INTEGER);
            // Note the argument order is left,top,right,bottom — not the storage
            // order of a Rect.
            0xA8A7 => {
                let mut s = Stack::new(regs);
                let bottom = s.pop_i16(mem);
                let right = s.pop_i16(mem);
                let top = s.pop_i16(mem);
                let left = s.pop_i16(mem);
                let addr = s.pop_u32(mem);
                Rect::new(top, left, bottom, right).write(mem, addr);
                s.finish();
            }
            // PROCEDURE OffsetRect(VAR r: Rect; dh, dv: INTEGER);
            0xA8A8 => {
                let mut s = Stack::new(regs);
                let dv = s.pop_i16(mem);
                let dh = s.pop_i16(mem);
                let addr = s.pop_u32(mem);
                let r = Rect::read(mem, addr).offset(dh, dv);
                r.write(mem, addr);
                s.finish();
            }
            // PROCEDURE InsetRect(VAR r: Rect; dh, dv: INTEGER);
            0xA8A9 => {
                let mut s = Stack::new(regs);
                let dv = s.pop_i16(mem);
                let dh = s.pop_i16(mem);
                let addr = s.pop_u32(mem);
                let r = Rect::read(mem, addr).inset(dh, dv);
                r.write(mem, addr);
                s.finish();
            }
            // FUNCTION SectRect(a, b: Rect; VAR dst: Rect): BOOLEAN;
            0xA8AA => {
                let mut s = Stack::new(regs);
                let dst = s.pop_u32(mem);
                let b = s.pop_u32(mem);
                let a = s.pop_u32(mem);
                let r = Rect::read(mem, a).intersect(&Rect::read(mem, b));
                let empty = r.is_empty();
                if empty {
                    Rect::default().write(mem, dst);
                } else {
                    r.write(mem, dst);
                }
                s.finish_bool(mem, !empty);
            }
            // PROCEDURE UnionRect(a, b: Rect; VAR dst: Rect);
            0xA8AB => {
                let mut s = Stack::new(regs);
                let dst = s.pop_u32(mem);
                let b = s.pop_u32(mem);
                let a = s.pop_u32(mem);
                Rect::read(mem, a)
                    .union(&Rect::read(mem, b))
                    .write(mem, dst);
                s.finish();
            }
            // FUNCTION EmptyRect(r: Rect): BOOLEAN;
            0xA8AE => {
                let mut s = Stack::new(regs);
                let a = s.pop_u32(mem);
                let empty = Rect::read(mem, a).is_empty();
                s.finish_bool(mem, empty);
            }
            // FUNCTION EqualRect(a, b: Rect): BOOLEAN;
            0xA8A6 => {
                let mut s = Stack::new(regs);
                let b = s.pop_u32(mem);
                let a = s.pop_u32(mem);
                let eq = Rect::read(mem, a) == Rect::read(mem, b);
                s.finish_bool(mem, eq);
            }
            // FUNCTION PtInRect(pt: Point; r: Rect): BOOLEAN;
            // A Point is passed by value as one long: high word = v, low = h.
            0xA8AD => {
                let mut s = Stack::new(regs);
                let r = s.pop_u32(mem);
                let pt = s.pop_u32(mem);
                let v = (pt >> 16) as i16;
                let h = (pt & 0xFFFF) as i16;
                let inside = Rect::read(mem, r).contains(h, v);
                s.finish_bool(mem, inside);
            }
            // PROCEDURE MapRect(VAR r: Rect; srcRect, dstRect: Rect);
            // Scales r from srcRect's coordinate space into dstRect's.
            0xA8FA => {
                let mut s = Stack::new(regs);
                let dst_rect = s.pop_u32(mem);
                let src_rect = s.pop_u32(mem);
                let target = s.pop_u32(mem);
                let src = Rect::read(mem, src_rect);
                let dst = Rect::read(mem, dst_rect);
                let r = Rect::read(mem, target);
                map_rect(&r, &src, &dst).write(mem, target);
                s.finish();
            }
            // PROCEDURE FillRect / PaintRect / EraseRect / InverRect / FrameRect
            0xA8A5 | 0xA8A2 => {
                let mut s = Stack::new(regs);
                // FillRect takes a pattern pointer; PaintRect uses the pen's.
                let pat = if t.canonical() == 0xA8A5 {
                    let p = s.pop_u32(mem);
                    Self::read_pat(mem, p)
                } else {
                    self.pen_pat
                };
                let addr = s.pop_u32(mem);
                let r = Rect::read(mem, addr);
                if self.recording {
                    self.record_rect(&r);
                } else {
                    let (f, b) = (self.fore, self.back);
                    self.fill_rect_pat(mem, &r, &pat, f, b);
                }
                s.finish();
            }
            0xA8A3 => {
                let mut s = Stack::new(regs);
                let addr = s.pop_u32(mem);
                let r = Rect::read(mem, addr);
                let c = self.back;
                self.fill_rect_mem(mem, &r, c);
                s.finish();
            }
            0xA8A1 => {
                let mut s = Stack::new(regs);
                let addr = s.pop_u32(mem);
                let r = Rect::read(mem, addr);
                if self.recording {
                    self.record_rect(&r);
                } else {
                    let c = self.fore;
                    self.frame_rect_mem(mem, &r, c);
                }
                s.finish();
            }
            // PROCEDURE FrameOval / PaintOval / EraseOval / InvertOval(r: Rect);
            // PROCEDURE FillOval(r: Rect; pat: Pattern);
            0xA8B7..=0xA8BB => {
                let mut s = Stack::new(regs);
                if t.canonical() == 0xA8BB {
                    let _pat = s.pop_u32(mem);
                }
                let addr = s.pop_u32(mem);
                let r = Rect::read(mem, addr);
                // Between OpenRgn and CloseRgn a framed shape contributes its
                // outline to the region instead of marking the screen. Bouncing
                // Ball is the module this is *for*: the SDK's own example keeps
                // its ball as a region built by `OpenRgn; FrameOval; CloseRgn`
                // and re-reads `rgnBBox` every frame — with ovals left out of
                // recording, that box came back empty and the ball spent the
                // whole session as a zero-pixel rectangle at the right answer's
                // position.
                if self.recording && t.canonical() == 0xA8B7 {
                    self.record_rect(&r);
                } else {
                    let colour = if t.canonical() == 0xA8B9 {
                        self.back
                    } else {
                        self.fore
                    };
                    if t.canonical() == 0xA8B7 {
                        self.frame_oval_mem(mem, &r, colour);
                    } else {
                        self.fill_oval_mem(mem, &r, colour);
                    }
                }
                s.finish();
            }
            // Round rects: ovalWidth/ovalHeight are consumed but the corners are
            // drawn square for now. Shape is approximate; geometry and stack are
            // exact.
            0xA8B0..=0xA8B4 => {
                let mut s = Stack::new(regs);
                if t.canonical() == 0xA8B4 {
                    let _pat = s.pop_u32(mem);
                }
                let _oval_h = s.pop_i16(mem);
                let _oval_w = s.pop_i16(mem);
                let addr = s.pop_u32(mem);
                let r = Rect::read(mem, addr);
                let colour = if t.canonical() == 0xA8B2 {
                    self.back
                } else {
                    self.fore
                };
                if t.canonical() == 0xA8B0 {
                    self.frame_rect_mem(mem, &r, colour);
                } else {
                    self.fill_rect_mem(mem, &r, colour);
                }
                s.finish();
            }
            // PROCEDURE MoveTo(h, v: INTEGER);  PROCEDURE Move(dh, dv);
            0xA893 => {
                let mut s = Stack::new(regs);
                self.pen_v = s.pop_i16(mem);
                self.pen_h = s.pop_i16(mem);
                s.finish();
            }
            0xA894 => {
                let mut s = Stack::new(regs);
                let dv = s.pop_i16(mem);
                let dh = s.pop_i16(mem);
                self.pen_h = self.pen_h.saturating_add(dh);
                self.pen_v = self.pen_v.saturating_add(dv);
                s.finish();
            }
            // PROCEDURE LineTo(h, v: INTEGER);  PROCEDURE Line(dh, dv);
            0xA891 => {
                let mut s = Stack::new(regs);
                let v = s.pop_i16(mem);
                let h = s.pop_i16(mem);
                if self.open_poly.is_some() {
                    self.poly_points.push((h, v));
                } else {
                    let ink = Ink::Pattern(self.pen_pat, self.fore, self.back);
                    self.line_mem_pat(
                        mem,
                        i32::from(self.pen_h),
                        i32::from(self.pen_v),
                        i32::from(h),
                        i32::from(v),
                        ink,
                    );
                }
                self.pen_h = h;
                self.pen_v = v;
                s.finish();
            }
            0xA892 => {
                let mut s = Stack::new(regs);
                let dv = s.pop_i16(mem);
                let dh = s.pop_i16(mem);
                let (h, v) = (
                    self.pen_h.saturating_add(dh),
                    self.pen_v.saturating_add(dv),
                );
                if self.open_poly.is_some() {
                    self.poly_points.push((h, v));
                } else {
                    let ink = Ink::Pattern(self.pen_pat, self.fore, self.back);
                    self.line_mem_pat(
                        mem,
                        i32::from(self.pen_h),
                        i32::from(self.pen_v),
                        i32::from(h),
                        i32::from(v),
                        ink,
                    );
                }
                self.pen_h = h;
                self.pen_v = v;
                s.finish();
            }
            // PROCEDURE GetPen(VAR pt: Point);
            0xA89A => {
                let mut s = Stack::new(regs);
                let addr = s.pop_u32(mem);
                mem.write_u16(addr, self.pen_v as u16);
                mem.write_u16(addr.wrapping_add(2), self.pen_h as u16);
                s.finish();
            }
            // FUNCTION NewRgn: RgnHandle;
            0xA8D8 => {
                let h = Self::make_region(mem, &Rect::default());
                self.regions.push(h);
                let s = Stack::new(regs);
                s.finish_u32(mem, h);
            }
            // PROCEDURE DisposeRgn(rgn: RgnHandle);
            0xA8D9 => {
                let mut s = Stack::new(regs);
                let h = s.pop_u32(mem);
                self.regions.retain(|&r| r != h);
                mem.dispose_handle(h);
                s.finish();
            }
            // PROCEDURE RectRgn(rgn: RgnHandle; r: Rect);
            0xA8DF => {
                let mut s = Stack::new(regs);
                let r_addr = s.pop_u32(mem);
                let rgn = s.pop_u32(mem);
                let r = Rect::read(mem, r_addr);
                if let Some(block) = mem.deref_handle(rgn) {
                    mem.write_u16(block, RGN_HEADER_SIZE as u16);
                    r.write(mem, block.wrapping_add(RGN_BBOX_OFFSET));
                }
                s.finish();
            }
            // PROCEDURE SetEmptyRgn(rgn: RgnHandle);
            0xA8DD => {
                let mut s = Stack::new(regs);
                let rgn = s.pop_u32(mem);
                if let Some(block) = mem.deref_handle(rgn) {
                    Rect::default().write(mem, block.wrapping_add(RGN_BBOX_OFFSET));
                }
                s.finish();
            }
            // PROCEDURE FillRgn / PaintRgn / EraseRgn: fill the bounding box.
            // Non-rectangular regions are not modelled yet; every region this
            // runtime creates is rectangular, so bbox filling is exact for them.
            0xA8D6 | 0xA8D3 | 0xA8D4 => {
                let mut s = Stack::new(regs);
                let pat = if t.canonical() == 0xA8D6 {
                    let p = s.pop_u32(mem);
                    Self::read_pat(mem, p)
                } else if t.canonical() == 0xA8D4 {
                    self.back_pat
                } else {
                    self.pen_pat
                };
                let rgn = s.pop_u32(mem);
                let (f, b) = if t.canonical() == 0xA8D4 {
                    (self.back, self.back)
                } else {
                    (self.fore, self.back)
                };
                if let Some(block) = mem.deref_handle(rgn) {
                    let r = Rect::read(mem, block.wrapping_add(RGN_BBOX_OFFSET));
                    self.fill_rect_pat(mem, &r, &pat, f, b);
                }
                s.finish();
            }
            // PROCEDURE FrameRgn / InverRgn(rgn: RgnHandle);
            // Same rectangular-region caveat as the fill verbs above.
            0xA8D2 | 0xA8D5 => {
                let mut s = Stack::new(regs);
                let rgn = s.pop_u32(mem);
                if let Some(block) = mem.deref_handle(rgn) {
                    let r = Rect::read(mem, block.wrapping_add(RGN_BBOX_OFFSET));
                    if t.canonical() == 0xA8D2 {
                        let c = self.fore;
                        self.frame_rect_mem(mem, &r, c);
                    } else {
                        self.invert_rect_mem(mem, &r);
                    }
                }
                s.finish();
            }
            // FUNCTION BitTst(bytePtr: Ptr; bitNum: LONGINT): BOOLEAN;
            //
            // Bit numbering runs from the HIGH bit of the first byte: bit 0 is
            // `0x80` of `bytePtr[0]`. Numbering from the low bit instead makes
            // every flag test read the wrong bit, which is far worse than an
            // unimplemented trap because it fails quietly.
            0xA85D => {
                let mut s = Stack::new(regs);
                let bit = s.pop_u32(mem);
                let ptr = s.pop_u32(mem);
                let byte = mem.read_u8(ptr.wrapping_add(bit / 8));
                let set = byte & (0x80 >> (bit % 8)) != 0;
                s.finish_bool(mem, set);
            }
            // PROCEDURE FrameArc / PaintArc / EraseArc / InvertArc(r; start, arc)
            // PROCEDURE FillArc(r; start, arc; pat)
            //
            // Angles are clockwise from twelve o'clock, in degrees, which is
            // QuickDraw's convention and not the mathematical one.
            0xA8BE..=0xA8C2 => {
                let mut s = Stack::new(regs);
                let pat = if t.canonical() == 0xA8C2 {
                    let p = s.pop_u32(mem);
                    Some(Self::read_pat(mem, p))
                } else {
                    None
                };
                let arc_angle = s.pop_i16(mem);
                let start_angle = s.pop_i16(mem);
                let addr = s.pop_u32(mem);
                let r = Rect::read(mem, addr);
                let verb = t.canonical();
                let (fore, back) = match verb {
                    0xA8C0 => (self.back, self.back),      // EraseArc
                    _ => (self.fore, self.back),
                };
                let pat = pat.unwrap_or(if verb == 0xA8C0 {
                    self.back_pat
                } else {
                    self.pen_pat
                });
                self.arc(
                    mem,
                    &r,
                    start_angle,
                    arc_angle,
                    &pat,
                    fore,
                    back,
                    verb == 0xA8BE,
                    verb == 0xA8C1,
                );
                s.finish();
            }
            // FUNCTION SlopeFromAngle(angle: INTEGER): Fixed;
            //
            // Rainstorm proves both the shape and the orientation. The shape:
            // it pushes one word (`Random mod 121 + 30`, so 30..150 degrees) and
            // pops a longword, and the caller reserved that longword *before*
            // pushing the angle — the Pascal function layout.
            //
            // The magnitude is decided by how the result is consumed. Each
            // raindrop is an 18-byte record whose `+$a`/`+$c` are the `_Line`
            // arguments `(dh, dv)` and whose `+$e`/`+$10` are the per-frame
            // `(dv, dh)` velocity. The module fills in the two vertical members
            // from `_Random` and then derives each horizontal one as
            // `Fix2Long(FixDiv(Long2Fix(dv), slope))` — it *divides* by the
            // slope, so the slope has to grow without bound at the middle of its
            // angle range if the drops are to fall straight down there. |tan|
            // does that at 90 degrees; |cot| would divide by zero instead. That
            // rules out the cotangent and leaves the tangent.
            //
            // The sign is unobservable from this corpus, because Rainstorm draws
            // its angle from a range symmetric about 90 and a sign flip only
            // mirrors the wind. It follows QuickDraw's own orientation: angles
            // run clockwise from twelve o'clock, so a ray at `angle` points
            // `(sin, -cos)` and the slope is negative through the first
            // quadrant.
            0xA8BC => {
                let mut s = Stack::new(regs);
                let angle = s.pop_i16(mem);
                s.finish_u32(mem, slope_from_angle(angle) as u32);
            }
            // PROCEDURE PlotIcon(theRect: Rect; theIcon: Handle);
            //
            // ProtoToasters: `_SetRect(r, 0, 0, 32, 32)`, `_GetIcon` ($A9BB),
            // then this trap with the rect pushed first and the resource handle
            // second, releasing the handle straight after. A 32x32 rect and an
            // 'ICON' handle settle both the identity and the argument order —
            // `_SetCTitle`, the other candidate at this trap number, takes a
            // ControlHandle first and this module never makes a control.
            //
            // The Toolbox implements it as `_CopyBits` in `srcCopy`, so clear
            // bits paint the background colour rather than being transparent.
            0xA94B => {
                let mut s = Stack::new(regs);
                let icon = s.pop_u32(mem);
                let rect_ptr = s.pop_u32(mem);
                let dst_rect = Rect::read(mem, rect_ptr);
                s.finish();
                let Some(bits) = mem.deref_handle(icon) else {
                    return Ok(());
                };
                let src = crate::blit::Surface {
                    base: bits,
                    row_bytes: u32::from(ICON_ROW_BYTES),
                    bounds: Rect::new(0, 0, ICON_SIDE, ICON_SIDE),
                    pixel_size: 1,
                    color_table: 0,
                };
                let dst = self.dest(mem);
                let to_index = self.palette_mapper();
                crate::blit::copy_bits(
                    mem,
                    &src,
                    &dst,
                    &Rect::new(0, 0, ICON_SIDE, ICON_SIDE),
                    &dst_rect,
                    crate::blit::mode::SRC_COPY,
                    self.fore,
                    self.back,
                    None,
                    &to_index,
                );
            }
            // PROCEDURE PlotCIcon(theRect: Rect; theCIcon: CIconHandle);
            //
            // Confetti Factory: `_GetCIcon` ($AA1E) supplies the handle, this
            // trap draws it, and `_DisposCIcon` ($AA25) frees it — the pairing
            // names the type. Rect first, handle second, as in the b/w path
            // beside it (`DRAWICON` in the module's own MacsBug names).
            0xAA1F => {
                let mut s = Stack::new(regs);
                let cicon = s.pop_u32(mem);
                let rect_ptr = s.pop_u32(mem);
                let dst_rect = Rect::read(mem, rect_ptr);
                s.finish();
                self.plot_cicon(mem, cicon, &dst_rect);
            }
            // PROCEDURE CopyMask(srcBits, maskBits, dstBits: BitMap;
            //                    srcRect, maskRect, dstRect: Rect);
            //
            // Punchout's After Dark `Pixels` library holds two routines with the
            // same body: one resolves three arguments into BitMaps and calls
            // `_CopyBits`, the next resolves the same three and calls this trap
            // with three rects after them. Six pointers, no result, and the
            // MacsBug name `COPYTO` on the pair.
            //
            // A mask pixel of 0 protects the destination; anything else lets the
            // source through. That is the 1-bit behaviour, which is what the
            // module supplies (its masks come from `Pixels.GetBits` on 1-bit
            // offscreen buffers).
            0xA817 => {
                let mut s = Stack::new(regs);
                let dr = s.pop_u32(mem);
                let mr = s.pop_u32(mem);
                let sr = s.pop_u32(mem);
                let dst_bits = s.pop_u32(mem);
                let mask_bits = s.pop_u32(mem);
                let src_bits = s.pop_u32(mem);
                let src_rect = Rect::read(mem, sr);
                let mask_rect = Rect::read(mem, mr);
                let dst_rect = Rect::read(mem, dr);
                s.finish();
                let (Some(src), Some(mask), Some(dst)) = (
                    crate::blit::Surface::resolve(mem, src_bits),
                    crate::blit::Surface::resolve(mem, mask_bits),
                    crate::blit::Surface::resolve(mem, dst_bits),
                ) else {
                    return Ok(());
                };
                let to_index = self.palette_mapper();
                crate::blit::copy_mask(
                    mem,
                    &src,
                    &mask,
                    &dst,
                    &src_rect,
                    &mask_rect,
                    &dst_rect,
                    self.fore,
                    self.back,
                    &to_index,
                );
            }
            // FUNCTION OpenPoly: PolyHandle;  PROCEDURE ClosePoly;
            //
            // Between the two, `LineTo` appends vertices instead of drawing.
            // The record is `{ u16 polySize; Rect polyBBox; Point pts[] }`.
            0xA8CB => {
                let h = mem.new_handle(POLY_HEADER_SIZE, false);
                if let Some(block) = mem.deref_handle(h) {
                    mem.write_u16(block, u16::try_from(POLY_HEADER_SIZE).unwrap_or(10));
                    Rect::default().write(mem, block.wrapping_add(POLY_BBOX_OFFSET));
                }
                self.open_poly = Some(h);
                self.poly_points.clear();
                // The pen's current position is the polygon's first vertex.
                self.poly_points.push((self.pen_h, self.pen_v));
                let s = Stack::new(regs);
                s.finish_u32(mem, h);
            }
            0xA8CC => {
                // ClosePoly: write the collected points and bounding box.
                if let Some(h) = self.open_poly.take() {
                    let pts = std::mem::take(&mut self.poly_points);
                    let need = POLY_HEADER_SIZE + 4 * u32::try_from(pts.len()).unwrap_or(0);
                    mem.resize_handle(h, need);
                    if let Some(block) = mem.deref_handle(h) {
                        mem.write_u16(block, u16::try_from(need).unwrap_or(u16::MAX));
                        let bbox = Self::bbox_of(&pts);
                        bbox.write(mem, block.wrapping_add(POLY_BBOX_OFFSET));
                        for (i, (h_, v)) in pts.iter().enumerate() {
                            let at = block
                                .wrapping_add(POLY_HEADER_SIZE)
                                .wrapping_add(4 * u32::try_from(i).unwrap_or(0));
                            mem.write_u16(at, *v as u16);
                            mem.write_u16(at.wrapping_add(2), *h_ as u16);
                        }
                    }
                }
                Stack::new(regs).finish();
            }
            // PROCEDURE KillPoly(poly: PolyHandle);
            0xA8CD => {
                let mut s = Stack::new(regs);
                let h = s.pop_u32(mem);
                mem.dispose_handle(h);
                s.finish();
            }
            // PROCEDURE OffsetPoly(poly: PolyHandle; dh, dv: INTEGER);
            0xA8CE => {
                let mut s = Stack::new(regs);
                let dv = s.pop_i16(mem);
                let dh = s.pop_i16(mem);
                let h = s.pop_u32(mem);
                if let Some(block) = mem.deref_handle(h) {
                    let size = u32::from(mem.read_u16(block)).max(POLY_HEADER_SIZE);
                    let bbox = Rect::read(mem, block.wrapping_add(POLY_BBOX_OFFSET));
                    bbox.offset(dh, dv)
                        .write(mem, block.wrapping_add(POLY_BBOX_OFFSET));
                    let mut at = block.wrapping_add(POLY_HEADER_SIZE);
                    let end = block.wrapping_add(size);
                    while at.wrapping_add(4) <= end {
                        let v = mem.read_u16(at) as i16;
                        let h_ = mem.read_u16(at.wrapping_add(2)) as i16;
                        mem.write_u16(at, v.saturating_add(dv) as u16);
                        mem.write_u16(at.wrapping_add(2), h_.saturating_add(dh) as u16);
                        at = at.wrapping_add(4);
                    }
                }
                s.finish();
            }
            // PROCEDURE FramePoly / PaintPoly / ErasePoly / InvertPoly(poly)
            // PROCEDURE FillPoly(poly; pat)
            0xA8C6..=0xA8CA => {
                let mut s = Stack::new(regs);
                let pat = if t.canonical() == 0xA8CA {
                    let p = s.pop_u32(mem);
                    Some(Self::read_pat(mem, p))
                } else {
                    None
                };
                let h = s.pop_u32(mem);
                let verb = t.canonical();
                let pts = Self::read_poly(mem, h);
                let (fore, back) = if verb == 0xA8C8 {
                    (self.back, self.back)
                } else {
                    (self.fore, self.back)
                };
                let pat = pat.unwrap_or(if verb == 0xA8C8 {
                    self.back_pat
                } else {
                    self.pen_pat
                });
                if verb == 0xA8C6 {
                    // FramePoly: outline through the vertices, closing the loop.
                    let c = self.fore;
                    for w in pts.windows(2) {
                        self.line_pts(mem, w[0], w[1], c);
                    }
                    if let (Some(first), Some(last)) = (pts.first(), pts.last()) {
                        if first != last {
                            self.line_pts(mem, *last, *first, c);
                        }
                    }
                } else if verb == 0xA8C9 {
                    let bbox = Self::bbox_of(&pts);
                    self.invert_rect_mem(mem, &bbox);
                } else {
                    self.fill_poly(mem, &pts, &pat, fore, back);
                }
                s.finish();
            }
            // Colour Utilities dispatch: a selector word, pushed last, then the
            // arguments. Only the selector two modules actually use is served;
            // the rest fail loudly rather than guess, because a wrong colour
            // conversion is a silent fidelity bug.
            //
            // Selector 7 is HSV → RGB. That is not assumed — it is what the
            // callers prove. NightLines builds `{hue, $FFFF, $FFFF}` and hands
            // the result straight to `_RGBForeColor`; under HSL a lightness of
            // $FFFF is pure white, so a colour-cycling saver would draw nothing
            // but white. Lissajous independently walks the first word by $FF a
            // step, which is a hue sweep. Both only make sense as HSV.
            0xA82E => {
                // The auto-pop flavour of this call — `$AC2E`, which is how
                // Mountains' Think C glue reaches it — is handled generically in
                // `Toolbox::dispatch_toolbox`, so by here the selector is at SP+0
                // whichever word arrived.
                let mut s = Stack::new(regs);
                let selector = s.pop_u16(mem);
                if selector != 7 {
                    return Err(format!(
                        "Colour Utilities selector {selector} is not implemented"
                    ));
                }
                let out = s.pop_u32(mem);
                let src = s.pop_u32(mem);
                let hsv = [
                    mem.read_u16(src),
                    mem.read_u16(src.wrapping_add(2)),
                    mem.read_u16(src.wrapping_add(4)),
                ];
                let rgb = hsv_to_rgb(hsv);
                for (i, v) in rgb.iter().enumerate() {
                    mem.write_u16(out.wrapping_add(2 * u32::try_from(i).unwrap_or(0)), *v);
                }
                s.finish();
            }
            // FUNCTION Color2Index(c: RGBColor): LONGINT;
            0xAA33 => {
                let mut s = Stack::new(regs);
                let addr = s.pop_u32(mem);
                let idx = self.nearest_index(mem, addr);
                s.finish_u32(mem, u32::from(idx));
            }
            // PROCEDURE Index2Color(index: LONGINT; VAR c: RGBColor);
            0xAA34 => {
                let mut s = Stack::new(regs);
                let out = s.pop_u32(mem);
                let idx = s.pop_u32(mem) as u8;
                let c = self
                    .fb
                    .palette
                    .get(usize::from(idx))
                    .copied()
                    .unwrap_or([0, 0, 0]);
                // RGBColor channels are 16-bit; replicate each byte into both.
                for (i, v) in c.iter().enumerate() {
                    let w = (u16::from(*v) << 8) | u16::from(*v);
                    mem.write_u16(out.wrapping_add((i as u32) * 2), w);
                }
                s.finish();
            }
            // FUNCTION GetForeColor / GetBackColor(VAR c: RGBColor);
            0xAA19 | 0xAA1A => {
                let mut s = Stack::new(regs);
                let out = s.pop_u32(mem);
                let idx = if t.canonical() == 0xAA19 { self.fore } else { self.back };
                let c = self.fb.palette.get(usize::from(idx)).copied().unwrap_or([0, 0, 0]);
                for (i, v) in c.iter().enumerate() {
                    let w = (u16::from(*v) << 8) | u16::from(*v);
                    mem.write_u16(out.wrapping_add((i as u32) * 2), w);
                }
                s.finish();
            }
            // FUNCTION GetCPixel(h, v: INTEGER; VAR c: RGBColor);
            0xAA17 => {
                let mut s = Stack::new(regs);
                let out = s.pop_u32(mem);
                let v = s.pop_i16(mem);
                let h = s.pop_i16(mem);
                let idx = self
                    .dest(mem)
                    .get(mem, i32::from(h), i32::from(v))
                    .unwrap_or(0);
                let c = self.fb.palette.get(usize::from(idx)).copied().unwrap_or([0, 0, 0]);
                for (i, cv) in c.iter().enumerate() {
                    let w = (u16::from(*cv) << 8) | u16::from(*cv);
                    mem.write_u16(out.wrapping_add((i as u32) * 2), w);
                }
                s.finish();
            }
            // FUNCTION GetPixel(h, v: INTEGER): BOOLEAN;
            0xA865 => {
                let mut s = Stack::new(regs);
                let v = s.pop_i16(mem);
                let h = s.pop_i16(mem);
                let set = self
                    .dest(mem)
                    .get(mem, i32::from(h), i32::from(v))
                    .unwrap_or(0)
                    != 0;
                s.finish_bool(mem, set);
            }
            // PROCEDURE ForeColor / BackColor(color: LONGINT);
            0xA862 => {
                let mut s = Stack::new(regs);
                let c = s.pop_u32(mem);
                self.fore = classic_colour_index(c);
                if self.log {
                    eprintln!("[qd] ForeColor {c} -> index {}", self.fore);
                }
                s.finish();
            }
            0xA863 => {
                let mut s = Stack::new(regs);
                let c = s.pop_u32(mem);
                self.back = classic_colour_index(c);
                if self.log {
                    eprintln!("[qd] BackColor {c} -> index {}", self.back);
                }
                s.finish();
            }
            // PROCEDURE ColorBit(whichBit: INTEGER);
            //
            // Selects which colour-separation plane subsequent drawing goes to,
            // for printing on a plane-at-a-time device. Randomizer calls it with
            // 0 as the last step of a port reset — TextSize, SpaceExtra,
            // ForeColor(blackColor), BackColor(whiteColor), ColorBit(0) — and
            // this framebuffer has exactly one plane, so plane 0 is the only
            // possible destination.
            0xA864 => {
                let mut s = Stack::new(regs);
                s.skip(2);
                s.finish();
            }
            // PROCEDURE RGBForeColor / RGBBackColor(c: RGBColor);
            0xAA14 => {
                let mut s = Stack::new(regs);
                let addr = s.pop_u32(mem);
                self.fore = self.nearest_index(mem, addr);
                if self.log {
                    eprintln!("[qd] RGBForeColor -> index {}", self.fore);
                }
                s.finish();
            }
            0xAA15 => {
                let mut s = Stack::new(regs);
                let addr = s.pop_u32(mem);
                self.back = self.nearest_index(mem, addr);
                if self.log {
                    eprintln!("[qd] RGBBackColor -> index {}", self.back);
                }
                s.finish();
            }
            // PROCEDURE AddSearch(searchProc: ProcPtr);
            // PROCEDURE DelSearch(searchProc: ProcPtr);
            //
            // A search procedure is consulted by `Color2Index` — and so by
            // `_RGBForeColor` and everything else that resolves a colour — ahead
            // of the device's inverse table. Its job here is to bypass colour
            // matching entirely and let the module name a palette index:
            //
            // * Tunnel's is eight instructions and its own MacsBug name is
            //   `DummyProc`: `*position = rgb->red; return true`, ignoring green
            //   and blue — which is why its caller,
            //   `ChooseInThisGraphicsDevice(ctx, index)`, only fills in red and
            //   leaves the other two words uninitialised.
            // * Supernova's does the same with a guard (green and blue must be
            //   zero) and clamps to `gdPMap->pmTable->ctSize`; its caller zeroes
            //   the other two words, so the guard never fires.
            //
            // Two independently written procedures, one contract: **the red word
            // is the palette index**. That is what this implements, clamped as
            // Supernova clamps, because a trap handler here cannot re-enter the
            // 68000 to run the module's own code. Both modules are then exact,
            // including their genuine-colour calls — Tunnel's black is
            // `{0,0,0}`, which is index 0 under this rule as it is under
            // `DummyProc`.
            //
            // Only one distinct procedure at a time is modelled. That is what
            // both modules do, and a second, different one is a hard failure
            // rather than a silent choice between them.
            0xAA3A => {
                let mut s = Stack::new(regs);
                let proc = s.pop_u32(mem);
                if proc == 0 {
                    return Err("_AddSearch was given a nil search procedure".into());
                }
                if let Some(had) = self.search_proc {
                    if had != proc {
                        return Err(format!(
                            "_AddSearch: a second search procedure {proc:#x} while {had:#x} \
                             is installed; only one is modelled"
                        ));
                    }
                }
                if self.log {
                    eprintln!("[qd] AddSearch {proc:#x}: red channel is now a palette index");
                }
                self.search_proc = Some(proc);
                s.finish();
            }
            0xAA4C => {
                let mut s = Stack::new(regs);
                let proc = s.pop_u32(mem);
                if self.search_proc == Some(proc) {
                    self.search_proc = None;
                }
                s.finish();
            }
            // PROCEDURE SetCPixel(h, v: INTEGER; c: RGBColor);
            0xAA16 => {
                let mut s = Stack::new(regs);
                let addr = s.pop_u32(mem);
                let v = s.pop_i16(mem);
                let h = s.pop_i16(mem);
                let c = self.nearest_index(mem, addr);
                let dst = self.dest(mem);
                Self::plot_on(&dst, mem, i32::from(h), i32::from(v), c);
                s.finish();
            }
            // FUNCTION NewPixPat: PixPatHandle;
            //
            // GeoBounce takes no arguments and reserves four bytes for the
            // result (`subq #4,a7` then the bare trap), stores the handle in its
            // own state, and later feeds it to `_MakeRGBPat` and `_PenPixPat`
            // before `_DisposPixPat`. So: no arguments, one handle out.
            //
            // The record is `patType(2) patMap(4) patData(4) patXData(4)
            // patXValid(2) patXMap(4) pat1Data(8)` = 28 bytes. It is built
            // well-formed — type 1, a solid `pat1Data` — so a module that does
            // look inside sees a pattern rather than zeroes.
            0xAA07 => {
                let h = mem.new_handle(PIXPAT_SIZE, true);
                if let Some(block) = mem.deref_handle(h) {
                    mem.write_u16(block, 1); // patType: full-colour pattern
                    mem.write_u16(block.wrapping_add(PIXPAT_X_VALID), 0xFFFF);
                    for i in 0..8 {
                        mem.write_u8(block.wrapping_add(PIXPAT_1_DATA + i), 0xFF);
                    }
                }
                let s = Stack::new(regs);
                s.finish_u32(mem, h);
            }
            // PROCEDURE DisposPixPat(ppat: PixPatHandle);
            0xAA08 => {
                let mut s = Stack::new(regs);
                let h = s.pop_u32(mem);
                self.rgb_pats.remove(&h);
                mem.dispose_handle(h);
                s.finish();
            }
            // PROCEDURE MakeRGBPat(ppat: PixPatHandle; myColor: RGBColor);
            //
            // On a real 8-bit screen this builds a two-colour dither that
            // averages to `myColor`. This framebuffer resolves colour the same
            // way `_RGBForeColor` does — nearest palette entry — so the pattern
            // it stands for is that one solid index, recorded against the handle
            // for `_PenPixPat` to pick up.
            0xAA0D => {
                let mut s = Stack::new(regs);
                let addr = s.pop_u32(mem);
                let ppat = s.pop_u32(mem);
                let rgb = [
                    (mem.read_u16(addr) >> 8) as u8,
                    (mem.read_u16(addr.wrapping_add(2)) >> 8) as u8,
                    (mem.read_u16(addr.wrapping_add(4)) >> 8) as u8,
                ];
                self.rgb_pats.insert(ppat, rgb);
                if let Some(block) = mem.deref_handle(ppat) {
                    mem.write_u16(block, 2); // patType: RGB pattern
                }
                s.finish();
            }
            // PROCEDURE PenPixPat / BackPixPat(ppat: PixPatHandle);
            //
            // Only patterns this runtime made through `_MakeRGBPat` can be
            // resolved to a colour; anything else would be a guess, so it fails
            // and names the handle.
            0xAA0A | 0xAA0B => {
                let mut s = Stack::new(regs);
                let ppat = s.pop_u32(mem);
                let Some(rgb) = self.rgb_pats.get(&ppat).copied() else {
                    return Err(format!(
                        "PixPat {ppat:#x} was not built by _MakeRGBPat, so its colour is unknown"
                    ));
                };
                let idx = nearest_in(&self.fb.palette, rgb);
                if t.canonical() == 0xAA0A {
                    self.pen_pat = [0xFF; 8];
                    self.fore = idx;
                } else {
                    self.back_pat = [0xFF; 8];
                    self.back = idx;
                }
                s.finish();
            }
            // PROCEDURE PenPat / BackPat(pat: Pattern);
            0xA89D | 0xA87C => {
                let mut s = Stack::new(regs);
                let p = s.pop_u32(mem);
                let pat = Self::read_pat(mem, p);
                if t.canonical() == 0xA89D {
                    self.pen_pat = pat;
                } else {
                    self.back_pat = pat;
                }
                s.finish();
            }
            // PenSize / PenMode / PenNormal: accepted; only the pattern affects
            // what appears on screen in this implementation.
            0xA89B | 0xA89C | 0xA89E => {
                let mut s = Stack::new(regs);
                match t.canonical() {
                    0xA89B => s.skip(4),
                    0xA89C => s.skip(2),
                    _ => {}
                }
                if t.canonical() == 0xA89E {
                    self.pen_pat = [0xFF; 8];
                }
                s.finish();
            }
            // PROCEDURE SetClip(rgn: RgnHandle) / ClipRect(r: Rect).
            //
            // Recorded into the **current port's own** clipRgn. The rasteriser
            // does not yet enforce clipping, so this changes nothing on screen —
            // but it makes `GetClip` round-trip, and it can no longer reach
            // After Dark's `blankRgn`, which it could when the port shared that
            // handle. `$A873` (SetPort) is handled in the Toolbox, not here.
            0xA879 | 0xA87B => {
                let mut s = Stack::new(regs);
                let arg = s.pop_u32(mem);
                let dst = self.port_clip_rgn(mem);
                let r = if t.canonical() == 0xA87B {
                    Rect::read(mem, arg)
                } else {
                    Self::rgn_box(mem, arg)
                };
                if dst != 0 {
                    Self::set_rgn_box(mem, dst, &r);
                }
                s.finish();
            }
            // PROCEDURE GetClip(rgn: RgnHandle);
            0xA87A => {
                let mut s = Stack::new(regs);
                let out = s.pop_u32(mem);
                let src = self.port_clip_rgn(mem);
                let r = if src == 0 {
                    Rect::new(0, 0, SCREEN_HEIGHT as i16, SCREEN_WIDTH as i16)
                } else {
                    Self::rgn_box(mem, src)
                };
                Self::set_rgn_box(mem, out, &r);
                s.finish();
            }
            // PROCEDURE OpenRgn;  drawing now builds a shape instead of marking
            // the screen.
            0xA8DA => {
                self.recording = true;
                self.record_box = None;
                Stack::new(regs).finish();
            }
            // PROCEDURE CloseRgn(dstRgn: RgnHandle);
            0xA8DB => {
                let mut s = Stack::new(regs);
                let rgn = s.pop_u32(mem);
                let r = self.record_box.unwrap_or_default();
                Self::set_rgn_box(mem, rgn, &r);
                self.recording = false;
                self.record_box = None;
                s.finish();
            }
            // PROCEDURE CopyRgn(src, dst: RgnHandle);
            0xA8DC => {
                let mut s = Stack::new(regs);
                let dst = s.pop_u32(mem);
                let src = s.pop_u32(mem);
                let r = Self::rgn_box(mem, src);
                Self::set_rgn_box(mem, dst, &r);
                s.finish();
            }
            // FUNCTION EmptyRgn(rgn: RgnHandle): BOOLEAN;
            0xA8E2 => {
                let mut s = Stack::new(regs);
                let rgn = s.pop_u32(mem);
                let empty = Self::rgn_box(mem, rgn).is_empty();
                s.finish_bool(mem, empty);
            }
            // FUNCTION EqualRgn(a, b: RgnHandle): BOOLEAN;
            0xA8E3 => {
                let mut s = Stack::new(regs);
                let b = s.pop_u32(mem);
                let a = s.pop_u32(mem);
                let eq = Self::rgn_box(mem, a) == Self::rgn_box(mem, b);
                s.finish_bool(mem, eq);
            }
            // PROCEDURE SectRgn / UnionRgn / DiffRgn / XorRgn(a, b, dst);
            0xA8E4..=0xA8E7 => {
                let mut s = Stack::new(regs);
                let dst = s.pop_u32(mem);
                let b = s.pop_u32(mem);
                let a = s.pop_u32(mem);
                let (ra, rb) = (Self::rgn_box(mem, a), Self::rgn_box(mem, b));
                // Rectangular regions only: intersection is exact, and the other
                // three fall back to a bounding box, which over-approximates.
                let out = match t.canonical() {
                    0xA8E4 => ra.intersect(&rb),
                    0xA8E5 => ra.union(&rb),
                    _ => ra,
                };
                let out = if out.is_empty() { Rect::default() } else { out };
                Self::set_rgn_box(mem, dst, &out);
                s.finish();
            }
            // PROCEDURE OffsetRgn / InsetRgn(rgn: RgnHandle; dh, dv: INTEGER);
            0xA8E0 | 0xA8E1 => {
                let mut s = Stack::new(regs);
                let dv = s.pop_i16(mem);
                let dh = s.pop_i16(mem);
                let rgn = s.pop_u32(mem);
                let r = Self::rgn_box(mem, rgn);
                let out = if t.canonical() == 0xA8E0 {
                    r.offset(dh, dv)
                } else {
                    r.inset(dh, dv)
                };
                Self::set_rgn_box(mem, rgn, &out);
                s.finish();
            }
            // FUNCTION PtInRgn(pt: Point; rgn: RgnHandle): BOOLEAN;
            0xA8E8 => {
                let mut s = Stack::new(regs);
                let rgn = s.pop_u32(mem);
                let pt = s.pop_u32(mem);
                let r = Self::rgn_box(mem, rgn);
                let inside = r.contains((pt & 0xFFFF) as i16, (pt >> 16) as i16);
                s.finish_bool(mem, inside);
            }
            // FUNCTION RectInRgn(r: Rect; rgn: RgnHandle): BOOLEAN;
            0xA8E9 => {
                let mut s = Stack::new(regs);
                let rgn = s.pop_u32(mem);
                let ra = s.pop_u32(mem);
                let hit = !Rect::read(mem, ra)
                    .intersect(&Self::rgn_box(mem, rgn))
                    .is_empty();
                s.finish_bool(mem, hit);
            }
            // PROCEDURE SetRectRgn(rgn; left, top, right, bottom);
            0xA8DE => {
                let mut s = Stack::new(regs);
                let bottom = s.pop_i16(mem);
                let right = s.pop_i16(mem);
                let top = s.pop_i16(mem);
                let left = s.pop_i16(mem);
                let rgn = s.pop_u32(mem);
                Self::set_rgn_box(mem, rgn, &Rect::new(top, left, bottom, right));
                s.finish();
            }
            // PROCEDURE LocalToGlobal / GlobalToLocal(VAR pt: Point);
            // The screen port's origin is (0,0), so both are the identity — but
            // the argument still has to be consumed.
            0xA870 | 0xA871 => {
                let mut s = Stack::new(regs);
                let _pt = s.pop_u32(mem);
                s.finish();
            }
            // PROCEDURE SetOrigin(h, v: INTEGER);
            //
            // Renumbers a port's coordinate system so drawing at `(h, v)` hits
            // the port's top-left pixel. Modules use it to draw in natural
            // coordinates; ignoring the call entirely left Clock rendering a
            // real clock face clipped into the screen's corner.
            //
            // Only the **boundary rectangle** is offset here, which is the part
            // that decides where pixels land: `Surface` maps a coordinate to a
            // pixel as `x - bounds.left`, so offsetting bounds *is* the
            // renumbering. Clock goes from 4,224 to 16,294 ink — a complete
            // face with numerals, tick marks and hands.
            //
            // Inside Macintosh also offsets `portRect` and `visRgn` (and
            // deliberately not `clipRgn`). Both are omitted, and that is a
            // measured decision rather than an oversight: modules read
            // `portRect` back to decide *where* to draw, and this runtime
            // advertises a single 640x480 display. Boris — which walks a cat in
            // from off-screen and is multi-monitor aware, calling
            // `GetMaxDevice` a hundred times a run — then computes positions
            // relative to the renumbered rect, lands permanently past the right
            // edge, and draws nothing at all instead of its usual 1,600 pixels.
            // Renumbering `portRect` is only meaningful once `MachineProfile`
            // carries more than one display; until then it converts a latent
            // multi-monitor assumption into a visible regression. Revisit with
            // multi-display support.
            0xA878 => {
                let mut s = Stack::new(regs);
                let v = s.pop_i16(mem);
                let h = s.pop_i16(mem);
                s.finish();
                let port = self.cur_port;
                if port == 0 {
                    return Ok(());
                }
                // The boundary rectangle is inline for a GrafPort and behind the
                // PixMapHandle for a CGrafPort. Reuse the blitter's own +4
                // marker rule rather than restating it, so the two can never
                // disagree about whether a port is colour.
                let bits_at = port.wrapping_add(crate::port::port::PORT_BITS);
                let marker = mem.read_u16(bits_at.wrapping_add(crate::blit::off::ROW_BYTES));
                let map_at = if marker & 0xC000 == 0xC000 {
                    let handle = mem.read_u32(bits_at.wrapping_add(crate::blit::off::BASE_ADDR));
                    mem.deref_handle(handle).unwrap_or(0)
                } else {
                    bits_at
                };
                if map_at == 0 {
                    return Ok(());
                }
                let b_at = map_at.wrapping_add(crate::blit::off::BOUNDS);
                let bounds = Rect::read(mem, b_at);
                // The delta is measured from `bounds`, not `portRect`, because
                // `bounds` is where this implementation *keeps* the origin.
                // Measuring from a `portRect` that is never updated made every
                // repeat call shift again: two SetOrigin(100, 50) calls landed
                // the origin at (200, 100). SetOrigin is idempotent on real
                // hardware and must be here.
                let dh = h.wrapping_sub(bounds.left);
                let dv = v.wrapping_sub(bounds.top);
                if dh == 0 && dv == 0 {
                    return Ok(());
                }
                bounds.offset(dh, dv).write(mem, b_at);
            }
            // PROCEDURE AddPt / SubPt(src: Point; VAR dst: Point);
            0xA87E | 0xA87F => {
                let mut s = Stack::new(regs);
                let dst = s.pop_u32(mem);
                let src = s.pop_u32(mem);
                let (sv, sh) = ((src >> 16) as i16, (src & 0xFFFF) as i16);
                let dv = mem.read_u16(dst) as i16;
                let dh = mem.read_u16(dst.wrapping_add(2)) as i16;
                let (nv, nh) = if t.canonical() == 0xA87E {
                    (dv.saturating_add(sv), dh.saturating_add(sh))
                } else {
                    (dv.saturating_sub(sv), dh.saturating_sub(sh))
                };
                mem.write_u16(dst, nv as u16);
                mem.write_u16(dst.wrapping_add(2), nh as u16);
                s.finish();
            }
            // PROCEDURE SetPt(VAR pt: Point; h, v: INTEGER);
            0xA880 => {
                let mut s = Stack::new(regs);
                let v = s.pop_i16(mem);
                let h = s.pop_i16(mem);
                let pt = s.pop_u32(mem);
                mem.write_u16(pt, v as u16);
                mem.write_u16(pt.wrapping_add(2), h as u16);
                s.finish();
            }
            // FUNCTION EqualPt(a, b: Point): BOOLEAN;
            0xA881 => {
                let mut s = Stack::new(regs);
                let b = s.pop_u32(mem);
                let a = s.pop_u32(mem);
s.finish_bool(mem, a == b);
            }
            // PROCEDURE MapPt(VAR pt: Point; srcRect, dstRect: Rect);
            0xA8F9 => {
                let mut s = Stack::new(regs);
                let dr = s.pop_u32(mem);
                let sr = s.pop_u32(mem);
                let pt = s.pop_u32(mem);
                let (src, dst) = (Rect::read(mem, sr), Rect::read(mem, dr));
                let v = mem.read_u16(pt) as i16;
                let h = mem.read_u16(pt.wrapping_add(2)) as i16;
                let mapped = map_rect(
                    &Rect::new(v, h, v.saturating_add(1), h.saturating_add(1)),
                    &src,
                    &dst,
                );
                mem.write_u16(pt, mapped.top as u16);
                mem.write_u16(pt.wrapping_add(2), mapped.left as u16);
                s.finish();
            }
            // PROCEDURE MapRgn(rgn: RgnHandle; srcRect, dstRect: Rect);
            0xA8FB => {
                let mut s = Stack::new(regs);
                let dr = s.pop_u32(mem);
                let sr = s.pop_u32(mem);
                let rgn = s.pop_u32(mem);
                let (src, dst) = (Rect::read(mem, sr), Rect::read(mem, dr));
                let out = map_rect(&Self::rgn_box(mem, rgn), &src, &dst);
                Self::set_rgn_box(mem, rgn, &out);
                s.finish();
            }
            // PROCEDURE CopyBits(src, dst: BitMap; srcRect, dstRect: Rect;
            //                    mode: INTEGER; maskRgn: RgnHandle);
            //
            // Only the screen-to-screen case is modelled, which is what modules
            // use it for: scrolling or duplicating part of the display.
            0xA8EC => {
                let mut s = Stack::new(regs);
                let mask_rgn = s.pop_u32(mem);
                let mode = s.pop_i16(mem);
                let dr = s.pop_u32(mem);
                let sr = s.pop_u32(mem);
                let dst_bits = s.pop_u32(mem);
                let src_bits = s.pop_u32(mem);
                let (src_rect, dst_rect) = (Rect::read(mem, sr), Rect::read(mem, dr));
                s.finish();

                // Resolve both arguments; either may be a BitMap, a PixMap or a
                // CGrafPort's portBits. Unusable shapes are skipped rather than
                // drawn as garbage.
                let (Some(src), Some(dst)) = (
                    crate::blit::Surface::resolve(mem, src_bits),
                    crate::blit::Surface::resolve(mem, dst_bits),
                ) else {
                    return Ok(());
                };
                let mask = if mask_rgn != 0 {
                    Some(Self::rgn_box(mem, mask_rgn))
                } else {
                    None
                };
                let palette = self.fb.palette.clone();
                let to_index = move |_m: &mut Memory, rgb: [u8; 3]| -> u8 {
                    let mut best = 0u8;
                    let mut best_d = i32::MAX;
                    for (i, c) in palette.iter().enumerate() {
                        let d = (i32::from(c[0]) - i32::from(rgb[0])).pow(2)
                            + (i32::from(c[1]) - i32::from(rgb[1])).pow(2)
                            + (i32::from(c[2]) - i32::from(rgb[2])).pow(2);
                        if d < best_d {
                            best_d = d;
                            best = u8::try_from(i).unwrap_or(0);
                        }
                    }
                    best
                };
                crate::blit::copy_bits(
                    mem,
                    &src,
                    &dst,
                    &src_rect,
                    &dst_rect,
                    mode,
                    self.fore,
                    self.back,
                    mask.as_ref(),
                    &to_index,
                );
            }
            // PROCEDURE ScrollRect(r: Rect; dh, dv: INTEGER; updateRgn: RgnHandle);
            0xA8EF => {
                let mut s = Stack::new(regs);
                let rgn = s.pop_u32(mem);
                let dv = s.pop_i16(mem);
                let dh = s.pop_i16(mem);
                let ra = s.pop_u32(mem);
                let r = Rect::read(mem, ra);
                let dstr = r.offset(dh, dv).intersect(&r);
                let srcr = dstr.offset(-dh, -dv);
                self.blit(mem, &srcr, &dstr);
                if rgn != 0 {
                    Self::set_rgn_box(mem, rgn, &r);
                }
                s.finish();
            }
            // Fixed-point and integer helpers. All documented exactly, so these
            // are bit-for-bit rather than approximations.
            0xA868 => {
                // FUNCTION FixMul(a, b: Fixed): Fixed;   16.16 multiply.
                let mut s = Stack::new(regs);
                let b = s.pop_u32(mem) as i32;
                let a = s.pop_u32(mem) as i32;
                let r = ((i64::from(a) * i64::from(b)) >> 16) as i32;
                s.finish_u32(mem, r as u32);
            }
            0xA869 => {
                // FUNCTION FixRatio(numer, denom: INTEGER): Fixed;
                let mut s = Stack::new(regs);
                let d = s.pop_i16(mem);
                let n = s.pop_i16(mem);
                let r = if d == 0 {
                    if n < 0 { i32::MIN } else { i32::MAX }
                } else {
                    ((i64::from(n) << 16) / i64::from(d)) as i32
                };
                s.finish_u32(mem, r as u32);
            }
            0xA84D => {
                // FUNCTION FixDiv(a, b: Fixed): Fixed;
                let mut s = Stack::new(regs);
                let b = s.pop_u32(mem) as i32;
                let a = s.pop_u32(mem) as i32;
                let r = if b == 0 {
                    if a < 0 { i32::MIN } else { i32::MAX }
                } else {
                    ((i64::from(a) << 16) / i64::from(b)) as i32
                };
                s.finish_u32(mem, r as u32);
            }
            0xA86C => {
                // FUNCTION FixRound(x: Fixed): INTEGER;  round half away from 0.
                let mut s = Stack::new(regs);
                let x = s.pop_u32(mem) as i32;
                let r = (i64::from(x) + 0x8000) >> 16;
                s.finish_u16(mem, i16::try_from(r).unwrap_or(i16::MAX) as u16);
            }
            0xA867 => {
                // FUNCTION LongMul(a, b: LONGINT; VAR result: Int64Bit);
                let mut s = Stack::new(regs);
                let out = s.pop_u32(mem);
                let b = s.pop_u32(mem) as i32;
                let a = s.pop_u32(mem) as i32;
                let p = i64::from(a) * i64::from(b);
                mem.write_u32(out, (p >> 32) as u32);
                mem.write_u32(out.wrapping_add(4), p as u32);
                s.finish();
            }
            0xA86A | 0xA86B => {
                // FUNCTION HiWord / LoWord(x: LONGINT): INTEGER;
                let mut s = Stack::new(regs);
                let x = s.pop_u32(mem);
                let w = if t.canonical() == 0xA86A {
                    (x >> 16) as u16
                } else {
                    x as u16
                };
                s.finish_u16(mem, w);
            }
            // Text: metrics answered from a fixed-width approximation and glyphs
            // not drawn. No After Dark 2.0x module on the disk depends on text for
            // its animation; Clock and Messages will need a real font later.
            // TextFont / TextFace / TextMode / TextSize — stored in the port,
            // where QuickDraw keeps them and where a port switch restores them.
            0xA887..=0xA88A => {
                let mut s = Stack::new(regs);
                let value = s.pop_i16(mem);
                s.finish();
                if self.cur_port != 0 {
                    let off = match t.canonical() {
                        0xA887 => crate::port::port::TX_FONT,
                        0xA888 => crate::port::port::TX_FONT.wrapping_add(2), // txFace
                        0xA889 => crate::port::port::TX_MODE,
                        _ => crate::port::port::TX_SIZE,
                    };
                    mem.write_u16(self.cur_port.wrapping_add(off), value as u16);
                }
            }
            0xA88E => {
                let mut s = Stack::new(regs);
                s.skip(4);
                s.finish();
            }
            0xA883 => {
                // PROCEDURE DrawChar(ch: CHAR); — a CHAR occupies a word, low byte.
                let mut s = Stack::new(regs);
                let ch = s.pop_u16(mem) as u8;
                s.finish();
                self.draw_text_bytes(mem, &[ch]);
            }
            0xA884 => {
                // PROCEDURE DrawString(s: Str255);
                //
                // 84 call sites across the disk — Life II alone has 46, Lunatic
                // Fringe 21 for its high-score table — and every one of them drew
                // nothing until there was a font to draw with.
                let mut s = Stack::new(regs);
                let p = s.pop_u32(mem);
                s.finish();
                let bytes = Self::pascal_bytes(mem, p);
                self.draw_text_bytes(mem, &bytes);
            }
            0xA885 => {
                // PROCEDURE DrawText(buf: Ptr; firstByte, byteCount: INTEGER);
                let mut s = Stack::new(regs);
                let count = s.pop_i16(mem);
                let first = s.pop_i16(mem);
                let p = s.pop_u32(mem);
                s.finish();
                if p != 0 && count > 0 {
                    let at = p.wrapping_add(u32::from(first.max(0) as u16));
                    let bytes = mem.read_bytes(at, count.max(0) as usize);
                    self.draw_text_bytes(mem, &bytes);
                }
            }
            0xA88C => {
                // FUNCTION StringWidth(s: Str255): INTEGER;
                let mut s = Stack::new(regs);
                let p = s.pop_u32(mem);
                let bytes = Self::pascal_bytes(mem, p);
                let w = self.measure(mem, &bytes);
                s.finish_u16(mem, w as u16);
            }
            0xA886 => {
                // FUNCTION TextWidth(buf: Ptr; firstByte, byteCount: INTEGER): INTEGER;
                let mut s = Stack::new(regs);
                let count = s.pop_i16(mem);
                let first = s.pop_i16(mem);
                let p = s.pop_u32(mem);
                let bytes = if p == 0 || count <= 0 {
                    Vec::new()
                } else {
                    let at = p.wrapping_add(u32::from(first.max(0) as u16));
                    mem.read_bytes(at, count as usize)
                };
                let w = self.measure(mem, &bytes);
                s.finish_u16(mem, w as u16);
            }
            0xA88D => {
                // FUNCTION CharWidth(ch: CHAR): INTEGER;
                let mut s = Stack::new(regs);
                let ch = s.pop_u16(mem) as u8;
                let w = self.measure(mem, &[ch]);
                s.finish_u16(mem, w as u16);
            }
            0xA88B => {
                // PROCEDURE GetFontInfo(VAR info: FontInfo);
                //
                // ascent, descent, widMax, leading — from the strike itself, so a
                // module laying out lines from these agrees with what gets drawn.
                let mut s = Stack::new(regs);
                let p = s.pop_u32(mem);
                let info = match self.current_font(mem) {
                    Some(f) => [f.ascent, f.descent, f.width_max, f.leading],
                    None => [10, 2, 6, 0],
                };
                for (i, v) in info.iter().enumerate() {
                    mem.write_u16(p.wrapping_add((i as u32) * 2), *v as u16);
                }
                s.finish();
            }
            0xA900 => {
                // PROCEDURE GetFNum(name: Str255; VAR num: INTEGER);
                let mut s = Stack::new(regs);
                let out = s.pop_u32(mem);
                let _name = s.pop_u32(mem);
                mem.write_u16(out, 0); // systemFont
                s.finish();
            }
            // Pen state records.
            0xA898 | 0xA899 => {
                let mut s = Stack::new(regs);
                let p = s.pop_u32(mem);
                if t.canonical() == 0xA898 {
                    // GetPenState: pnLoc, pnSize, pnMode, pnPat
                    mem.write_u16(p, self.pen_v as u16);
                    mem.write_u16(p.wrapping_add(2), self.pen_h as u16);
                    mem.write_u16(p.wrapping_add(4), 1);
                    mem.write_u16(p.wrapping_add(6), 1);
                    mem.write_u16(p.wrapping_add(8), 8);
                }
                s.finish();
            }
            0xA896 | 0xA897 => {
                // HidePen / ShowPen.
                Stack::new(regs).finish();
            }
            other => {
                return Err(format!(
                    "QuickDraw trap ${other:04X} is claimed but not implemented"
                ));
            }
        }
        Ok(())
    }

    /// Nearest palette index for an `RGBColor` in memory (three 16-bit channels).
    /// A closure that maps an RGB triple onto the current palette.
    ///
    /// Clones the palette so the result can be handed to the blitter while
    /// `self` stays borrowed for the destination surface.
    fn palette_mapper(&self) -> impl Fn(&mut Memory, [u8; 3]) -> u8 + use<> {
        let palette = self.fb.palette.clone();
        move |_m: &mut Memory, rgb: [u8; 3]| -> u8 { nearest_in(&palette, rgb) }
    }

    /// Draw a `cicn` through its mask, scaling into `dst_rect`.
    ///
    /// The resource is a fixed 82-byte header — `PixMap`(50), mask `BitMap`(14),
    /// b/w `BitMap`(14), `iconData` handle(4) — followed by the mask bits, the
    /// b/w bits, the colour table and finally the pixel data, each sized by the
    /// header it belongs to. Verified against Confetti Factory's 26 `cicn`s: the
    /// arithmetic lands exactly on the end of the resource.
    fn plot_cicon(&mut self, mem: &mut Memory, cicon: u32, dst_rect: &Rect) {
        let Some(base) = mem.deref_handle(cicon) else {
            return;
        };
        let pm_row = u32::from(mem.read_u16(base.wrapping_add(4)) & 0x3FFF);
        let pm_bounds = Rect::read(mem, base.wrapping_add(6));
        let pm_depth = mem.read_u16(base.wrapping_add(32));
        let mask_row = u32::from(mem.read_u16(base.wrapping_add(CICN_MASK + 4)));
        let mask_bounds = Rect::read(mem, base.wrapping_add(CICN_MASK + 6));
        let bw_row = u32::from(mem.read_u16(base.wrapping_add(CICN_BMAP + 4)));
        let bw_bounds = Rect::read(mem, base.wrapping_add(CICN_BMAP + 6));
        if pm_bounds.is_empty() || mask_bounds.is_empty() {
            return;
        }

        let mask_at = base.wrapping_add(CICN_HEADER);
        let bw_at = mask_at.wrapping_add(mask_row * mask_bounds.height().unsigned_abs());
        let ct_at = bw_at.wrapping_add(bw_row * bw_bounds.height().unsigned_abs());
        let ct_size = mem.read_u16(ct_at.wrapping_add(6)); // entries minus one
        let pix_at = ct_at.wrapping_add(CT_HEADER + (u32::from(ct_size) + 1) * CT_SPEC);

        let mask = crate::blit::Surface {
            base: mask_at,
            row_bytes: mask_row,
            bounds: mask_bounds,
            pixel_size: 1,
            color_table: 0,
        };
        // A depth the blitter cannot address means the colour half is unusable;
        // the mask still says which pixels belong to the icon, so fall back to
        // the b/w plane rather than drawing nothing.
        let colour = matches!(pm_depth, 1 | 2 | 4 | 8) && pm_row != 0;
        let src = if colour {
            crate::blit::Surface {
                base: pix_at,
                row_bytes: pm_row,
                bounds: pm_bounds,
                pixel_size: pm_depth,
                color_table: 0,
            }
        } else {
            crate::blit::Surface {
                base: bw_at,
                row_bytes: bw_row,
                bounds: bw_bounds,
                pixel_size: 1,
                color_table: 0,
            }
        };
        if src.row_bytes == 0 {
            return;
        }

        // Resolve the inline colour table once: it is part of the resource, not
        // a `CTabHandle`, so `Surface::rgb_of` cannot reach it.
        let mut lut = [0u8; 256];
        if colour {
            let palette = self.fb.palette.clone();
            let entries = usize::from(ct_size).min(255) + 1;
            for (i, slot) in lut.iter_mut().enumerate().take(entries) {
                let spec = ct_at.wrapping_add(CT_HEADER + (i as u32) * CT_SPEC);
                let rgb = [
                    (mem.read_u16(spec.wrapping_add(2)) >> 8) as u8,
                    (mem.read_u16(spec.wrapping_add(4)) >> 8) as u8,
                    (mem.read_u16(spec.wrapping_add(6)) >> 8) as u8,
                ];
                *slot = nearest_in(&palette, rgb);
            }
        }

        let dst = self.dest(mem);
        let (sw, sh) = (src.bounds.width().max(1), src.bounds.height().max(1));
        let (dw, dh) = (dst_rect.width(), dst_rect.height());
        for dy in 0..dh {
            let sy = dy * sh / dh;
            for dx in 0..dw {
                let sx = dx * sw / dw;
                let mx = i32::from(mask.bounds.left) + dx * mask.bounds.width().max(1) / dw;
                let my = i32::from(mask.bounds.top) + dy * mask.bounds.height().max(1) / dh;
                if mask.get(mem, mx, my).unwrap_or(0) == 0 {
                    continue; // masked out: the destination shows through
                }
                let Some(raw) = src.get(
                    mem,
                    i32::from(src.bounds.left) + sx,
                    i32::from(src.bounds.top) + sy,
                ) else {
                    continue;
                };
                let value = if colour {
                    lut[usize::from(raw)]
                } else if raw != 0 {
                    self.fore
                } else {
                    self.back
                };
                dst.set(
                    mem,
                    i32::from(dst_rect.left) + dx,
                    i32::from(dst_rect.top) + dy,
                    value,
                );
            }
        }
    }

    fn nearest_index(&self, mem: &mut Memory, addr: u32) -> u8 {
        // An installed `_AddSearch` procedure short-circuits colour matching:
        // the red word is the index the module wants. See the `$AA3A` arm.
        if self.search_proc.is_some() {
            return u8::try_from(mem.read_u16(addr).min(255)).unwrap_or(255);
        }
        let r = (mem.read_u16(addr) >> 8) as i32;
        let g = (mem.read_u16(addr.wrapping_add(2)) >> 8) as i32;
        let b = (mem.read_u16(addr.wrapping_add(4)) >> 8) as i32;
        let mut best = 0u8;
        let mut best_d = i32::MAX;
        for (i, c) in self.fb.palette.iter().enumerate() {
            let dr = i32::from(c[0]) - r;
            let dg = i32::from(c[1]) - g;
            let db = i32::from(c[2]) - b;
            let d = dr * dr + dg * dg + db * db;
            if d < best_d {
                best_d = d;
                best = u8::try_from(i).unwrap_or(0);
            }
        }
        best
    }
}

/// `_MapRect`: scale `r` from `src`'s space into `dst`'s.
#[must_use]
pub fn map_rect(r: &Rect, src: &Rect, dst: &Rect) -> Rect {
    let sw = src.width().max(1);
    let sh = src.height().max(1);
    let map_h = |h: i16| -> i16 {
        let rel = i32::from(h) - i32::from(src.left);
        let scaled = rel * dst.width() / sw;
        i16::try_from(i32::from(dst.left) + scaled).unwrap_or(i16::MAX)
    };
    let map_v = |v: i16| -> i16 {
        let rel = i32::from(v) - i32::from(src.top);
        let scaled = rel * dst.height() / sh;
        i16::try_from(i32::from(dst.top) + scaled).unwrap_or(i16::MAX)
    };
    Rect {
        top: map_v(r.top),
        left: map_h(r.left),
        bottom: map_v(r.bottom),
        right: map_h(r.right),
    }
}

/// Map a classic `ForeColor` constant to a palette index.
///
/// These eight constants predate Color QuickDraw and are still used by older
/// modules.
fn classic_colour_index(c: u32) -> u8 {
    match c {
        30 => 0,    // whiteColor
        33 => 255,  // blackColor
        205 => 210, // redColor
        341 => 145, // greenColor
        409 => 35,  // blueColor
        273 => 180, // cyanColor
        137 => 190, // magentaColor
        69 => 60,   // yellowColor
        _ => 255,
    }
}

/// Convert a 16-bit-per-component `HSVColor` to an `RGBColor`.
///
/// Hue spans the wheel over the full `u16` range, so `0x2AAA` is 60°.
#[must_use]
pub fn hsv_to_rgb(hsv: [u16; 3]) -> [u16; 3] {
    let (h, s, v) = (
        f64::from(hsv[0]) / 65536.0,
        f64::from(hsv[1]) / 65535.0,
        f64::from(hsv[2]) / 65535.0,
    );
    let sector = h * 6.0;
    let i = sector.floor();
    let f = sector - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "i is floor of a value in 0.0..6.0"
    )]
    let (r, g, b) = match (i as i32).rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "components are clamped to 0.0..1.0"
    )]
    let to16 = |x: f64| (x.clamp(0.0, 1.0) * 65535.0).round() as u16;
    [to16(r), to16(g), to16(b)]
}

/// Is this trap QuickDraw's to service?
#[must_use]
pub fn is_quickdraw(canonical: u16) -> bool {
    matches!(canonical,
        0xA8A1..=0xA8AE      // rect calls
        | 0xA8B0..=0xA8BB    // round rects and ovals
        | 0xA8BC             // SlopeFromAngle
        | 0xA8BE..=0xA8C2    // arcs
        | 0xA817             // CopyMask
        | 0xA94B | 0xAA1F    // PlotIcon, PlotCIcon
        | 0xAA07 | 0xAA08 | 0xAA0A | 0xAA0B | 0xAA0D // pixel patterns
        | 0xAA3A | 0xAA4C    // AddSearch, DelSearch
        | 0xA8C6..=0xA8CE    // polygons
        | 0xA8D2..=0xA8E9    // region calls
        | 0xA85D             // BitTst
        | 0xA82E             // Colour Utilities dispatch
        | 0xA891 | 0xA892 | 0xA893 | 0xA894 | 0xA89A  // pen movement
        | 0xA896 | 0xA897 | 0xA898 | 0xA899            // pen state records
        | 0xA870 | 0xA871 | 0xA878 | 0xA87E | 0xA87F | 0xA880 | 0xA881 // points
        | 0xA8EC | 0xA8EF                              // CopyBits, ScrollRect
        | 0xA84D | 0xA867 | 0xA868 | 0xA869 | 0xA86A | 0xA86B | 0xA86C // fixed math
        | 0xA883..=0xA88E | 0xA900                     // text
        | 0xA8F9 | 0xA8FB                              // MapPt, MapRgn
        | 0xA89B | 0xA89C | 0xA89D | 0xA89E           // pen state
        | 0xA87C | 0xA879 | 0xA87A | 0xA87B // clip state (SetPort/GetPort live in Toolbox)
        | 0xA862 | 0xA863 | 0xA864                    // classic colour
        | 0xA8FA                                      // MapRect
        | 0xAA14 | 0xAA15 | 0xAA16 | 0xAA17           // Color QuickDraw
        | 0xAA19 | 0xAA1A | 0xAA33 | 0xAA34 | 0xA865  // colour queries
    )
}
