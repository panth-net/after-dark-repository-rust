//! A drawing surface for the launcher's own interface.
//!
//! Deliberately **not** QuickDraw. The module's screen is an 8-bit indexed
//! framebuffer whose palette the module owns and rewrites; the launcher's chrome
//! must not compete for those 256 entries, and it has no business going through
//! an emulated Toolbox to draw a list box. So this is a plain `0RGB` canvas that
//! shares exactly one thing with the emulator: the font.
//!
//! # The font is the real thing
//!
//! Text is drawn from a genuine Macintosh `FONT`/`NFNT` strike out of the user's
//! own System file, decoded by [`ad_resource::font`]. That is why the launcher
//! looks like the control panel it replaces rather than like a modern dialog, and
//! it is the same decoder the text-drawing modules will use.

use ad_resource::{font::font_id_parts, BitmapFont, ResourceFork};
use std::path::Path;

/// A font that owns its resource bytes.
///
/// [`BitmapFont`] borrows its strike, so something has to hold the resource.
/// Re-parsing per use is free — it is a handful of bounds-checked header reads —
/// and it keeps this a plain owned value instead of a self-referential struct.
#[derive(Debug)]
pub struct Font {
    bytes: Vec<u8>,
    /// Where it came from, for the startup line.
    pub origin: String,
}

impl Font {
    /// The decoded strike.
    ///
    /// # Panics
    /// Never: [`Font::discover`] only keeps bytes that already parsed.
    #[must_use]
    pub fn strike(&self) -> BitmapFont<'_> {
        BitmapFont::parse(&self.bytes).unwrap_or_else(|_| {
            unreachable!("Font::discover only stores strikes that parsed")
        })
    }

    /// Find a usable font beside the modules.
    ///
    /// Prefers Geneva 12 — family 3, size 12, the classic list font, and the
    /// largest strike the System file carries. Falls back to any other `FONT`
    /// strike, then to Chicago's `NFNT`.
    ///
    /// Returns `None` when the user has no font files, which is a real
    /// possibility and not a failure: nothing may be **bundled**, because these
    /// are Apple's fonts from the user's own disk, so the launcher degrades to a
    /// terminal listing rather than shipping a substitute.
    #[must_use]
    pub fn discover(dir: &Path) -> Option<Self> {
        let mut best: Option<(i32, Vec<u8>, String)> = None;
        for (file, ty) in [("System.rsrc", b"FONT"), ("Chicago.rsrc", b"NFNT")] {
            let Ok(bytes) = std::fs::read(dir.join(file)) else {
                continue;
            };
            let Ok(fork) = ResourceFork::parse(&bytes) else {
                continue;
            };
            for r in fork.all() {
                if &r.res_type != ty {
                    continue;
                }
                let (family, size) = font_id_parts(r.id);
                // A FONT id that is a multiple of 128 is a family *name* record
                // with no strike; NFNT ids carry no such meaning, so only screen
                // out the ones that actually fail to parse.
                if ty == b"FONT" && size == 0 {
                    continue;
                }
                let Ok(strike) = BitmapFont::parse(r.data) else {
                    continue;
                };
                // Score: Geneva 12 wins outright, then bigger is better up to a
                // readable ceiling, then anything at all.
                let score = if (family, size) == (3, 12) {
                    1000
                } else {
                    i32::from(strike.rect_height.min(16))
                };
                let origin = format!("{file} {} {}", String::from_utf8_lossy(ty), r.id);
                if best.as_ref().is_none_or(|(s, _, _)| score > *s) {
                    best = Some((score, r.data.to_vec(), origin));
                }
            }
        }
        best.map(|(_, bytes, origin)| Self { bytes, origin })
    }
}

