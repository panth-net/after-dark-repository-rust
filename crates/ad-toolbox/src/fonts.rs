//! The fonts QuickDraw draws with.
//!
//! Fonts do not come from the module. They came from the System file, so the
//! *host* supplies them — the same arrangement as `KCHR`, and for the same
//! reason: a module calling `TextFont(0)` is asking for the system font, which it
//! has never seen and does not carry.
//!
//! # Why this holds bytes rather than parsed strikes
//!
//! [`ad_resource::BitmapFont`] borrows its strike, so something must own the
//! resource. Re-parsing on each lookup is a handful of bounds-checked header
//! reads and keeps this a plain owned value instead of a self-referential one.
//! Every stored strike has already parsed once, at `add` time, so a lookup cannot
//! fail on bytes that were accepted.

use ad_resource::{font::font_id_parts, BitmapFont, ResourceFork};

/// One strike, with whatever is known about which font it is.
#[derive(Debug, Clone)]
struct Strike {
    /// Font family number, or `None` when the resource does not say.
    family: Option<i16>,
    /// Point size, or `None` — see [`FontBank::add_nfnt`].
    size: Option<i16>,
    bytes: Vec<u8>,
}

/// Every font available to the emulated machine.
#[derive(Debug, Default)]
pub struct FontBank {
    strikes: Vec<Strike>,
}

impl FontBank {
    /// Add a `FONT` strike, whose id encodes family and size.
    ///
    /// Returns false when the bytes are not a usable strike, which includes the
    /// family *name* records that share the type.
    pub fn add_font(&mut self, id: i16, bytes: &[u8]) -> bool {
        let (family, size) = font_id_parts(id);
        if size == 0 || BitmapFont::parse(bytes).is_err() {
            return false;
        }
        self.strikes.push(Strike {
            family: Some(family),
            size: Some(size),
            bytes: bytes.to_vec(),
        });
        true
    }

    /// Add an `NFNT` strike.
    ///
    /// An `NFNT` id carries no meaning — a `FOND` maps ids to family and size, and
    /// this does not parse `FOND`s. So the family and size are recorded as
    /// unknown and the strike is used only when nothing better matches. Guessing a
    /// size from the bitmap height would be wrong in a way that silently picks the
    /// wrong font: Geneva 9 is twelve rows tall.
    pub fn add_nfnt(&mut self, bytes: &[u8]) -> bool {
        if BitmapFont::parse(bytes).is_err() {
            return false;
        }
        self.strikes.push(Strike {
            family: None,
            size: None,
            bytes: bytes.to_vec(),
        });
        true
    }

    /// Load every strike in a resource fork — a System file or a font suitcase.
    ///
    /// Returns how many were added.
    pub fn load_fork(&mut self, fork_bytes: &[u8]) -> usize {
        let Ok(fork) = ResourceFork::parse(fork_bytes) else {
            return 0;
        };
        let mut added = 0;
        for r in fork.all() {
            let ok = match &r.res_type {
                b"FONT" => self.add_font(r.id, r.data),
                b"NFNT" => self.add_nfnt(r.data),
                _ => false,
            };
            added += usize::from(ok);
        }
        added
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.strikes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.strikes.is_empty()
    }

