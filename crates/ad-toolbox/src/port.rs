//! `GrafPort`, `GDevice` and `PixMap`.
//!
//! Most After Dark modules never draw a pixel until they have a port and a
//! device to look at. They read the port's `portRect` for screen bounds, the
//! device's `PixMap` for depth and base address, and then either call QuickDraw
//! or write straight to the bitmap. Without these they spin, which is why 39 of
//! the 66 modules on the disk hang rather than reporting a missing trap.
//!
//! # Why the offsets are shared
//!
//! `GrafPort` (black and white) and `CGrafPort` (Color QuickDraw) differ in their
//! first 16 bytes and diverge again around the pen patterns, but Apple
//! deliberately aligned the fields in between. `portRect`, `visRgn`, `clipRgn`,
//! `pnLoc`, `pnSize`, `pnMode`, `pnVis` and the whole text group sit at identical
//! offsets in both. A module compiled against either interface therefore reads
//! the right bytes, which is what makes one implementation serve both.

use ad_memory::Memory;

use crate::quickdraw::{Rect, SCREEN_HEIGHT, SCREEN_ROW_BYTES, SCREEN_WIDTH};

/// Field offsets common to `GrafPort` and `CGrafPort`.
#[allow(
    clippy::module_inception,
    reason = "these are the GrafPort field offsets; `port::port::PORT_RECT` reads correctly"
)]
pub mod port {
    /// `INTEGER device`
    pub const DEVICE: u32 = 0;
    /// `BitMap portBits` (mono) / `PixMapHandle portPixMap` (colour).
    pub const PORT_BITS: u32 = 2;
    /// `Rect portRect` — same offset in both layouts.
    pub const PORT_RECT: u32 = 16;
    /// `RgnHandle visRgn`
    pub const VIS_RGN: u32 = 24;
    /// `RgnHandle clipRgn`
    pub const CLIP_RGN: u32 = 28;
    /// `Pattern bkPat` (mono) / `PixPatHandle bkPixPat` (colour).
    pub const BK_PAT: u32 = 32;
    /// `Point pnLoc`
    pub const PN_LOC: u32 = 48;
    /// `Point pnSize`
    pub const PN_SIZE: u32 = 52;
    /// `INTEGER pnMode`
    pub const PN_MODE: u32 = 56;
    /// `INTEGER pnVis`
    pub const PN_VIS: u32 = 66;
    /// `INTEGER txFont`
    pub const TX_FONT: u32 = 68;
    /// `INTEGER txMode`
    pub const TX_MODE: u32 = 72;
    /// `INTEGER txSize`
    pub const TX_SIZE: u32 = 74;
    /// `LONGINT fgColor`
    pub const FG_COLOR: u32 = 80;
    /// `LONGINT bkColor`
    pub const BK_COLOR: u32 = 84;
    /// `INTEGER colrBit`
    pub const COLR_BIT: u32 = 88;
    /// `QDProcsPtr grafProcs`
    pub const GRAF_PROCS: u32 = 104;
    /// Size of a `GrafPort`. `CGrafPort` is the same size.
    pub const SIZE: u32 = 108;
}

/// `PixMap` field offsets.
pub mod pixmap {
    pub const BASE_ADDR: u32 = 0;
    /// `rowBytes`, with the high bit set to mark this as a `PixMap` rather than a
    /// `BitMap`. Code that tests bit 15 to tell them apart depends on it.
    pub const ROW_BYTES: u32 = 4;
    pub const BOUNDS: u32 = 6;
    pub const PM_VERSION: u32 = 14;
    pub const PACK_TYPE: u32 = 16;
    pub const PACK_SIZE: u32 = 18;
    pub const H_RES: u32 = 22;
    pub const V_RES: u32 = 26;
    pub const PIXEL_TYPE: u32 = 30;
    pub const PIXEL_SIZE: u32 = 32;
    pub const CMP_COUNT: u32 = 34;
    pub const CMP_SIZE: u32 = 36;
    pub const PLANE_BYTES: u32 = 38;
    pub const PM_TABLE: u32 = 42;
    pub const SIZE: u32 = 50;
}

