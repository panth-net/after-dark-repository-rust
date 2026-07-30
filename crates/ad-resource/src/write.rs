//! Classic Macintosh resource fork **writer**.
//!
//! The inverse of [`crate::fork`], and deliberately the same format rather than
//! a new one: what this writes, [`crate::ResourceFork::parse`] reads, so the
//! saved file is checked by the reader that already has malformed-input tests
//! and 4000 mutants behind it. There is one format in this project, not two.
//!
//! # Why a writer exists at all
//!
//! Modules save state through the Resource Manager. Lunatic Fringe's high score
//! is `RmveResource` the old `LFhs 128`, build a handle, `AddResource`,
//! `UpdateResFile` — so "high scores survive a restart" means "a resource fork
//! gets written". Nothing else in the pipeline needs to change: the saved fork
//! is parsed at load and overlaid on the module's own resources.
//!
//! # What it never does
//!
//! It never rewrites the original module. The module file is the user's licensed
//! copy and is opened read-only; saves go to a separate overlay fork holding
//! only what the module changed.
//!
//! # Determinism
//!
//! Types are emitted in ascending byte order and ids ascending within a type,
//! identical name bytes are shared in the name list, and no timestamps or
//! handles are stored. The same resources therefore always produce the same
//! bytes, which is what makes a content hash of a fork meaningful and lets a
//! save be compared for "did anything actually change".

use alloc::vec;
use alloc::vec::Vec;

use crate::error::{Error, Result};

/// A resource that owns its bytes, ready to be written.
///
/// `name_bytes` rather than a `String`: a resource name is part of a module's
/// identity, and MacRoman decode-then-encode is not guaranteed to be the
/// identity for every byte a real fork might carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedResource {
    pub res_type: [u8; 4],
    pub id: i16,
    pub name_bytes: Option<Vec<u8>>,
    pub attrs: u8,
    pub data: Vec<u8>,
}

/// Where the data area starts. Real forks put it at 256 and leave the gap for
/// the system; matching that keeps the output readable by ResEdit and DeRez.
const DATA_OFFSET: usize = 256;
/// Bytes of map header before the type list.
const MAP_HEADER_LEN: usize = 28;
/// A resource's data offset is stored in 24 bits.
const MAX_DATA_OFFSET: usize = 0x00FF_FFFF;
/// Both map list offsets are 16-bit.
const MAX_MAP_OFFSET: usize = 0xFFFF;

fn fits(what: &'static str, value: usize, limit: usize) -> Result<()> {
    if value > limit {
        return Err(Error::TooLargeToWrite { what, value, limit });
    }
    Ok(())
}

