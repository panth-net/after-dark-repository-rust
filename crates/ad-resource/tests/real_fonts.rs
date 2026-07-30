//! The font decoder against the real strikes in the System file.
//!
//! Skips when `modules/` is absent — it is gitignored, being extracted from the
//! user's own disk image, so CI and a fresh clone have no fonts to read. The unit
//! tests cover the header arithmetic on a hand-assembled font; this covers the
//! thing they cannot, which is whether a *shipped* 1991 strike decodes into
//! legible glyphs. Run it with `--nocapture` to see them.

// This crate enables `arithmetic_side_effects` because it parses untrusted input.
// This file renders ASCII art from an already-validated font; the arithmetic is
// bounded by the font's own header and is clearer written plainly.
#![allow(clippy::arithmetic_side_effects, clippy::indexing_slicing)]

use ad_resource::{font::font_id_parts, BitmapFont, ResourceFork};
use std::path::PathBuf;

fn modules_dir() -> Option<PathBuf> {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../modules");
    d.is_dir().then_some(d)
}

/// Render one line of text as ASCII art, the way QuickDraw will blit it.
fn render(font: &BitmapFont<'_>, text: &str) -> Vec<String> {
    let width = font.text_width(text.as_bytes()).max(1) as usize;
    let mut rows = vec![vec![b' '; width + 4]; font.rect_height.max(1) as usize];
    let mut pen = 0i32;
    for &ch in text.as_bytes() {
        let Some(g) = font.glyph(ch) else { continue };
        for bit in 0..g.bits {
            for y in 0..font.rect_height {
                if !font.strike_bit(g.strike_bit + bit, y) {
                    continue;
                }
                let x = pen + i32::from(g.left) + i64::from(bit) as i32;
                if let Ok(x) = usize::try_from(x) {
                    if let Some(row) = rows.get_mut(y as usize) {
                        if let Some(cell) = row.get_mut(x) {
                            *cell = b'#';
                        }
                    }
                }
            }
        }
        pen += i32::from(g.advance);
    }
    rows.into_iter()
        .map(|r| String::from_utf8_lossy(&r).trim_end().to_owned())
        .collect()
}

#[test]
fn the_system_files_fonts_decode_into_legible_glyphs() {
    let Some(dir) = modules_dir() else {
        println!("skipped: no modules/ directory (it is gitignored)");
        return;
    };
    let bytes = match std::fs::read(dir.join("System.rsrc")) {
        Ok(b) => b,
        Err(e) => {
            println!("skipped: System.rsrc: {e}");
            return;
        }
    };
    let fork = ResourceFork::parse(&bytes).expect("parse System.rsrc");
    let strikes: Vec<_> = fork
        .all()
        .iter()
        .filter(|r| &r.res_type == b"FONT" && font_id_parts(r.id).1 != 0)
        .collect();
    assert!(!strikes.is_empty(), "System.rsrc carries no FONT strikes");

    for r in strikes {
        let (family, size) = font_id_parts(r.id);
        let font = BitmapFont::parse(r.data)
            .unwrap_or_else(|e| panic!("FONT {} ({family}/{size}): {e}", r.id));

        // The header must describe a plausible strike, not merely a parseable one.
        //
        // `fRectHeight` is the bitmap bounding box, *not* the point size: Geneva 9
        // is 12 rows tall with an ascent of 10 and a descent of 2. Point size is
        // the em, so the only true relation is that the bitmap is at least as tall
        // as the size and at least as tall as ascent plus descent.
        assert!(
            font.rect_height >= size,
            "FONT {}: a size-{size} font cannot be {} rows tall",
            r.id,
            font.rect_height
        );
        assert!(font.ascent > 0 && font.descent >= 0, "FONT {}", r.id);
        assert!(
            font.ascent + font.descent <= font.rect_height,
            "FONT {}: ascent+descent must fit the strike",
            r.id
        );

        // Every printable ASCII character must have a glyph, and a run of them
        // must have ink. A decoder that got the location table wrong still
        // "works" until you ask whether anything is actually set.
        let mut inked = 0;
        for ch in b'!'..=b'~' {
            let g = font.glyph(ch).unwrap_or_else(|| panic!("FONT {}: {ch}", r.id));
            assert!(g.advance > 0, "FONT {}: {:?} has no advance", r.id, ch as char);
            if (0..g.bits).any(|b| (0..font.rect_height).any(|y| font.strike_bit(g.strike_bit + b, y)))
            {
                inked += 1;
            }
        }
        assert_eq!(inked, (b'!'..=b'~').count(), "FONT {}: some glyph is blank", r.id);

        // A space must advance and have no ink — the cheapest way to catch a
        // strike read one glyph out of step.
        let space = font.glyph(b' ').expect("space");
        assert!(space.advance > 0, "FONT {}: space must advance", r.id);
        assert!(
            (0..space.bits).all(|b| (0..font.rect_height).all(|y| !font.strike_bit(space.strike_bit + b, y))),
            "FONT {}: space has ink, so the location table is off by a glyph",
            r.id
        );

        println!("\n=== FONT {} — family {family}, size {size} ===", r.id);
        for line in render(&font, "After Dark") {
            println!("{line}");
        }
    }
}