/// `GDevice` field offsets.
pub mod gdevice {
    pub const GD_REF_NUM: u32 = 0;
    pub const GD_ID: u32 = 2;
    pub const GD_TYPE: u32 = 4;
    pub const GD_ITABLE: u32 = 6;
    pub const GD_RES_PREF: u32 = 10;
    pub const GD_SEARCH_PROC: u32 = 12;
    pub const GD_COMP_PROC: u32 = 16;
    pub const GD_FLAGS: u32 = 20;
    pub const GD_PMAP: u32 = 22;
    pub const GD_REF_CON: u32 = 26;
    pub const GD_NEXT_GD: u32 = 30;
    pub const GD_RECT: u32 = 34;
    pub const GD_MODE: u32 = 42;
    pub const GD_CC_BYTES: u32 = 46;
    pub const GD_CC_DEPTH: u32 = 48;
    pub const GD_CC_XDATA: u32 = 50;
    pub const SIZE: u32 = 54;
}

/// `gdFlags` bits that modules test through `_TestDeviceAttribute`.
pub mod gd_flags {
    /// Device is active.
    pub const ACTIVE: u16 = 1 << 15;
    /// Device has a colour lookup table (as opposed to direct colour).
    pub const CLUT_TYPE: u16 = 1 << 14;
    /// Main screen.
    pub const MAIN_SCREEN: u16 = 1 << 11;
    /// Screen device (rather than an offscreen `GWorld`).
    pub const SCREEN_DEVICE: u16 = 1 << 12;
    /// All bits a real active main colour screen would report.
    pub const MAIN_COLOUR_SCREEN: u16 = ACTIVE | CLUT_TYPE | MAIN_SCREEN | SCREEN_DEVICE;
}

/// Attribute selectors accepted by `_TestDeviceAttribute`.
pub mod device_attr {
    pub const GD_DEV_TYPE: i16 = 0;
    pub const BURST_DEVICE: i16 = 7;
    pub const EXT_32_DEVICE: i16 = 8;
    pub const RAM_INIT: i16 = 10;
    pub const MAIN_SCREEN: i16 = 11;
    pub const ALL_INIT: i16 = 12;
    pub const SCREEN_DEVICE: i16 = 13;
    pub const NO_DRIVER: i16 = 14;
    pub const SCREEN_ACTIVE: i16 = 15;
}

/// `ColorTable` field offsets, and the `ColorSpec` array that follows.
pub mod ctab {
    /// Changes whenever the table's contents change; code caches against it.
    pub const CT_SEED: u32 = 0;
    pub const CT_FLAGS: u32 = 4;
    /// **Entries minus one.** A 256-entry table stores 255 here, and reading it
    /// as a count is a classic off-by-one.
    pub const CT_SIZE: u32 = 6;
    /// First `ColorSpec`.
    pub const CT_TABLE: u32 = 8;
    /// `ColorSpec` = `value: word` then `rgb: RGBColor` (three words).
    pub const SPEC_SIZE: u32 = 8;

    /// Bytes needed for a table of `entries` colours.
    #[must_use]
    pub const fn size_for(entries: u32) -> u32 {
        CT_TABLE + entries * SPEC_SIZE
    }
}

/// Colours in the screen's table. An 8-bit indexed device has exactly this many.
pub const PALETTE_ENTRIES: u32 = 256;

/// First `ctSeed`. Any nonzero value does; zero is what an uninitialised table
/// reads as, so starting there would make "never set" and "set once"
/// indistinguishable to a module caching against the seed.
pub const INITIAL_CT_SEED: u32 = 1;

/// Fill a `ColorTable` from `palette`, padding with black if it is short.
///
/// Written in the Color Manager's own terms: `ctSize` is entries **minus one**,
/// each `ColorSpec` carries its own index in `value`, and the 8-bit channels are
/// promoted to 16 by replication (`0xAB` → `0xABAB`) rather than by shifting,
/// which is what keeps white at exactly `0xFFFF` instead of `0xFF00`.
pub fn write_color_table(mem: &mut Memory, handle: u32, palette: &[[u8; 3]], seed: u32) {
    let Some(base) = mem.deref_handle(handle) else {
        return;
    };
    mem.write_u32(base.wrapping_add(ctab::CT_SEED), seed);
    // ctFlags 0 marks a pixel map's table; 0x8000 would mark a device's.
    mem.write_u16(base.wrapping_add(ctab::CT_FLAGS), 0);
    let last = PALETTE_ENTRIES.saturating_sub(1);
    mem.write_u16(base.wrapping_add(ctab::CT_SIZE), last as u16);
    for i in 0..PALETTE_ENTRIES {
        let spec = base
            .wrapping_add(ctab::CT_TABLE)
            .wrapping_add(i.wrapping_mul(ctab::SPEC_SIZE));
        let [r, g, b] = palette.get(i as usize).copied().unwrap_or([0, 0, 0]);
        mem.write_u16(spec, i as u16);
        mem.write_u16(spec.wrapping_add(2), u16::from(r) << 8 | u16::from(r));
        mem.write_u16(spec.wrapping_add(4), u16::from(g) << 8 | u16::from(g));
        mem.write_u16(spec.wrapping_add(6), u16::from(b) << 8 | u16::from(b));
    }
}