/// Serialise resources into a resource fork.
///
/// # Errors
/// [`Error::DuplicateResource`] if two entries share a `(type, id)` — the
/// reader refuses those, so writing one would produce a file that cannot be
/// read back. [`Error::TooLargeToWrite`] if the data area exceeds 16 MB or the
/// map exceeds 64 KB, the two limits the format cannot express.
pub fn write_fork(resources: &[OwnedResource]) -> Result<Vec<u8>> {
    // Deterministic order: type ascending, then id ascending.
    let mut sorted: Vec<&OwnedResource> = resources.iter().collect();
    sorted.sort_by_key(|r| (r.res_type, r.id));
    for pair in sorted.windows(2) {
        if let [a, b] = pair {
            if a.res_type == b.res_type && a.id == b.id {
                return Err(Error::DuplicateResource {
                    res_type: a.res_type,
                    id: a.id,
                });
            }
        }
    }

    // ---- data area: u32 length + payload, per resource ----
    let mut data = Vec::new();
    let mut data_offsets: Vec<usize> = Vec::with_capacity(sorted.len());
    for r in &sorted {
        data_offsets.push(data.len());
        let len = u32::try_from(r.data.len()).map_err(|_| Error::TooLargeToWrite {
            what: "resource payload length",
            value: r.data.len(),
            limit: u32::MAX as usize,
        })?;
        data.extend_from_slice(&len.to_be_bytes());
        data.extend_from_slice(&r.data);
    }
    if let Some(last) = data_offsets.last() {
        fits("resource data offset", *last, MAX_DATA_OFFSET)?;
    }

    // ---- name list, with identical names shared ----
    // Built before the map so `nameOffset` is known when the ref lists are laid
    // out. Offsets are relative to the name list's own start.
    let mut names: Vec<u8> = Vec::new();
    let mut name_offset: Vec<i16> = Vec::with_capacity(sorted.len());
    for r in &sorted {
        match &r.name_bytes {
            // -1 is the canonical "unnamed"; the reader treats any negative
            // value that way.
            None => name_offset.push(-1),
            Some(bytes) => {
                // A Pascal string cannot exceed 255 bytes; a longer name is
                // truncated rather than refused, because the *name* is metadata
                // and the resource's data is what a save exists to preserve.
                let len = u8::try_from(bytes.len()).unwrap_or(u8::MAX);
                let entry: Vec<u8> = core::iter::once(len)
                    .chain(bytes.iter().take(usize::from(len)).copied())
                    .collect();
                let at = match find_subslice(&names, &entry) {
                    Some(at) => at,
                    None => {
                        let at = names.len();
                        names.extend_from_slice(&entry);
                        at
                    }
                };
                fits("resource name offset", at, i16::MAX as usize)?;
                name_offset.push(at as i16);
            }
        }
    }

    // ---- type list and ref lists ----
    // One type-list entry per distinct type, in the order `sorted` visits them.
    let mut types: Vec<([u8; 4], usize)> = Vec::new(); // (type, count)
    for r in &sorted {
        match types.last_mut() {
            Some((ty, count)) if *ty == r.res_type => *count = count.saturating_add(1),
            _ => types.push((r.res_type, 1)),
        }
    }

    // Every length below is checked against the field that has to hold it, so a
    // fork this writer emits either round-trips or is refused. Silent wrapping
    // here would produce a file that parses and hands back the wrong bytes,
    // which is the one outcome worth going out of the way to prevent.
    let type_list_len = types.len().saturating_mul(8).saturating_add(2);
    let ref_lists_len = sorted.len().saturating_mul(12);
    let name_list_offset = MAP_HEADER_LEN
        .saturating_add(type_list_len)
        .saturating_add(ref_lists_len);
    let map_len = name_list_offset.saturating_add(names.len());
    fits("resource name list offset", name_list_offset, MAX_MAP_OFFSET)?;

    // Map header: +0..16 is a copy of the fork header, +16 next map, +20 file
    // ref, +22 attrs. All four are written by the Resource Manager at open time
    // and ignored on read; zeroing them keeps the output byte-stable. Built by
    // appending rather than by indexing, so the layout is visible in the code
    // and there is no slice to get wrong.
    let mut map: Vec<u8> = vec![0; 24];
    map.extend_from_slice(&u16::try_from(MAP_HEADER_LEN).unwrap_or(u16::MAX).to_be_bytes());
    map.extend_from_slice(&u16::try_from(name_list_offset).unwrap_or(u16::MAX).to_be_bytes());
    debug_assert_eq!(map.len(), MAP_HEADER_LEN);

    // Counts are stored minus one, and an empty fork stores -1.
    map.extend_from_slice(&count_minus_one(types.len()).to_be_bytes());
    // `refListOffset` is relative to the start of the *type list*, not the map.
    let mut ref_cursor = type_list_len;
    for (ty, count) in &types {
        map.extend_from_slice(ty);
        map.extend_from_slice(&count_minus_one(*count).to_be_bytes());
        map.extend_from_slice(&u16::try_from(ref_cursor).unwrap_or(u16::MAX).to_be_bytes());
        ref_cursor = ref_cursor.saturating_add(count.saturating_mul(12));
    }
    for (i, r) in sorted.iter().enumerate() {
        let off = data_offsets.get(i).copied().unwrap_or(0);
        let name_at = name_offset.get(i).copied().unwrap_or(-1);
        map.extend_from_slice(&r.id.to_be_bytes());
        map.extend_from_slice(&name_at.to_be_bytes());
        map.push(r.attrs);
        let off = u32::try_from(off).unwrap_or(u32::MAX);
        map.extend_from_slice(&[(off >> 16) as u8, (off >> 8) as u8, off as u8]);
        map.extend_from_slice(&0u32.to_be_bytes()); // handle, runtime-only
    }
    map.extend_from_slice(&names);
    debug_assert_eq!(map.len(), map_len);

    // ---- header, then assemble ----
    let map_offset = DATA_OFFSET.saturating_add(data.len());
    let mut out = Vec::with_capacity(map_offset.saturating_add(map.len()));
    out.extend_from_slice(&(DATA_OFFSET as u32).to_be_bytes());
    out.extend_from_slice(&u32::try_from(map_offset).unwrap_or(u32::MAX).to_be_bytes());
    out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(0).to_be_bytes());
    out.extend_from_slice(&u32::try_from(map.len()).unwrap_or(0).to_be_bytes());
    out.resize(DATA_OFFSET, 0);
    out.extend_from_slice(&data);
    out.extend_from_slice(&map);
    Ok(out)
}