    /// The best strike for `family` at `size`.
    ///
    /// Exact match first, then the same family at the nearest size, then any
    /// family at the nearest size, then a strike of unknown identity. Never
    /// `None` when anything is loaded: text that silently fails to draw is the
    /// state this replaces, and a slightly wrong size is far more useful than a
    /// blank screen.
    #[must_use]
    pub fn best(&self, family: i16, size: i16) -> Option<BitmapFont<'_>> {
        let score = |s: &Strike| -> (u8, i32) {
            // Lower is better: tier first, then how far off the size is.
            let distance = s
                .size
                .map_or(1_000, |sz| i32::from(sz).saturating_sub(i32::from(size)).abs());
            match (s.family, s.size) {
                (Some(f), Some(sz)) if f == family && sz == size => (0, 0),
                (Some(f), Some(_)) if f == family => (1, distance),
                (Some(_), Some(_)) => (2, distance),
                _ => (3, 0),
            }
        };
        let best = self.strikes.iter().min_by_key(|s| score(s))?;
        BitmapFont::parse(&best.bytes).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A strike whose only job is to be parseable, with a recognisable advance so
    /// tests can tell which one `best` picked.
    fn strike(advance: u8) -> Vec<u8> {
        let mut d = Vec::new();
        let mut w = |v: i16| d.extend_from_slice(&v.to_be_bytes());
        w(0x9000u16 as i16); // fontType
        w(b'A' as i16); // firstChar
        w(b'A' as i16); // lastChar
        w(3); // widMax
        w(0); // kernMax
        w(-2); // nDescent
        w(3); // fRectWidth
        w(2); // fRectHeight
        // owTLoc: words from +16 to the owTable = header(26) + strike(4) + loc(6) - 16
        w(((26 + 4 + 6) - 16) / 2);
        w(2); // ascent
        w(0); // descent
        w(0); // leading
        w(1); // rowWords
        d.extend_from_slice(&[0b1110_0000, 0x00, 0b1110_0000, 0x00]); // strike
        for v in [0i16, 3, 3] {
            d.extend_from_slice(&v.to_be_bytes()); // locTable
        }
        for v in [i16::from(advance), 0, 0] {
            d.extend_from_slice(&v.to_be_bytes()); // owTable: offset 0, advance
        }
        d
    }

    #[test]
    fn an_empty_bank_has_no_font() {
        let bank = FontBank::default();
        assert!(bank.is_empty());
        assert!(bank.best(0, 12).is_none());
    }

    #[test]
    fn exact_family_and_size_wins() {
        let mut bank = FontBank::default();
        // Geneva 9 = FONT 393, Geneva 12 = 396, Monaco 9 = 521 — the three real
        // ids in the System file on the source disk.
        assert!(bank.add_font(393, &strike(9)));
        assert!(bank.add_font(396, &strike(12)));
        assert!(bank.add_font(521, &strike(4)));
        assert_eq!(bank.len(), 3);
        assert_eq!(bank.best(3, 12).expect("geneva 12").char_width(b'A'), 12);
        assert_eq!(bank.best(3, 9).expect("geneva 9").char_width(b'A'), 9);
        assert_eq!(bank.best(4, 9).expect("monaco 9").char_width(b'A'), 4);
    }

    #[test]
    fn the_same_family_at_a_different_size_beats_another_family() {
        let mut bank = FontBank::default();
        bank.add_font(393, &strike(9)); // Geneva 9
        bank.add_font(521, &strike(4)); // Monaco 9 — exact size, wrong family
        // Asking for Geneva 10 must give Geneva 9, not Monaco 9.
        assert_eq!(bank.best(3, 10).expect("font").char_width(b'A'), 9);
    }

    #[test]
    fn any_font_beats_no_font() {
        let mut bank = FontBank::default();
        bank.add_font(396, &strike(12));
        // Family 0 is the system font, which is not loaded. Text that silently
        // fails to draw is the state this replaces.
        assert_eq!(bank.best(0, 12).expect("a fallback").char_width(b'A'), 12);
    }

    #[test]
    fn a_strike_of_unknown_identity_is_the_last_resort() {
        let mut bank = FontBank::default();
        assert!(bank.add_nfnt(&strike(7)));
        bank.add_font(396, &strike(12));
        // A known Geneva 12 outranks an NFNT whose family and size are unknown,
        // even when the request matches neither.
        assert_eq!(bank.best(9, 18).expect("font").char_width(b'A'), 12);
    }

    #[test]
    fn name_records_and_junk_are_refused() {
        let mut bank = FontBank::default();
        // A FONT id that is a multiple of 128 is a family *name* record.
        assert!(!bank.add_font(384, &strike(9)));
        assert!(!bank.add_font(396, b"not a font"));
        assert!(!bank.add_nfnt(&[]));
        assert!(bank.is_empty());
    }
}