/// The screen's port, pixel map and graphics device.
#[derive(Debug, Clone, Copy)]
pub struct Screen {
    /// A full-screen `GrafPort`, made current before the module runs — which is
    /// what After Dark itself did.
    pub port: u32,
    /// `PixMapHandle` describing the screen bitmap.
    pub pix_map: u32,
    /// `GDHandle` for the main device.
    pub device: u32,
    /// `CTabHandle` for the screen's palette, held by `pmTable`.
    ///
    /// This used to be nil, and a nil `pmTable` on a `clutType` device is not a
    /// simplification — it is a lie that reads as data. Supernova walks
    /// `GetMaxDevice()` → `gdPMap` → `pmTable` → `ctSize` and divides by a
    /// quarter of it, so a nil table dereferenced address zero, took an
    /// exception vector for a `ColorTable`, and divided by whatever word happened
    /// to sit six bytes in. An 8-bit indexed device has 256 colours and must say
    /// so.
    pub color_table: u32,
}

impl Screen {
    /// Build the port, pixel map and device for `screen_base`.
    ///
    /// `full_rgn` is a region covering the whole screen, reused for both `visRgn`
    /// and `clipRgn`.
    pub fn build(
        mem: &mut Memory,
        screen_base: u32,
        vis_rgn: u32,
        clip_rgn: u32,
        palette: &[[u8; 3]],
    ) -> Self {
        let bounds = Rect::new(0, 0, SCREEN_HEIGHT as i16, SCREEN_WIDTH as i16);

        // ---- the screen's ColorTable, which `pmTable` must point at ----
        let ct = mem.new_handle(ctab::size_for(PALETTE_ENTRIES), true);
        write_color_table(mem, ct, palette, INITIAL_CT_SEED);

        // ---- PixMap, behind a handle because that is what gdPMap holds ----
        let pm = mem.new_handle(pixmap::SIZE, true);
        let pmp = mem.deref_handle(pm).unwrap_or(0);
        mem.write_u32(pmp.wrapping_add(pixmap::BASE_ADDR), screen_base);
        // Bit 15 of rowBytes marks a PixMap; code that distinguishes PixMap from
        // BitMap tests exactly that bit.
        let row_bytes = u16::try_from(SCREEN_ROW_BYTES).unwrap_or(0) | 0x8000;
        mem.write_u16(pmp.wrapping_add(pixmap::ROW_BYTES), row_bytes);
        bounds.write(mem, pmp.wrapping_add(pixmap::BOUNDS));
        mem.write_u16(pmp.wrapping_add(pixmap::PM_VERSION), 0);
        mem.write_u16(pmp.wrapping_add(pixmap::PACK_TYPE), 0);
        mem.write_u32(pmp.wrapping_add(pixmap::PACK_SIZE), 0);
        // 72 dpi as Fixed (72 << 16).
        mem.write_u32(pmp.wrapping_add(pixmap::H_RES), 72 << 16);
        mem.write_u32(pmp.wrapping_add(pixmap::V_RES), 72 << 16);
        mem.write_u16(pmp.wrapping_add(pixmap::PIXEL_TYPE), 0); // chunky
        mem.write_u16(pmp.wrapping_add(pixmap::PIXEL_SIZE), 8);
        mem.write_u16(pmp.wrapping_add(pixmap::CMP_COUNT), 1);
        mem.write_u16(pmp.wrapping_add(pixmap::CMP_SIZE), 8);
        mem.write_u32(pmp.wrapping_add(pixmap::PLANE_BYTES), 0);
        mem.write_u32(pmp.wrapping_add(pixmap::PM_TABLE), ct);

        // ---- GDevice ----
        let gd = mem.new_handle(gdevice::SIZE, true);
        let gdp = mem.deref_handle(gd).unwrap_or(0);
        mem.write_u16(gdp.wrapping_add(gdevice::GD_REF_NUM), 0);
        mem.write_u16(gdp.wrapping_add(gdevice::GD_ID), 0);
        mem.write_u16(gdp.wrapping_add(gdevice::GD_TYPE), 2); // clutType
        mem.write_u32(gdp.wrapping_add(gdevice::GD_ITABLE), 0);
        mem.write_u16(gdp.wrapping_add(gdevice::GD_RES_PREF), 4);
        mem.write_u16(
            gdp.wrapping_add(gdevice::GD_FLAGS),
            gd_flags::MAIN_COLOUR_SCREEN,
        );
        mem.write_u32(gdp.wrapping_add(gdevice::GD_PMAP), pm);
        mem.write_u32(gdp.wrapping_add(gdevice::GD_REF_CON), 0);
        // Single monitor: the device list terminates here.
        mem.write_u32(gdp.wrapping_add(gdevice::GD_NEXT_GD), 0);
        bounds.write(mem, gdp.wrapping_add(gdevice::GD_RECT));
        mem.write_u32(gdp.wrapping_add(gdevice::GD_MODE), 0);

        // ---- the screen's GrafPort ----
        let port_addr = mem.reserve_host(port::SIZE, "screen GrafPort");
        Self::init_port(mem, port_addr, screen_base, pm, vis_rgn, clip_rgn);

        // Low-memory globals the Palette Manager and older code read directly.
        mem.write_u32(ad_memory::globals::MAIN_DEVICE, gd);
        mem.write_u32(ad_memory::globals::DEVICE_LIST, gd);
        mem.write_u32(ad_memory::globals::THE_GDEVICE, gd);

        Self {
            port: port_addr,
            pix_map: pm,
            device: gd,
            color_table: ct,
        }
    }