/// Palette for the launcher's chrome. After Dark's own control panel was a grey
/// System 7 dialog; this is that, not a dark theme.
pub mod colour {
    pub const BACKGROUND: u32 = 0x00DD_DDDD;
    pub const PANEL: u32 = 0x00EE_EEEE;
    pub const INK: u32 = 0x0000_0000;
    pub const DIM: u32 = 0x0077_7777;
    pub const FRAME: u32 = 0x0044_4444;
    pub const SELECTED: u32 = 0x0000_0066;
    pub const SELECTED_INK: u32 = 0x00FF_FFFF;
    pub const HEADING: u32 = 0x0000_0000;
}

/// A `0RGB` drawing surface the window can display directly.
#[derive(Debug)]
pub struct Canvas {
    pub px: Vec<u32>,
    pub w: usize,
    pub h: usize,
}

impl Canvas {
    #[must_use]
    pub fn new(w: usize, h: usize) -> Self {
        Self {
            px: vec![colour::BACKGROUND; w * h],
            w,
            h,
        }
    }

    pub fn clear(&mut self, c: u32) {
        self.px.fill(c);
    }

    /// Filled rectangle, clipped to the canvas.
    pub fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: u32) {
        for row in y..y.saturating_add(h) {
            for col in x..x.saturating_add(w) {
                self.set(col, row, c);
            }
        }
    }

    /// One-pixel outline.
    pub fn frame(&mut self, x: i32, y: i32, w: i32, h: i32, c: u32) {
        let (right, bottom) = (x.saturating_add(w).saturating_sub(1), y.saturating_add(h).saturating_sub(1));
        for col in x..=right {
            self.set(col, y, c);
            self.set(col, bottom, c);
        }
        for row in y..=bottom {
            self.set(x, row, c);
            self.set(right, row, c);
        }
    }

    fn set(&mut self, x: i32, y: i32, c: u32) {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
            return;
        };
        if x >= self.w || y >= self.h {
            return;
        }
        if let Some(p) = self.px.get_mut(y * self.w + x) {
            *p = c;
        }
    }

    /// Draw `text` with its **baseline** at `y`, returning the pen's end position.
    ///
    /// Baseline rather than top edge, because that is the coordinate the font
    /// itself is described in — `ascent` rows above, `descent` below — and mixing
    /// the two is how text ends up a few pixels out of line with everything else.
    pub fn text(&mut self, font: &BitmapFont<'_>, x: i32, y: i32, text: &str, c: u32) -> i32 {
        let mut pen = x;
        for ch in ad_resource::macroman::encode(text) {
            let Some(g) = font.glyph(ch) else { continue };
            for bit in 0..g.bits {
                for row in 0..font.rect_height {
                    if font.strike_bit(g.strike_bit.saturating_add(bit), row) {
                        self.set(
                            pen.saturating_add(i32::from(g.left)).saturating_add(i32::from(bit)),
                            y.saturating_sub(i32::from(font.ascent)).saturating_add(i32::from(row)),
                            c,
                        );
                    }
                }
            }
            pen = pen.saturating_add(i32::from(g.advance));
        }
        pen
    }

    /// Draw `text` truncated with an ellipsis to fit `max_width`.
    pub fn text_clipped(
        &mut self,
        font: &BitmapFont<'_>,
        x: i32,
        y: i32,
        text: &str,
        max_width: i32,
        c: u32,
    ) {
        let bytes = ad_resource::macroman::encode(text);
        if font.text_width(&bytes) <= max_width {
            self.text(font, x, y, text, c);
            return;
        }
        // Trim by *character*, not by byte, so a multi-byte name is not cut in
        // half — MacRoman is single-byte, but the source string is UTF-8.
        let ellipsis_w = font.text_width(b"...");
        let mut keep = String::new();
        for ch in text.chars() {
            let mut trial = keep.clone();
            trial.push(ch);
            if font.text_width(&ad_resource::macroman::encode(&trial))
                > max_width.saturating_sub(ellipsis_w)
            {
                break;
            }
            keep = trial;
        }
        keep.push_str("...");
        self.text(font, x, y, &keep, c);
    }
}
