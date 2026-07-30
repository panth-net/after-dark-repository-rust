//! Classic Macintosh bitmap fonts: `FONT` and `NFNT`.
//!
//! Format reference: *Inside Macintosh: Text*, "Font Resources".
//!
//! ```text
//! +0   i16 fontType     $9000 proportional, bit 0 set = fixed width
//! +2   i16 firstChar    first character code in the strike
//! +4   i16 lastChar     last character code
//! +6   i16 widMax       widest advance
//! +8   i16 kernMax      maximum *negative* kern, so <= 0
//! +10  i16 nDescent     negative of descent (or the strike's high word, unused)
//! +12  i16 fRectWidth   bounding box of the widest glyph
//! +14  i16 fRectHeight  height of the strike, and of every glyph
//! +16  i16 owTLoc       words from THIS FIELD to the offset/width table
//! +18  i16 ascent
//! +20  i16 descent
//! +22  i16 leading
//! +24  i16 rowWords     strike width in words
//! +26       bitImage    rowWords*2 * fRectHeight bytes: every glyph, side by side
//!           locTable    (lastChar-firstChar+3) i16: bit offset of each glyph
//!           owTable     (lastChar-firstChar+3) i16: high byte offset, low byte advance
//! ```
//!
//! `owTLoc` is measured from its own address, which is what makes it a check
//! rather than a hint: it must resolve to exactly where the location table ends.
//! It does, on all three fonts in the System file on the source disk —
//! Geneva 9, Geneva 12 and Monaco 9 — which is how this reading was confirmed
//! before a single glyph was drawn.
//!
//! # Why this crate
//!
//! A font strike is bytes from an untrusted file turned into a structure, which
//! is exactly this crate's job: borrowed, bounds-checked, no panics. Drawing
//! belongs to QuickDraw, and nothing here knows what a pixel is.

use crate::error::{Error, Result};

/// Bytes of font header before the strike.
const HEADER_LEN: usize = 26;

/// One glyph's placement, in strike coordinates and pen coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyph {
    /// First bit column of this glyph within the strike.
    pub strike_bit: u16,
    /// Width of the glyph in the strike, in bits. May be 0 (a space).
    pub bits: u16,
    /// Pen-relative left edge, already including `kernMax`.
    pub left: i16,
    /// How far to advance the pen after drawing.
    pub advance: i16,
}

/// A decoded bitmap font, borrowing its strike from the resource.
#[derive(Debug, Clone, Copy)]
pub struct BitmapFont<'a> {
    pub first_char: u8,
    pub last_char: u8,
    /// Widest advance in the font.
    pub width_max: i16,
    pub kern_max: i16,
    pub rect_width: i16,
    pub rect_height: i16,
    pub ascent: i16,
    pub descent: i16,
    pub leading: i16,
    /// True when every glyph advances by the same amount.
    pub fixed_width: bool,
    row_bytes: usize,
    strike: &'a [u8],
    loc: &'a [u8],
    ow: &'a [u8],
}

fn be_i16(bytes: &[u8], at: usize) -> Option<i16> {
    let end = at.checked_add(2)?;
    Some(i16::from_be_bytes(
        <[u8; 2]>::try_from(bytes.get(at..end)?).ok()?,
    ))
}

fn oob(what: &'static str, offset: usize, need: usize, len: usize) -> Error {
    Error::OutOfBounds {
        what,
        offset,
        need,
        fork_len: len,
    }
}