    /// Build a fresh `PixMap` describing the screen, behind its own handle.
    ///
    /// Every port needs its **own** PixMap. Sharing one handle across ports looks
    /// harmless until a module redirects a port's pixels to an offscreen buffer:
    /// with a shared PixMap that moves *every* port at once, so a sprite blit
    /// reads and writes the same memory and composites onto itself. That is why
    /// fifteen modules ran perfectly and drew nothing.
    pub fn new_screen_pixmap(mem: &mut Memory, screen_base: u32) -> u32 {
        let bounds = Rect::new(0, 0, SCREEN_HEIGHT as i16, SCREEN_WIDTH as i16);
        let pm = mem.new_handle(pixmap::SIZE, true);
        let pmp = mem.deref_handle(pm).unwrap_or(0);
        mem.write_u32(pmp.wrapping_add(pixmap::BASE_ADDR), screen_base);
        let row_bytes = u16::try_from(SCREEN_ROW_BYTES).unwrap_or(0) | 0x8000;
        mem.write_u16(pmp.wrapping_add(pixmap::ROW_BYTES), row_bytes);
        bounds.write(mem, pmp.wrapping_add(pixmap::BOUNDS));
        mem.write_u32(pmp.wrapping_add(pixmap::H_RES), 72 << 16);
        mem.write_u32(pmp.wrapping_add(pixmap::V_RES), 72 << 16);
        mem.write_u16(pmp.wrapping_add(pixmap::PIXEL_TYPE), 0);
        mem.write_u16(pmp.wrapping_add(pixmap::PIXEL_SIZE), 8);
        mem.write_u16(pmp.wrapping_add(pixmap::CMP_COUNT), 1);
        mem.write_u16(pmp.wrapping_add(pixmap::CMP_SIZE), 8);
        pm
    }

