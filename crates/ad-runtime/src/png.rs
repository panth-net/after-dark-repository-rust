//! Writing a PNG, with no dependency to do it.
//!
//! Every visual claim in this project is checked by looking at a picture — a
//! rendered module frame, and now the launcher's own interface. That makes a PNG
//! writer part of the *evidence* path, not a convenience, so it lives here rather
//! than being copied into each tool that needs one. It was copied into two before
//! this existed.
//!
//! Deflate "stored" blocks: no compression, so no compression library. The files
//! are larger than they need to be and are read by a human or a hash, never
//! shipped.

use std::path::Path;

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, entry) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *entry = c;
    }
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        let index = usize::from(((c ^ u32::from(b)) & 0xFF) as u8);
        c = table.get(index).copied().unwrap_or(0) ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a.wrapping_add(u32::from(x))) % 65521;
        b = (b.wrapping_add(a)) % 65521;
    }
    (b << 16) | a
}

fn chunk(tag: &[u8], data: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(data.len())
        .unwrap_or(0)
        .to_be_bytes()
        .to_vec();
    let mut body = tag.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
    out
}

/// Write 8-bit RGB pixels as a PNG.
///
/// # Errors
/// The underlying write error, or a message if `rgb` is not `width * height * 3`
/// bytes — a short buffer would otherwise produce a file that opens and is wrong.
pub fn write_rgb(path: &Path, width: u32, height: u32, rgb: &[u8]) -> Result<(), String> {
    let stride = (width as usize).saturating_mul(3);
    let want = stride.saturating_mul(height as usize);
    if rgb.len() != want {
        return Err(format!(
            "{}: expected {want} bytes for {width}x{height}, got {}",
            path.display(),
            rgb.len()
        ));
    }

    // One filter byte (0 = none) per scanline.
    let mut raw = Vec::with_capacity(want.saturating_add(height as usize));
    for y in 0..height as usize {
        raw.push(0);
        let from = y.saturating_mul(stride);
        let to = from.saturating_add(stride);
        raw.extend_from_slice(rgb.get(from..to).unwrap_or(&[]));
    }

    let mut z = vec![0x78, 0x01];
    let blocks = raw.chunks(65_535).count().max(1);
    for (i, part) in raw.chunks(65_535).enumerate() {
        z.push(u8::from(i.saturating_add(1) >= blocks));
        let len = u16::try_from(part.len()).unwrap_or(u16::MAX);
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(part);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit, truecolour

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend_from_slice(&chunk(b"IHDR", &ihdr));
    png.extend_from_slice(&chunk(b"IDAT", &z));
    png.extend_from_slice(&chunk(b"IEND", b""));
    std::fs::write(path, png).map_err(|e| format!("{}: {e}", path.display()))
}

/// Write `0RGB` pixels — what a window buffer holds — as a PNG.
///
/// # Errors
/// As [`write_rgb`].
pub fn write_argb(path: &Path, width: u32, height: u32, px: &[u32]) -> Result<(), String> {
    let mut rgb = Vec::with_capacity(px.len().saturating_mul(3));
    for &p in px {
        rgb.push((p >> 16) as u8);
        rgb.push((p >> 8) as u8);
        rgb.push(p as u8);
    }
    write_rgb(path, width, height, &rgb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_file_a_decoder_would_accept() {
        let dir = std::env::temp_dir().join("ad-runtime-png");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("t.png");
        let px = vec![0x00FF_8000u32; 4 * 3];
        write_argb(&path, 4, 3, &px).expect("write");
        let bytes = std::fs::read(&path).expect("read");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        // IHDR must report the dimensions asked for, not the buffer's shape.
        assert_eq!(&bytes[16..20], &4u32.to_be_bytes());
        assert_eq!(&bytes[20..24], &3u32.to_be_bytes());
        assert!(bytes.ends_with(&chunk(b"IEND", b"")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_short_buffer_is_refused_rather_than_written_wrong() {
        let path = std::env::temp_dir().join("ad-runtime-png-bad.png");
        let err = write_rgb(&path, 10, 10, &[0u8; 9]).expect_err("must refuse");
        assert!(err.contains("expected 300 bytes"), "{err}");
        assert!(!path.exists(), "nothing should have been written");
    }
}