impl<'a> BitmapFont<'a> {
    /// Parse a `FONT` or `NFNT` resource. The two types share one format.
    ///
    /// # Errors
    /// [`Error::OutOfBounds`] when any table runs past the resource, or
    /// [`Error::TooShort`] for a resource smaller than the header. A `FONT`
    /// whose id is a multiple of 128 is a *family name* record with no strike at
    /// all, and is rejected here rather than half-decoded.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let len = data.len();
        if len < HEADER_LEN {
            return Err(Error::TooShort { len });
        }
        let field = |at: usize, what: &'static str| {
            be_i16(data, at).ok_or_else(|| oob(what, at, 2, len))
        };
        let font_type = field(0, "fontType")?;
        let first = field(2, "firstChar")?;
        let last = field(4, "lastChar")?;
        let width_max = field(6, "widMax")?;
        let kern_max = field(8, "kernMax")?;
        let rect_width = field(12, "fRectWidth")?;
        let rect_height = field(14, "fRectHeight")?;
        let ow_t_loc = field(16, "owTLoc")?;
        let ascent = field(18, "ascent")?;
        let descent = field(20, "descent")?;
        let leading = field(22, "leading")?;
        let row_words = field(24, "rowWords")?;

        // Character codes are bytes; a strike claiming otherwise cannot be
        // indexed by one and is malformed rather than merely unusual.
        let (first_char, last_char) = match (u8::try_from(first), u8::try_from(last)) {
            (Ok(f), Ok(l)) if f <= l => (f, l),
            _ => return Err(oob("firstChar/lastChar", 2, 4, len)),
        };
        if row_words <= 0 || rect_height <= 0 {
            return Err(oob("rowWords/fRectHeight", 24, 2, len));
        }
        let row_bytes = (row_words as usize).saturating_mul(2);
        let strike_len = row_bytes.saturating_mul(rect_height as usize);
        let strike_end = HEADER_LEN.saturating_add(strike_len);
        let strike = data
            .get(HEADER_LEN..strike_end)
            .ok_or_else(|| oob("bitImage", HEADER_LEN, strike_len, len))?;

        // The location table carries `lastChar - firstChar + 3` entries: one per
        // character, one for the missing-character symbol, and one sentinel that
        // gives the last real glyph its width.
        let chars = usize::from(last_char.saturating_sub(first_char)).saturating_add(1);
        let loc_entries = chars.saturating_add(2);
        let loc_len = loc_entries.saturating_mul(2);
        let loc_end = strike_end.saturating_add(loc_len);
        let loc = data
            .get(strike_end..loc_end)
            .ok_or_else(|| oob("locTable", strike_end, loc_len, len))?;

        // `owTLoc` counts words from its own address at +16. Checking it rather
        // than trusting the computed position is what makes a font whose header
        // disagrees with its own layout an error instead of a silent misread.
        let ow_start = 16usize.saturating_add((ow_t_loc.max(0) as usize).saturating_mul(2));
        if ow_start != loc_end {
            return Err(oob("owTLoc", 16, loc_len, len));
        }

        // The offset/width table is documented as the same length — a word per
        // character, one for the missing symbol, and a `$FFFF` terminator — but
        // **two of the three real strikes in the System file end without that
        // terminator**: FONT 393 and 396 are each exactly two bytes short of the
        // documented size. Nothing reads the terminator, so requiring it would
        // reject Geneva 9 and Geneva 12 to satisfy a word no code looks at.
        //
        // Only the entries a glyph lookup can actually reach are required: index
        // 0 through the missing symbol at `chars`, so `chars + 1` words. Anything
        // beyond that is taken if present and ignored if not.
        let ow_needed = chars.saturating_add(1).saturating_mul(2);
        let ow_end = ow_start
            .saturating_add(loc_len)
            .min(len);
        let ow = data
            .get(ow_start..ow_end)
            .filter(|t| t.len() >= ow_needed)
            .ok_or_else(|| oob("owTable", ow_start, ow_needed, len))?;

        Ok(Self {
            first_char,
            last_char,
            width_max,
            kern_max,
            rect_width,
            rect_height,
            ascent,
            descent,
            leading,
            fixed_width: font_type & 0x0001 != 0,
            row_bytes,
            strike,
            loc,
            ow,
        })
    }

    /// Line height: ascent plus descent plus leading.
    #[must_use]
    pub fn line_height(&self) -> i16 {
        self.ascent
            .saturating_add(self.descent)
            .saturating_add(self.leading)
    }

    fn table_i16(table: &[u8], index: usize) -> Option<i16> {
        be_i16(table, index.checked_mul(2)?)
    }

    /// Where `ch` sits in the strike, and how far it advances the pen.
    ///
    /// A character outside the strike, or one whose offset/width entry is `-1`,
    /// resolves to the font's **missing-character symbol** — which is what the
    /// Font Manager does, and is why a module drawing an accented character gets
    /// a visible box rather than nothing.
    #[must_use]
    pub fn glyph(&self, ch: u8) -> Option<Glyph> {
        let index = self.index_of(ch)?;
        let start = Self::table_i16(self.loc, index)?;
        let end = Self::table_i16(self.loc, index.saturating_add(1))?;
        let entry = Self::table_i16(self.ow, index)?;
        let bits = u16::try_from(i32::from(end).checked_sub(i32::from(start))?).ok()?;
        // High byte is the pen-relative offset, low byte the advance. Both are
        // unsigned; `kernMax` (never positive) supplies the leftward shift.
        let value = entry as u16;
        let offset = i16::from((value >> 8) as u8);
        let advance = i16::from((value & 0x00FF) as u8);
        Some(Glyph {
            strike_bit: u16::try_from(start).ok()?,
            bits,
            left: self.kern_max.saturating_add(offset),
            advance,
        })
    }

    /// Table index for `ch`, falling back to the missing-character symbol.
    fn index_of(&self, ch: u8) -> Option<usize> {
        let last_index = usize::from(self.last_char.saturating_sub(self.first_char));
        let missing = last_index.saturating_add(1);
        if ch < self.first_char || ch > self.last_char {
            return Some(missing);
        }
        let index = usize::from(ch.saturating_sub(self.first_char));
        // -1 means "this code has no glyph"; the Font Manager substitutes the
        // missing symbol rather than drawing nothing.
        match Self::table_i16(self.ow, index) {
            Some(-1) | None => Some(missing),
            Some(_) => Some(index),
        }
    }

    /// Is the strike bit at `(x, y)` set? `x` is a bit column, `y` a row.
    #[must_use]
    pub fn strike_bit(&self, x: u16, y: i16) -> bool {
        if y < 0 || y >= self.rect_height {
            return false;
        }
        let byte_index = (y as usize)
            .saturating_mul(self.row_bytes)
            .saturating_add(usize::from(x) / 8);
        match self.strike.get(byte_index) {
            // Bit 7 is the leftmost pixel, as everywhere else in QuickDraw.
            Some(&b) => b & (0x80 >> (usize::from(x) % 8)) != 0,
            None => false,
        }
    }

    /// Advance width of one character, `_CharWidth`.
    #[must_use]
    pub fn char_width(&self, ch: u8) -> i16 {
        self.glyph(ch).map_or(0, |g| g.advance)
    }

    /// Advance width of a run, `_TextWidth` / `_StringWidth`.
    #[must_use]
    pub fn text_width(&self, bytes: &[u8]) -> i32 {
        bytes
            .iter()
            .map(|&c| i32::from(self.char_width(c)))
            .fold(0i32, i32::saturating_add)
    }
}