    /// Initialise a port in place, as `_OpenPort` / `_OpenCPort` would.
    ///
    /// `pix_map` is the port's own `PixMapHandle` — see [`Self::new_screen_pixmap`].
    pub fn init_port(
        mem: &mut Memory,
        addr: u32,
        screen_base: u32,
        pix_map: u32,
        vis_rgn: u32,
        clip_rgn: u32,
    ) {
        let bounds = Rect::new(0, 0, SCREEN_HEIGHT as i16, SCREEN_WIDTH as i16);
        for i in 0..port::SIZE {
            mem.write_u8(addr.wrapping_add(i), 0);
        }
        mem.write_u16(addr.wrapping_add(port::DEVICE), 0);
        // A CGrafPort holds a PixMapHandle here plus portVersion with the high two
        // bits set, which is how Color QuickDraw recognises a colour port. A mono
        // GrafPort would hold a BitMap instead; the fields that matter to modules
        // are the aligned ones further in.
        mem.write_u32(addr.wrapping_add(port::PORT_BITS), pix_map);
        mem.write_u16(addr.wrapping_add(6), 0xC000); // portVersion
        bounds.write(mem, addr.wrapping_add(port::PORT_RECT));
        mem.write_u32(addr.wrapping_add(port::VIS_RGN), vis_rgn);
        mem.write_u32(addr.wrapping_add(port::CLIP_RGN), clip_rgn);
        // pnLoc (0,0), pnSize (1,1), pnMode patCopy, pen visible.
        mem.write_u32(addr.wrapping_add(port::PN_LOC), 0);
        mem.write_u16(addr.wrapping_add(port::PN_SIZE), 1); // v
        mem.write_u16(addr.wrapping_add(port::PN_SIZE + 2), 1); // h
        mem.write_u16(addr.wrapping_add(port::PN_MODE), 8); // patCopy
        mem.write_u16(addr.wrapping_add(port::PN_VIS), 0);
        mem.write_u16(addr.wrapping_add(port::TX_FONT), 0);
        mem.write_u16(addr.wrapping_add(port::TX_MODE), 0); // srcOr
        mem.write_u16(addr.wrapping_add(port::TX_SIZE), 12);
        // QuickDraw's own defaults: black ink on a white background — for every
        // port, the screen's included. This field was inverted twice in this
        // project's history (fore white on black, then black on black), each
        // time tuned so the era's set of modules "looked right". The real
        // defect was never here: the param block's QD globals copy shipped
        // with all five patterns zeroed, so the canonical SDK blank —
        // `FillRgn(blankRgn, qdGlobalsCopy->qdBlack)` — filled with the white
        // pattern and the wrong colour scheme became whatever made that
        // accident photogenic. Lunatic Fringe pins the ink itself: its loading
        // screen paints `blankRgn` with the untouched default pen and draws
        // light-grey text over the result, so the default ink must be black.
        mem.write_u32(addr.wrapping_add(port::FG_COLOR), 33); // blackColor
        mem.write_u32(addr.wrapping_add(port::BK_COLOR), 30); // whiteColor
        mem.write_u16(addr.wrapping_add(port::COLR_BIT), 0);
        mem.write_u32(addr.wrapping_add(port::GRAF_PROCS), 0);
        let _ = screen_base;
    }

    /// Fill a `QDGlobals` record, as `_InitGraf` does.
    ///
    /// `global_ptr` is what the module passed — a pointer to its own `thePort`
    /// field, with the rest of the record laid out after it.
    pub fn init_graf(&self, mem: &mut Memory, global_ptr: u32, screen_base: u32, seed: u32) {
        /// Offsets within `QDGlobals`, from `GraphicsModule_Types.h`.
        const THE_PORT: u32 = 0;
        const WHITE: u32 = 4;
        const BLACK: u32 = 12;
        const GRAY: u32 = 20;
        const LT_GRAY: u32 = 28;
        const DK_GRAY: u32 = 36;
        const ARROW: u32 = 44;
        const SCREEN_BITS: u32 = 112;
        const RAND_SEED: u32 = 126;

        mem.write_u32(global_ptr.wrapping_add(THE_PORT), self.port);

        // Standard patterns, as 8 rows of 8 bits.
        let pats: [(u32, [u8; 8]); 5] = [
            (WHITE, [0x00; 8]),
            (BLACK, [0xFF; 8]),
            (GRAY, [0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55]),
            (LT_GRAY, [0x88, 0x22, 0x88, 0x22, 0x88, 0x22, 0x88, 0x22]),
            (DK_GRAY, [0x77, 0xDD, 0x77, 0xDD, 0x77, 0xDD, 0x77, 0xDD]),
        ];
        for (off, rows) in pats {
            mem.write_bytes(global_ptr.wrapping_add(off), &rows);
        }
        // The arrow cursor: 32 bytes data, 32 bytes mask, then the hot spot. Zeroed
        // is a valid (invisible) cursor and nothing in a screen saver draws it.
        for i in 0..68u32 {
            mem.write_u8(global_ptr.wrapping_add(ARROW).wrapping_add(i), 0);
        }
        // screenBits: the BitMap modules read to find the screen.
        mem.write_u32(global_ptr.wrapping_add(SCREEN_BITS), screen_base);
        mem.write_u16(
            global_ptr.wrapping_add(SCREEN_BITS).wrapping_add(4),
            u16::try_from(SCREEN_ROW_BYTES).unwrap_or(0),
        );
        Rect::new(0, 0, SCREEN_HEIGHT as i16, SCREEN_WIDTH as i16)
            .write(mem, global_ptr.wrapping_add(SCREEN_BITS).wrapping_add(6));
        mem.write_u32(global_ptr.wrapping_add(RAND_SEED), seed);
    }
}