/// `count - 1`, with an empty list stored as `0xFFFF` — see the reader's
/// `count_minus_one`, which this is the exact inverse of.
fn count_minus_one(count: usize) -> u16 {
    match u16::try_from(count) {
        Ok(0) => 0xFFFF,
        Ok(n) => n.saturating_sub(1),
        Err(_) => u16::MAX.saturating_sub(1),
    }
}

/// Position of `needle` within `haystack`, for sharing identical name entries.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len().saturating_sub(needle.len()))
        .find(|&i| haystack.get(i..i.saturating_add(needle.len())) == Some(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ResourceFork;
    use alloc::vec;

    fn res(ty: &[u8; 4], id: i16, name: Option<&[u8]>, attrs: u8, data: &[u8]) -> OwnedResource {
        OwnedResource {
            res_type: *ty,
            id,
            name_bytes: name.map(<[u8]>::to_vec),
            attrs,
            data: data.to_vec(),
        }
    }

    #[test]
    fn every_field_survives_a_round_trip() {
        let input = vec![
            res(b"LFhs", 128, Some(b"High Scores"), 0x20, &[1, 2, 3, 4]),
            res(b"snd ", 1000, Some(b"Normal Shot"), 0x00, &[0xAA; 300]),
            res(b"snd ", 1001, None, 0x40, &[0xBB; 7]),
            res(b"PICT", -32000, Some(b"\xA9 1991"), 0x01, &[]),
        ];
        let bytes = write_fork(&input).expect("write");
        let fork = ResourceFork::parse(&bytes).expect("parse back");
        assert_eq!(fork.len(), input.len());
        for want in &input {
            let got = fork
                .get(&want.res_type, want.id)
                .unwrap_or_else(|| panic!("{:?} {} missing", want.res_type, want.id));
            assert_eq!(got.data, want.data.as_slice());
            assert_eq!(got.attrs, want.attrs, "attrs must survive");
            assert_eq!(
                got.name_bytes.map(<[u8]>::to_vec),
                want.name_bytes,
                "raw name bytes must survive"
            );
        }
    }

    #[test]
    fn an_empty_fork_writes_and_reads_as_empty() {
        // A real empty fork stores -1 for the type count; +1 on the unsigned
        // value would conjure 65536 phantom types. `DesktopPrinters DB` on the
        // After Dark disk is exactly this shape.
        let bytes = write_fork(&[]).expect("write");
        let fork = ResourceFork::parse(&bytes).expect("parse");
        assert!(fork.is_empty());
    }

    #[test]
    fn output_is_byte_stable_and_order_independent() {
        let a = vec![
            res(b"snd ", 2, None, 0, &[2]),
            res(b"LFhs", 1, Some(b"n"), 0, &[1]),
        ];
        let b = vec![
            res(b"LFhs", 1, Some(b"n"), 0, &[1]),
            res(b"snd ", 2, None, 0, &[2]),
        ];
        assert_eq!(write_fork(&a).unwrap(), write_fork(&b).unwrap());
        assert_eq!(write_fork(&a).unwrap(), write_fork(&a).unwrap());
    }

    #[test]
    fn identical_names_are_shared_not_repeated() {
        let many = (0..8)
            .map(|i| res(b"STR ", i, Some(b"Shared Name"), 0, &[i as u8]))
            .collect::<Vec<_>>();
        let bytes = write_fork(&many).unwrap();
        let count = (0..bytes.len())
            .filter(|&i| bytes.get(i..i + 11) == Some(b"Shared Name".as_slice()))
            .count();
        assert_eq!(count, 1, "the name list should carry one copy");
        // …and every resource still reports it.
        let fork = ResourceFork::parse(&bytes).unwrap();
        for r in fork.all() {
            assert_eq!(r.name.as_deref(), Some("Shared Name"));
        }
    }

    #[test]
    fn a_duplicate_is_refused_rather_than_written_unreadable() {
        let dup = vec![res(b"LFhs", 1, None, 0, &[1]), res(b"LFhs", 1, None, 0, &[2])];
        assert_eq!(
            write_fork(&dup),
            Err(Error::DuplicateResource {
                res_type: *b"LFhs",
                id: 1
            })
        );
    }

    #[test]
    fn a_map_too_large_to_address_is_an_error_not_a_truncation() {
        // 5,500 resources need 66,000 bytes of ref list, past the 16-bit
        // nameListOffset. Truncating that would write a fork that parses and
        // hands back the wrong bytes.
        let many = (0..5_500i16)
            .map(|i| res(b"STR ", i, None, 0, &[0]))
            .collect::<Vec<_>>();
        assert!(matches!(
            write_fork(&many),
            Err(Error::TooLargeToWrite { .. })
        ));
    }
}