/// Family and size encoded in a `FONT` resource id.
///
/// `id = family * 128 + size`, and a size of 0 means the resource is the
/// family's *name* record rather than a strike. `NFNT` ids carry no such
/// meaning — a `FOND` maps them — so this is only for `FONT`.
#[must_use]
pub fn font_id_parts(id: i16) -> (i16, i16) {
    (id / 128, id % 128)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    /// Build a two-character font: 'A' is a solid 3x2 block, 'B' is empty.
    /// Deliberately hand-assembled so the header arithmetic is the thing under
    /// test rather than a real font's self-consistency.
    fn tiny_font() -> Vec<u8> {
        let first: i16 = b'A' as i16;
        let last: i16 = b'B' as i16;
        let entries = (last - first + 3) as usize; // 4
        let row_words: i16 = 1;
        let rect_height: i16 = 2;
        let strike_len = (row_words as usize) * 2 * (rect_height as usize);
        // owTLoc counts words from offset 16 to the start of the owTable.
        let ow_start = HEADER_LEN + strike_len + entries * 2;
        let ow_t_loc = ((ow_start - 16) / 2) as i16;

        let mut d = Vec::new();
        let mut w = |v: i16| d.extend_from_slice(&v.to_be_bytes());
        w(0x9000u16 as i16); // fontType: proportional
        w(first);
        w(last);
        w(3); // widMax
        w(0); // kernMax
        w(-2); // nDescent
        w(3); // fRectWidth
        w(rect_height);
        w(ow_t_loc);
        w(2); // ascent
        w(0); // descent
        w(0); // leading
        w(row_words);
        assert_eq!(d.len(), HEADER_LEN);

        // Strike: 'A' occupies bits 0..3 and is solid; 'B' occupies 3..3.
        // Row 0 and row 1 both 1110_0000.
        d.extend_from_slice(&[0b1110_0000, 0x00]);
        d.extend_from_slice(&[0b1110_0000, 0x00]);
        assert_eq!(d.len(), HEADER_LEN + strike_len);

        // locTable: A starts at 0, B at 3, missing at 3, sentinel at 3.
        for v in [0i16, 3, 3, 3] {
            d.extend_from_slice(&v.to_be_bytes());
        }
        // owTable: A offset 0 advance 4; B offset 0 advance 2; missing; sentinel.
        for v in [0x0004i16, 0x0002, 0x0000, 0x0000] {
            d.extend_from_slice(&v.to_be_bytes());
        }
        assert_eq!(d.len(), ow_start + entries * 2);
        d
    }

    #[test]
    fn decodes_a_hand_assembled_font() {
        let d = tiny_font();
        let f = BitmapFont::parse(&d).expect("parse");
        assert_eq!((f.first_char, f.last_char), (b'A', b'B'));
        assert_eq!(f.ascent, 2);
        assert_eq!(f.rect_height, 2);
        assert_eq!(f.line_height(), 2);
        assert!(!f.fixed_width);
    }

    #[test]
    fn glyph_geometry_and_advance() {
        let d = tiny_font();
        let f = BitmapFont::parse(&d).expect("parse");
        let a = f.glyph(b'A').expect("A");
        assert_eq!(a.strike_bit, 0);
        assert_eq!(a.bits, 3);
        assert_eq!(a.advance, 4);
        assert_eq!(a.left, 0);
        let b = f.glyph(b'B').expect("B");
        assert_eq!(b.bits, 0, "B has no ink");
        assert_eq!(b.advance, 2);
    }

    #[test]
    fn strike_bits_read_most_significant_first() {
        let d = tiny_font();
        let f = BitmapFont::parse(&d).expect("parse");
        for y in 0..2 {
            assert!(f.strike_bit(0, y) && f.strike_bit(1, y) && f.strike_bit(2, y));
            assert!(!f.strike_bit(3, y), "bit 3 is clear");
        }
        assert!(!f.strike_bit(0, -1), "above the strike");
        assert!(!f.strike_bit(0, 2), "below the strike");
        assert!(!f.strike_bit(9999, 0), "past the row");
    }

    #[test]
    fn text_width_sums_advances() {
        let d = tiny_font();
        let f = BitmapFont::parse(&d).expect("parse");
        assert_eq!(f.char_width(b'A'), 4);
        assert_eq!(f.text_width(b"AAB"), 10);
    }

    #[test]
    fn a_character_outside_the_strike_becomes_the_missing_symbol() {
        let d = tiny_font();
        let f = BitmapFont::parse(&d).expect("parse");
        // 'Z' is past lastChar; it must resolve, not vanish, so a module drawing
        // an accented character gets a visible box instead of silence.
        let z = f.glyph(b'Z').expect("Z resolves to the missing symbol");
        assert_eq!(z.advance, 0, "the synthetic missing symbol has no advance");
    }

    #[test]
    fn a_header_that_disagrees_with_its_own_layout_is_rejected() {
        // owTLoc is the one field that can be cross-checked, and a font whose
        // tables do not land where it says they do is malformed.
        let mut d = tiny_font();
        d[17] = d[17].wrapping_add(1); // low byte of owTLoc
        assert!(matches!(
            BitmapFont::parse(&d),
            Err(Error::OutOfBounds { what: "owTLoc", .. })
        ));
    }

    #[test]
    fn truncation_is_rejected_down_to_the_last_readable_entry() {
        let d = tiny_font();
        // The smallest legitimate strike: header + bitImage + the full location
        // table + every offset/width entry a glyph lookup can reach. The words
        // past that are the documented terminator, which Geneva 9 and Geneva 12
        // both omit, so cutting them must stay legal.
        let minimum = 26 + 4 + 8 + 6;
        for n in 0..minimum {
            // Never a panic, always an error — this is untrusted input.
            assert!(BitmapFont::parse(&d[..n]).is_err(), "len {n} was accepted");
        }
        for n in minimum..=d.len() {
            let f = BitmapFont::parse(&d[..n])
                .unwrap_or_else(|e| panic!("len {n} was rejected: {e}"));
            // …and it is still a usable font, not merely a parsed one.
            assert_eq!(f.glyph(b'A').expect("A").advance, 4, "len {n}");
        }
    }

    #[test]
    fn a_zero_height_strike_is_refused_rather_than_dividing_by_it() {
        let mut d = tiny_font();
        d[14] = 0;
        d[15] = 0; // fRectHeight = 0
        assert!(BitmapFont::parse(&d).is_err());
    }

    #[test]
    fn font_ids_split_into_family_and_size() {
        // The System file on the source disk carries FONT 393, 396 and 521.
        assert_eq!(font_id_parts(393), (3, 9)); // Geneva 9
        assert_eq!(font_id_parts(396), (3, 12)); // Geneva 12
        assert_eq!(font_id_parts(521), (4, 9)); // Monaco 9
        assert_eq!(font_id_parts(384), (3, 0)); // Geneva's name record
    }

    #[test]
    fn a_name_record_has_no_strike_and_is_refused() {
        assert!(matches!(
            BitmapFont::parse(&[0u8; 8]),
            Err(Error::TooShort { len: 8 })
        ));
        let _ = vec![0u8; 0];
    }
}
