//! MacRoman text handling.
//!
//! Resource names, module titles and credit strings are MacRoman. We decode to
//! `String` for display but keep the original bytes wherever byte-exactness
//! matters, and we never let a high byte become U+FFFD silently.

use alloc::string::String;

/// MacRoman code points for bytes 0x80..=0xFF.
const HIGH: [char; 128] = [
    'Ä', 'Å', 'Ç', 'É', 'Ñ', 'Ö', 'Ü', 'á', 'à', 'â', 'ä', 'ã', 'å', 'ç', 'é', 'è', // 80
    'ê', 'ë', 'í', 'ì', 'î', 'ï', 'ñ', 'ó', 'ò', 'ô', 'ö', 'õ', 'ú', 'ù', 'û', 'ü', // 90
    '†', '°', '¢', '£', '§', '•', '¶', 'ß', '®', '©', '™', '´', '¨', '≠', 'Æ', 'Ø', // A0
    '∞', '±', '≤', '≥', '¥', 'µ', '∂', '∑', '∏', 'π', '∫', 'ª', 'º', 'Ω', 'æ', 'ø', // B0
    '¿', '¡', '¬', '√', 'ƒ', '≈', '∆', '«', '»', '…', '\u{00A0}', 'À', 'Ã', 'Õ', 'Œ', 'œ', // C0
    '–', '—', '“', '”', '‘', '’', '÷', '◊', 'ÿ', 'Ÿ', '⁄', '€', '‹', '›', 'ﬁ', 'ﬂ', // D0
    '‡', '·', '‚', '„', '‰', 'Â', 'Ê', 'Á', 'Ë', 'È', 'Í', 'Î', 'Ï', 'Ì', 'Ó', 'Ô', // E0
    '\u{F8FF}', 'Ò', 'Ú', 'Û', 'Ù', 'ı', 'ˆ', '˜', '¯', '˘', '˙', '˚', '¸', '˝', '˛', 'ˇ', // F0
];

/// Decode MacRoman bytes to a `String`, trimming trailing NULs.
///
/// Classic Mac strings use CR (`\r`) as the line terminator; we preserve it
/// rather than translating, so round-trips stay byte-exact.
#[must_use]
pub fn decode(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .rposition(|&b| b != 0)
        .map_or(0, |i| i.saturating_add(1));
    let mut out = String::with_capacity(end);
    for &b in bytes.iter().take(end) {
        match b.checked_sub(0x80) {
            // Low half is ASCII-identical.
            None => out.push(b as char),
            Some(idx) => out.push(
                HIGH.get(usize::from(idx))
                    .copied()
                    .unwrap_or('\u{FFFD}'),
            ),
        }
    }
    out
}

/// Encode a `String` back to MacRoman bytes.
///
/// The inverse of [`decode`] for anything `decode` produced. It exists for
/// *writing* resources back out, and it is deliberately not the primary path:
/// where the original bytes are still available they are what gets written, and
/// this is for names the host itself composed. A character with no MacRoman
/// representation becomes `?`, which is visible in a name rather than silent.
#[must_use]
pub fn encode(text: &str) -> alloc::vec::Vec<u8> {
    let mut out = alloc::vec::Vec::with_capacity(text.len());
    for ch in text.chars() {
        if let Ok(b) = u8::try_from(u32::from(ch)) {
            if b < 0x80 {
                out.push(b);
                continue;
            }
        }
        match HIGH.iter().position(|&c| c == ch) {
            Some(i) => out.push(0x80u8.saturating_add(i as u8)),
            None => out.push(b'?'),
        }
    }
    out
}

/// Decode a Pascal string (leading length byte) from `bytes` at `offset`.
///
/// Returns the decoded text and the total bytes consumed (`1 + len`).
#[must_use]
pub fn pascal_string(bytes: &[u8], offset: usize) -> Option<(String, usize)> {
    let len = usize::from(*bytes.get(offset)?);
    let start = offset.checked_add(1)?;
    let end = start.checked_add(len)?;
    let slice = bytes.get(start..end)?;
    Some((decode(slice), len.checked_add(1)?))
}

/// A filesystem-safe, deterministic, round-trippable encoding of a 4-byte
/// resource type.
///
/// Resource types are arbitrary bytes: `snd ` has a trailing space, `µVal`
/// contains MacRoman `0xB5`, and types differing only in case collide on
/// case-insensitive filesystems. Any byte outside `[A-Za-z0-9]` becomes `%XX`,
/// which is stable across platforms and reversible.
#[must_use]
pub fn type_to_filename(res_type: &[u8; 4]) -> String {
    let mut out = String::with_capacity(12);
    for &b in res_type {
        if b.is_ascii_alphanumeric() {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(char::from_digit(u32::from(b >> 4), 16).unwrap_or('0'));
            out.push(char::from_digit(u32::from(b & 0x0F), 16).unwrap_or('0'));
        }
    }
    out
}

/// Read a big-endian `i16` from the start of `slice` without indexing.
#[must_use]
pub(crate) fn be_i16(slice: &[u8]) -> Option<i16> {
    <[u8; 2]>::try_from(slice).ok().map(i16::from_be_bytes)
}

/// Read a big-endian `u32` from the start of `slice` without indexing.
#[must_use]
pub(crate) fn be_u32(slice: &[u8]) -> Option<u32> {
    <[u8; 4]>::try_from(slice).ok().map(u32::from_be_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::borrow::ToOwned;

    #[test]
    fn ascii_round_trips() {
        assert_eq!(decode(b"Asteroids"), "Asteroids");
    }

    #[test]
    fn decodes_copyright_and_micro() {
        assert_eq!(decode(&[0xA9]), "©");
        assert_eq!(decode(&[0xB5]), "µ");
    }

    #[test]
    fn strips_trailing_nuls_only() {
        assert_eq!(decode(b"ab\0\0"), "ab");
        assert_eq!(decode(b"a\0b"), "a\0b");
    }

    #[test]
    fn type_filenames_are_safe_and_distinct() {
        assert_eq!(type_to_filename(b"Manm"), "Manm");
        assert_eq!(type_to_filename(b"snd "), "snd%20");
        assert_eq!(type_to_filename(b"\xB5Val"), "%b5Val");
        assert_eq!(type_to_filename(b"STR "), "STR%20");
        // 'snd ' and 'STR#' must not collide with anything
        assert_ne!(type_to_filename(b"STR#"), type_to_filename(b"STR "));
    }

    #[test]
    fn encode_inverts_decode_for_every_byte() {
        // Every byte except NUL, which `decode` trims by design.
        for b in 1u8..=255 {
            assert_eq!(encode(&decode(&[b])), alloc::vec![b], "byte {b:#04x}");
        }
    }

    #[test]
    fn unrepresentable_characters_are_visible_not_silent() {
        assert_eq!(encode("a\u{4e2d}b"), b"a?b".to_vec());
    }

    #[test]
    fn pascal_string_reads_length_prefixed() {
        let buf = b"\x04Rain";
        assert_eq!(pascal_string(buf, 0), Some(("Rain".to_owned(), 5)));
    }

    #[test]
    fn pascal_string_rejects_overrun() {
        assert_eq!(pascal_string(b"\x08ab", 0), None);
    }
}
