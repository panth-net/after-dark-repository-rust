//! MacBinary II unwrapping.
//!
//! Classic Mac files carry two forks plus Finder metadata, which single-fork
//! filesystems cannot represent. MacBinary packs all of it into one stream: a
//! 128-byte header, then the data fork and resource fork, each padded to a
//! 128-byte boundary.
//!
//! Needed on the import path (`.bin` files) and to unwrap the SDK archives,
//! which are MacBinary-wrapped StuffIt.

use crate::error::{Error, Result};
use crate::macroman;
use alloc::string::String;

/// Header size, and the padding granularity for both forks.
const BLOCK: usize = 128;

/// The parts of a MacBinary file.
#[derive(Debug, Clone, Copy)]
pub struct MacBinary<'a> {
    pub name: &'a [u8],
    pub file_type: [u8; 4],
    pub creator: [u8; 4],
    pub data_fork: &'a [u8],
    pub resource_fork: &'a [u8],
}

impl<'a> MacBinary<'a> {
    #[must_use]
    pub fn name_str(&self) -> String {
        macroman::decode(self.name)
    }

    /// Parse a MacBinary I/II stream.
    ///
    /// Validation follows the spec's structural checks: byte 0 (version) and
    /// byte 74 (zero fill) must be 0, the filename length must be 1..=63, and
    /// both declared fork lengths must fit within the input.
    ///
    /// # Errors
    /// [`Error::NotMacBinary`] if any check fails.
    pub fn parse(bytes: &'a [u8]) -> Result<Self> {
        let header = bytes.get(0..BLOCK).ok_or(Error::NotMacBinary)?;
        if header.first() != Some(&0) || header.get(74) != Some(&0) {
            return Err(Error::NotMacBinary);
        }
        let name_len = usize::from(*header.get(1).ok_or(Error::NotMacBinary)?);
        if name_len == 0 || name_len > 63 {
            return Err(Error::NotMacBinary);
        }
        let name = header.get(2..2usize.saturating_add(name_len)).ok_or(Error::NotMacBinary)?;

        let arr4 = |off: usize| -> Result<[u8; 4]> {
            header
                .get(off..off.saturating_add(4))
                .and_then(|s| <[u8; 4]>::try_from(s).ok())
                .ok_or(Error::NotMacBinary)
        };
        let be32 = |off: usize| -> Result<usize> {
            Ok(u32::from_be_bytes(arr4(off)?) as usize)
        };

        let file_type = arr4(65)?;
        let creator = arr4(69)?;
        let data_len = be32(83)?;
        let rsrc_len = be32(87)?;

        let data_start = BLOCK;
        let data_end = data_start
            .checked_add(data_len)
            .ok_or(Error::NotMacBinary)?;
        let data_fork = bytes.get(data_start..data_end).ok_or(Error::NotMacBinary)?;

        let rsrc_start = round_up(data_end).ok_or(Error::NotMacBinary)?;
        let rsrc_end = rsrc_start
            .checked_add(rsrc_len)
            .ok_or(Error::NotMacBinary)?;
        // Some writers omit trailing padding, so accept a short tail only when the
        // resource fork is declared empty.
        let resource_fork = match bytes.get(rsrc_start..rsrc_end) {
            Some(s) => s,
            None if rsrc_len == 0 => &[],
            None => return Err(Error::NotMacBinary),
        };

        Ok(Self {
            name,
            file_type,
            creator,
            data_fork,
            resource_fork,
        })
    }

}

/// True when `bytes` looks like a MacBinary stream.
///
/// A free function rather than an associated one: as `MacBinary<'a>::detect` it
/// would inherit the impl's lifetime and force callers to name it.
#[must_use]
pub fn is_macbinary(bytes: &[u8]) -> bool {
    MacBinary::parse(bytes).is_ok()
}

/// Round `n` up to the next 128-byte boundary.
fn round_up(n: usize) -> Option<usize> {
    let rem = n % BLOCK;
    if rem == 0 {
        Some(n)
    } else {
        n.checked_add(BLOCK.checked_sub(rem)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    fn synth(name: &str, data: &[u8], rsrc: &[u8]) -> Vec<u8> {
        let mut h = vec![0u8; BLOCK];
        h[1] = name.len() as u8;
        h[2..2 + name.len()].copy_from_slice(name.as_bytes());
        h[65..69].copy_from_slice(b"ADgm");
        h[69..73].copy_from_slice(b"ADrk");
        h[83..87].copy_from_slice(&(data.len() as u32).to_be_bytes());
        h[87..91].copy_from_slice(&(rsrc.len() as u32).to_be_bytes());
        let mut out = h;
        out.extend_from_slice(data);
        while out.len() % BLOCK != 0 {
            out.push(0);
        }
        out.extend_from_slice(rsrc);
        while out.len() % BLOCK != 0 {
            out.push(0);
        }
        out
    }

    #[test]
    fn round_trips_both_forks() {
        let raw = synth("Hard Rain", b"", b"\x01\x02\x03");
        let mb = MacBinary::parse(&raw).expect("parse");
        assert_eq!(mb.name_str(), "Hard Rain");
        assert_eq!(mb.file_type, *b"ADgm");
        assert_eq!(mb.creator, *b"ADrk");
        assert!(mb.data_fork.is_empty(), "After Dark modules have no data fork");
        assert_eq!(mb.resource_fork, b"\x01\x02\x03");
    }

    #[test]
    fn rejects_non_macbinary() {
        assert!(matches!(
            MacBinary::parse(&[0xFF; 200]),
            Err(Error::NotMacBinary)
        ));
        assert!(matches!(
            MacBinary::parse(b"too short"),
            Err(Error::NotMacBinary)
        ));
    }

    #[test]
    fn rejects_lying_fork_length() {
        let mut raw = synth("x", b"", b"");
        raw[83..87].copy_from_slice(&9_999_999u32.to_be_bytes());
        assert!(matches!(
            MacBinary::parse(&raw),
            Err(Error::NotMacBinary)
        ));
    }

    #[test]
    fn rounds_to_block_boundary() {
        assert_eq!(round_up(0), Some(0));
        assert_eq!(round_up(1), Some(128));
        assert_eq!(round_up(128), Some(128));
        assert_eq!(round_up(129), Some(256));
    }
}
