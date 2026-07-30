//! Classic Macintosh resource fork reader.
//!
//! Format reference: *Inside Macintosh: More Macintosh Toolbox*, Resource Manager.
//!
//! ```text
//! offset 0   ┌─ fork header (16 bytes) ────────────────┐
//!            │ u32 dataOffset  u32 mapOffset           │
//!            │ u32 dataLength  u32 mapLength           │
//! mapOffset  ├─ map ───────────────────────────────────┤
//!            │ +24 u16 typeListOffset (from map start) │
//!            │ +26 u16 nameListOffset (from map start) │
//!            ├─ type list ─────────────────────────────┤
//!            │ u16 numTypes-1                          │
//!            │ [4]u8 type, u16 count-1, u16 refOffset  │ × numTypes
//!            ├─ ref lists ─────────────────────────────┤
//!            │ i16 id, i16 nameOffset, u8 attrs,       │
//!            │ u24 dataOffset, u32 handle              │ × count
//! dataOffset ├─ data ──────────────────────────────────┤
//!            │ u32 length, [length]u8 payload          │ × n
//! ```
//!
//! Every field is read through a bounds-checked accessor. A malformed fork must
//! produce an [`Error`], never a panic and never a truncated resource — a
//! silently short payload would render as a subtly wrong sprite rather than a
//! visible failure.
//!
//! # Region confinement
//!
//! Note from the diagram that every offset stored *inside* a fork is relative to
//! a region the header declared: the type list, name list and ref lists are
//! map-relative, and each resource's payload offset is data-relative. So the two
//! regions are carved into sub-slices up front ([`Reader`]) and each offset is
//! resolved inside its own region only. Escaping is then unrepresentable rather
//! than merely rejected: an offset that would land in the other region, in the
//! 16-byte header, or in the padding between them is simply past the end of the
//! slice being read.
//!
//! Bounds-checking each read against the whole file instead — what this used to
//! do — accepts a data offset that happens to point into the map and hands back
//! the map's bytes as a resource payload. Nothing downstream can tell that from
//! a real resource, which makes it worse than an error.
//!
//! What the header *declares* is still taken at face value: a fork that declares
//! its two regions overlapping gets exactly the bytes it asked for. That is not
//! an escape — the bytes are inside the declared region — and refusing it would
//! reject real forks, several of which pad `dataLength` past the last payload.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::macroman;

/// Bytes in the fork header: two offset/length pairs.
const HEADER_LEN: usize = 16;

/// A single resource, borrowing its payload from the fork bytes.
#[derive(Clone)]
pub struct Resource<'a> {
    /// Four-byte resource type, exactly as stored (e.g. `b"snd "`).
    pub res_type: [u8; 4],
    pub id: i16,
    /// Resource name decoded from MacRoman, if it has one.
    pub name: Option<String>,
    /// Raw name bytes, preserved for byte-exact round-tripping.
    pub name_bytes: Option<&'a [u8]>,
    /// Resource attribute byte (`resSysHeap`, `resPurgeable`, `resLocked`, …).
    pub attrs: u8,
    pub data: &'a [u8],
}

/// First long of a System 7 compressed resource: `0xA89F6572`.
///
/// The word `$A89F` is also the *Unimplemented* A-line trap, which is exactly
/// how this format announces itself when nobody is looking for it: hand the
/// compressed bytes to a CPU and the first instruction is a trap nothing
/// implements. Thirty-one modules on the After Dark 3.0-era discs reported
/// "unhandled Toolbox trap $A89F" for that reason and no other.
pub const COMPRESSED_MAGIC: u32 = 0xA89F_6572;

/// The header on a compressed resource.
///
/// Only what is needed to recognise one and say what it would take to expand
/// it. Decompression itself is not implemented; see [`Resource::compression`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Compression {
    /// Header format version — 8 and 9 both occur; the 3.0-era discs use 9.
    pub version: u8,
    /// Size of the resource once expanded.
    pub unpacked_len: u32,
    /// `dcmp` resource ID that can expand it. IDs 0–2 were supplied by the
    /// System; anything else ships inside the file, as `dcmp 128` does here.
    pub decompressor_id: i16,
}

impl<'a> Resource<'a> {
    /// The resource type as a display string (`snd ` keeps its trailing space).
    #[must_use]
    pub fn type_str(&self) -> String {
        macroman::decode(&self.res_type)
    }

    /// The compression header, if this resource is compressed.
    ///
    /// Recognised by the magic rather than by the attribute byte: the attribute
    /// varies across the discs here (`0x01`, `0x29`, `0x39`…) while the magic
    /// does not, and a resource whose payload begins with it is compressed
    /// whatever its flags claim.
    #[must_use]
    pub fn compression(&self) -> Option<Compression> {
        let magic = u32::from_be_bytes(self.data.get(0..4)?.try_into().ok()?);
        if magic != COMPRESSED_MAGIC {
            return None;
        }
        Some(Compression {
            version: *self.data.get(6)?,
            unpacked_len: u32::from_be_bytes(self.data.get(8..12)?.try_into().ok()?),
            decompressor_id: i16::from_be_bytes(self.data.get(12..14)?.try_into().ok()?),
        })
    }

    /// Whether this resource's payload is compressed.
    #[must_use]
    pub fn is_compressed(&self) -> bool {
        self.compression().is_some()
    }

    /// Read a big-endian `i16` at `offset` within this resource's payload.
    #[must_use]
    pub fn be_i16(&self, offset: usize) -> Option<i16> {
        macroman::be_i16(self.data.get(offset..offset.checked_add(2)?)?)
    }

    /// Read a big-endian `u32` at `offset` within this resource's payload.
    #[must_use]
    pub fn be_u32(&self, offset: usize) -> Option<u32> {
        macroman::be_u32(self.data.get(offset..offset.checked_add(4)?)?)
    }
}

impl core::fmt::Debug for Resource<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Resource")
            .field("type", &self.type_str())
            .field("id", &self.id)
            .field("name", &self.name)
            .field("attrs", &format_args!("{:#04x}", self.attrs))
            .field("len", &self.data.len())
            .finish()
    }
}

/// A parsed resource fork.
#[derive(Debug)]
pub struct ResourceFork<'a> {
    resources: Vec<Resource<'a>>,
    /// `(type, id)` → index into `resources`.
    index: BTreeMap<([u8; 4], i16), usize>,
}

/// Bounds-checked big-endian readers over **one region** of the fork.
///
/// `bytes` is the region alone — the header, the data area, or the map — so an
/// offset is checked against the extent it is relative to and nothing wider. See
/// the module's *Region confinement* section for why that is the whole point.
///
/// `base` and `fork_len` are carried for diagnostics only: an error still names
/// the file offset you would look at in a hex dump, not a region-relative one.
struct Reader<'a> {
    bytes: &'a [u8],
    base: usize,
    fork_len: usize,
}

impl<'a> Reader<'a> {
    /// The 16-byte fork header, bounded so header reads cannot stray into the
    /// areas the header describes.
    fn header(bytes: &'a [u8]) -> Result<Self> {
        let fork_len = bytes.len();
        Ok(Self {
            bytes: bytes.get(..HEADER_LEN).ok_or(Error::TooShort { len: fork_len })?,
            base: 0,
            fork_len,
        })
    }

    /// Carve the region `off..off + len` out of the fork.
    ///
    /// This is both the header's bounds check and the region's construction:
    /// there is no way to obtain a `Reader` for a region that does not fit, so no
    /// later read can be validated against the wrong extent.
    fn region(bytes: &'a [u8], what: &'static str, off: u32, len: u32) -> Result<Self> {
        let fork_len = bytes.len();
        let oob = || Error::HeaderOutOfBounds {
            what,
            offset: off,
            len,
            fork_len,
        };
        // `usize` may be narrower than `u32`. A value that cannot be represented
        // is out of bounds, never a truncated — and therefore wrong — index.
        let base = usize::try_from(off).map_err(|_| oob())?;
        let end = usize::try_from(len)
            .ok()
            .and_then(|l| base.checked_add(l))
            .ok_or_else(oob)?;
        Ok(Self {
            bytes: bytes.get(base..end).ok_or_else(oob)?,
            base,
            fork_len,
        })
    }

    /// Fork-absolute position of a region offset. Diagnostics only.
    fn at(&self, offset: usize) -> usize {
        self.base.saturating_add(offset)
    }

    fn oob(&self, what: &'static str, offset: usize, need: usize) -> Error {
        Error::OutOfBounds {
            what,
            offset: self.at(offset),
            need,
            fork_len: self.fork_len,
        }
    }

    /// Fetch a fixed-size array, or an out-of-bounds error naming the field.
    ///
    /// Using `try_into` rather than indexing means the compiler proves the length,
    /// so there is no panic path to reason about.
    fn take<const N: usize>(&self, offset: usize, what: &'static str) -> Result<[u8; N]> {
        let end = offset.checked_add(N).ok_or_else(|| self.oob(what, offset, N))?;
        self.bytes
            .get(offset..end)
            .and_then(|s| <[u8; N]>::try_from(s).ok())
            .ok_or_else(|| self.oob(what, offset, N))
    }

    /// Borrow `len` bytes at `offset`, within this region.
    fn slice(&self, offset: usize, len: usize, what: &'static str) -> Result<&'a [u8]> {
        let end = offset
            .checked_add(len)
            .ok_or_else(|| self.oob(what, offset, len))?;
        self.bytes
            .get(offset..end)
            .ok_or_else(|| self.oob(what, offset, len))
    }

    fn u16(&self, offset: usize, what: &'static str) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take::<2>(offset, what)?))
    }

    fn u32(&self, offset: usize, what: &'static str) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take::<4>(offset, what)?))
    }

    fn u8(&self, offset: usize, what: &'static str) -> Result<u8> {
        Ok(u8::from_be_bytes(self.take::<1>(offset, what)?))
    }

    /// A 24-bit big-endian value, as used for resource data offsets, widened to
    /// an index. On a target whose `usize` is narrower it cannot be an index at
    /// all, which is out of bounds rather than a silent truncation.
    fn u24(&self, offset: usize, what: &'static str) -> Result<usize> {
        let [a, b, c] = self.take::<3>(offset, what)?;
        let v = (u32::from(a) << 16) | (u32::from(b) << 8) | u32::from(c);
        usize::try_from(v).map_err(|_| self.oob(what, offset, 3))
    }

    fn array4(&self, offset: usize, what: &'static str) -> Result<[u8; 4]> {
        self.take::<4>(offset, what)
    }

    /// Borrow one resource's payload: a `u32` length followed by that many bytes,
    /// both inside this region.
    ///
    /// Called on the data-area reader, so a resource can only ever be assembled
    /// from data-area bytes. A length that overruns the area is an error even
    /// when those bytes exist in the file — they belong to the map.
    fn payload(&self, offset: usize, res_type: [u8; 4], id: i16) -> Result<&'a [u8]> {
        let size = self.u32(offset, "resource length")?;
        // Saturating: if it saturated, the `get` below fails and reports it.
        let start = offset.saturating_add(4);
        let oob = || Error::ResourceOutOfBounds {
            res_type,
            id,
            offset: self.at(start),
            size,
            fork_len: self.fork_len,
        };
        let end = usize::try_from(size)
            .ok()
            .and_then(|n| start.checked_add(n))
            .ok_or_else(oob)?;
        self.bytes.get(start..end).ok_or_else(oob)
    }

    /// Borrow a Pascal string (leading length byte) at `offset`, within this
    /// region — both the length byte and the bytes it claims.
    fn pascal(&self, offset: usize, what: &'static str) -> Result<&'a [u8]> {
        let len = usize::from(self.u8(offset, what)?);
        self.slice(offset.saturating_add(1), len, what)
    }
}

/// Resolve a count stored as `count - 1`.
///
/// The Resource Manager stores both the type count and each type's resource
/// count decremented by one, so an **empty** map stores `-1` (`0xFFFF`). Adding
/// one to the unsigned value would yield 65536 phantom entries instead of zero —
/// which is exactly what a real empty fork (`DesktopPrinters DB` on the After
/// Dark disk) triggers.
#[must_use]
fn count_minus_one(raw: u16) -> usize {
    if raw == 0xFFFF {
        0
    } else {
        usize::from(raw).saturating_add(1)
    }
}

impl<'a> ResourceFork<'a> {
    /// Parse a resource fork.
    ///
    /// # Errors
    /// Returns [`Error`] if any structure is out of bounds or a `(type, id)`
    /// pair is duplicated.
    pub fn parse(bytes: &'a [u8]) -> Result<Self> {
        let header = Reader::header(bytes)?;

        // Carving the regions *is* the header bounds check; from here on `bytes`
        // is never touched again, so nothing can address the fork as a whole.
        let data = Reader::region(
            bytes,
            "data area",
            header.u32(0, "dataOffset")?,
            header.u32(8, "dataLength")?,
        )?;
        let map = Reader::region(
            bytes,
            "map area",
            header.u32(4, "mapOffset")?,
            header.u32(12, "mapLength")?,
        )?;

        // Both list offsets are map-relative and stay map-relative: a u16 cannot
        // reach out of the map area it indexes.
        let type_list = usize::from(map.u16(24, "typeListOffset")?);
        let name_list = usize::from(map.u16(26, "nameListOffset")?);

        // Both counts are stored as `count - 1`; see `count_minus_one`.
        let num_types = count_minus_one(map.u16(type_list, "numTypes")?);

        let mut resources = Vec::new();
        let mut index = BTreeMap::new();

        for ti in 0..num_types {
            // 8-byte entries follow the 2-byte count.
            let entry = type_list
                .checked_add(2)
                .and_then(|b| b.checked_add(ti.checked_mul(8)?))
                .ok_or_else(|| map.oob("type list entry", type_list, 8))?;
            let res_type = map.array4(entry, "resource type")?;
            let count = count_minus_one(map.u16(entry.saturating_add(4), "type count")?);
            // refListOffset is relative to the start of the type list, not to the
            // map and not to the file.
            let ref_base = type_list
                .checked_add(usize::from(
                    map.u16(entry.saturating_add(6), "refListOffset")?,
                ))
                .ok_or_else(|| map.oob("refListOffset", entry, 2))?;

            for ri in 0..count {
                let rref = ri
                    .checked_mul(12)
                    .and_then(|d| ref_base.checked_add(d))
                    .ok_or_else(|| map.oob("ref list entry", ref_base, 12))?;

                let id = map.u16(rref, "resource id")? as i16;
                let name_rel = map.u16(rref.saturating_add(2), "nameOffset")? as i16;
                let attrs = map.u8(rref.saturating_add(4), "attrs")?;
                let rel_data = map.u24(rref.saturating_add(5), "resource dataOffset")?;

                // Data-relative, resolved in the data area alone: an offset that
                // lands in the map or the header cannot resolve at all.
                let data_bytes = data.payload(rel_data, res_type, id)?;

                // A negative nameOffset (canonically -1) means "unnamed"; a
                // positive one indexes the name list, which lives in the map.
                let name_bytes = match usize::try_from(name_rel) {
                    Err(_) => None,
                    Ok(rel) => Some(map.pascal(
                        name_list
                            .checked_add(rel)
                            .ok_or_else(|| map.oob("resource name", name_list, 1))?,
                        "resource name",
                    )?),
                };
                let name = name_bytes.map(macroman::decode);

                if index.insert((res_type, id), resources.len()).is_some() {
                    return Err(Error::DuplicateResource { res_type, id });
                }
                resources.push(Resource {
                    res_type,
                    id,
                    name,
                    name_bytes,
                    attrs,
                    data: data_bytes,
                });
            }
        }

        Ok(Self { resources, index })
    }

    /// All resources, in resource-map order.
    #[must_use]
    pub fn all(&self) -> &[Resource<'a>] {
        &self.resources
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.resources.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
    }

    /// Look up one resource by type and id.
    #[must_use]
    pub fn get(&self, res_type: &[u8; 4], id: i16) -> Option<&Resource<'a>> {
        self.resources.get(*self.index.get(&(*res_type, id))?)
    }

    /// All resources of one type, in ascending id order.
    #[must_use]
    pub fn of_type(&self, res_type: &[u8; 4]) -> Vec<&Resource<'a>> {
        let mut v: Vec<&Resource<'a>> = self
            .resources
            .iter()
            .filter(|r| &r.res_type == res_type)
            .collect();
        v.sort_by_key(|r| r.id);
        v
    }

    /// Count of resources of one type.
    #[must_use]
    pub fn count_of(&self, res_type: &[u8; 4]) -> usize {
        self.resources
            .iter()
            .filter(|r| &r.res_type == res_type)
            .count()
    }

    /// Every distinct resource type present, in ascending byte order.
    #[must_use]
    pub fn types(&self) -> Vec<[u8; 4]> {
        let mut t: Vec<[u8; 4]> = self.resources.iter().map(|r| r.res_type).collect();
        t.sort_unstable();
        t.dedup();
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    // Map-relative layout of the synthetic fork built by `build`.
    const TYPE_LIST: usize = 28;
    const ENTRY_MANM: usize = 30; // type list entry 0
    const ENTRY_SND: usize = 38; // type list entry 1
    const REF_MANM: usize = 46; // 2 × 12-byte ref entries
    const REF_SND: usize = 70; // 1 × 12-byte ref entry
    const NAME_LIST: usize = 82;
    const MAP_LEN: usize = 85;

    // Data-relative layout: three payloads, each a u32 length then its bytes.
    const REL_A: usize = 0; // Manm 1,   b"AAAA"
    const REL_B: usize = 8; // Manm 2,   b"BB"
    const REL_C: usize = 14; // snd  128, b"CCCCCCCC"
    const DATA_LEN: usize = 26;

    /// A synthetic fork plus where its regions ended up: the fixed point that
    /// every malformed variant below is derived from by poking one field.
    struct Synth {
        bytes: Vec<u8>,
        data_at: usize,
        map_at: usize,
    }

    impl Synth {
        /// Absolute position of a map-relative offset, for editing the fixture.
        fn map(&self, rel: usize) -> usize {
            self.map_at + rel
        }
    }

    fn put_u16(b: &mut [u8], at: usize, v: u16) {
        b[at..at + 2].copy_from_slice(&v.to_be_bytes());
    }

    fn put_u32(b: &mut [u8], at: usize, v: u32) {
        b[at..at + 4].copy_from_slice(&v.to_be_bytes());
    }

    fn ref_entry(map: &mut [u8], at: usize, id: i16, name_off: i16, attrs: u8, rel: usize) {
        put_u16(map, at, id as u16);
        put_u16(map, at + 2, name_off as u16);
        map[at + 4] = attrs;
        map[at + 5] = (rel >> 16) as u8;
        map[at + 6] = (rel >> 8) as u8;
        map[at + 7] = rel as u8;
        // at+8..at+12 is the Resource Manager's handle field: zero on disk.
    }

    /// Build a minimal but complete fork: three resources of two types, one of
    /// them named.
    ///
    /// `map_first` puts the map area *before* the data area. Real forks are the
    /// other way round, but the layout is legal and it is the only one in which a
    /// forward-only, map-relative u16 offset can reach the data area at all — so
    /// it is the only layout that can test that direction of confinement.
    fn build(map_first: bool) -> Synth {
        let mut data = vec![0u8; DATA_LEN];
        put_u32(&mut data, REL_A, 4);
        data[REL_A + 4..REL_A + 8].copy_from_slice(b"AAAA");
        put_u32(&mut data, REL_B, 2);
        data[REL_B + 4..REL_B + 6].copy_from_slice(b"BB");
        put_u32(&mut data, REL_C, 8);
        data[REL_C + 4..REL_C + 12].copy_from_slice(b"CCCCCCCC");

        let mut map = vec![0u8; MAP_LEN];
        // map[0..16] is the Resource Manager's copy of the fork header and is
        // ignored by the parser; some tests plant bait bytes there.
        put_u16(&mut map, 24, TYPE_LIST as u16);
        put_u16(&mut map, 26, NAME_LIST as u16);
        put_u16(&mut map, TYPE_LIST, 1); // numTypes - 1
        map[ENTRY_MANM..ENTRY_MANM + 4].copy_from_slice(b"Manm");
        put_u16(&mut map, ENTRY_MANM + 4, 1); // count - 1
        put_u16(&mut map, ENTRY_MANM + 6, (REF_MANM - TYPE_LIST) as u16);
        map[ENTRY_SND..ENTRY_SND + 4].copy_from_slice(b"snd ");
        put_u16(&mut map, ENTRY_SND + 4, 0); // count - 1
        put_u16(&mut map, ENTRY_SND + 6, (REF_SND - TYPE_LIST) as u16);
        ref_entry(&mut map, REF_MANM, 1, -1, 0x00, REL_A);
        ref_entry(&mut map, REF_MANM + 12, 2, -1, 0x40, REL_B);
        ref_entry(&mut map, REF_SND, 128, 0, 0x00, REL_C);
        map[NAME_LIST..NAME_LIST + 3].copy_from_slice(b"\x02hi");

        // Real forks start the data area at 256; the gap belongs to neither
        // region, so no internal offset may resolve into it either.
        let (data_at, map_at) = if map_first {
            (HEADER_LEN + MAP_LEN, HEADER_LEN)
        } else {
            (256, 256 + DATA_LEN)
        };
        let mut bytes = vec![0u8; (data_at + DATA_LEN).max(map_at + MAP_LEN)];
        put_u32(&mut bytes, 0, data_at as u32);
        put_u32(&mut bytes, 4, map_at as u32);
        put_u32(&mut bytes, 8, DATA_LEN as u32);
        put_u32(&mut bytes, 12, MAP_LEN as u32);
        bytes[data_at..data_at + DATA_LEN].copy_from_slice(&data);
        bytes[map_at..map_at + MAP_LEN].copy_from_slice(&map);
        Synth {
            bytes,
            data_at,
            map_at,
        }
    }

    /// Offset of `part` within `whole` by *provenance* rather than by contents.
    /// The invariant under test is which region the bytes came from, and the same
    /// bytes occur in more than one place in a fork, so comparing contents would
    /// pass by coincidence.
    fn offset_in(whole: &[u8], part: &[u8]) -> usize {
        (part.as_ptr() as usize).wrapping_sub(whole.as_ptr() as usize)
    }

    /// Assert every payload came from the declared data area and every name from
    /// the declared map area.
    fn assert_regions(fork: &ResourceFork<'_>, bytes: &[u8]) {
        let data_at = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        let map_at = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        let data_len = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
        let map_len = u32::from_be_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as usize;
        for r in fork.all() {
            let off = offset_in(bytes, r.data);
            assert!(
                off >= data_at && off + r.data.len() <= data_at + data_len,
                "{:?} {} payload came from outside the declared data area",
                r.type_str(),
                r.id
            );
            if let Some(n) = r.name_bytes {
                let off = offset_in(bytes, n);
                assert!(
                    off >= map_at && off + n.len() <= map_at + map_len,
                    "{:?} {} name came from outside the declared map area",
                    r.type_str(),
                    r.id
                );
            }
        }
    }

    #[test]
    fn synthetic_fork_parses_and_every_byte_comes_from_its_own_region() {
        for map_first in [false, true] {
            let s = build(map_first);
            let fork = ResourceFork::parse(&s.bytes).expect("the fixture must be a valid fork");
            assert_eq!(fork.len(), 3);
            assert_eq!(fork.get(b"Manm", 1).expect("Manm 1").data, b"AAAA");
            let b = fork.get(b"Manm", 2).expect("Manm 2");
            assert_eq!(b.data, b"BB");
            assert_eq!(b.attrs, 0x40);
            assert_eq!(b.name, None, "nameOffset -1 means unnamed");
            let c = fork.get(b"snd ", 128).expect("snd 128");
            assert_eq!(c.data, b"CCCCCCCC");
            assert_eq!(c.name.as_deref(), Some("hi"));
            assert_eq!(fork.types(), vec![*b"Manm", *b"snd "]);
            assert_regions(&fork, &s.bytes);
        }
    }

    /// Prevents: a resource dataOffset that leaves the data area and names bytes
    /// in the map. The offset here is `DATA_LEN`, i.e. the first byte past the
    /// data area, which is the first byte of the map — in bounds for the file,
    /// and holding a plausible length/payload pair. Resolving against the whole
    /// fork returned `b"MAP!"` as the payload of Manm 1.
    #[test]
    fn resource_data_offset_cannot_reach_into_the_map_area() {
        let mut s = build(false);
        let bait = s.map(0);
        put_u32(&mut s.bytes, bait, 4);
        s.bytes[bait + 4..bait + 8].copy_from_slice(b"MAP!");
        // The bait is in the map's ignored header copy, so the fork still parses.
        ResourceFork::parse(&s.bytes).expect("bait must not disturb a valid fork");
        assert!(
            s.data_at + DATA_LEN + 8 <= s.bytes.len(),
            "the escaping offset must be in bounds for the file, or this proves nothing"
        );

        let rel = s.map(REF_MANM);
        s.bytes[rel + 5] = 0;
        s.bytes[rel + 6] = 0;
        s.bytes[rel + 7] = DATA_LEN as u8;
        let err = ResourceFork::parse(&s.bytes).expect_err("must not read the map as a payload");
        assert!(
            matches!(
                err,
                Error::OutOfBounds {
                    what: "resource length",
                    ..
                }
            ),
            "got {err:?}"
        );
    }

    /// Prevents: a map-relative typeListOffset that leaves the map and reads a
    /// type list out of resource payload bytes. Needs the map-before-data layout,
    /// since a u16 offset only points forward.
    #[test]
    fn type_list_offset_cannot_reach_into_the_data_area() {
        let mut s = build(true);
        assert!(
            s.data_at == s.map_at + MAP_LEN && s.data_at + DATA_LEN <= s.bytes.len(),
            "the data area must directly follow the map for this to be a crossing, not a truncation"
        );
        let field = s.map(24);
        put_u16(&mut s.bytes, field, MAP_LEN as u16);
        let err = ResourceFork::parse(&s.bytes).expect_err("must not read a type list from data");
        assert!(
            matches!(err, Error::OutOfBounds { what: "numTypes", .. }),
            "got {err:?}"
        );
    }

    /// Prevents: a nameOffset that walks off the end of the name list and takes
    /// its Pascal length byte from a resource payload. Under whole-fork bounds
    /// this yielded a resource named from data-area bytes.
    #[test]
    fn resource_name_offset_cannot_reach_into_the_data_area() {
        let mut s = build(true);
        let entry = s.map(REF_SND);
        // NAME_LIST + this == MAP_LEN, the first byte of the data area.
        put_u16(&mut s.bytes, entry + 2, (MAP_LEN - NAME_LIST) as u16);
        let err = ResourceFork::parse(&s.bytes).expect_err("must not name a resource from data");
        assert!(
            matches!(
                err,
                Error::OutOfBounds {
                    what: "resource name",
                    ..
                }
            ),
            "got {err:?}"
        );
    }

    /// Prevents: an offset resolving into the 16-byte fork header. A region
    /// declared at offset 0 (or, as here, an empty one) used to let a resource
    /// offset of 0 read its length word out of the header itself and hand back
    /// the header's bytes; a map declared over the header used to read
    /// typeListOffset from beyond it.
    #[test]
    fn offsets_cannot_resolve_into_the_fork_header() {
        let mut s = build(false);
        put_u32(&mut s.bytes, 0, 0); // dataOffset = 0
        put_u32(&mut s.bytes, 8, 0); // dataLength = 0
        let err = ResourceFork::parse(&s.bytes)
            .expect_err("an empty data area cannot yield resources, least of all header bytes");
        assert!(
            matches!(
                err,
                Error::OutOfBounds {
                    what: "resource length",
                    ..
                }
            ),
            "got {err:?}"
        );

        let mut s = build(false);
        put_u32(&mut s.bytes, 4, 0); // mapOffset = 0
        put_u32(&mut s.bytes, 12, HEADER_LEN as u32); // mapLength = 16
        let err = ResourceFork::parse(&s.bytes)
            .expect_err("a 16-byte map has no field at +24 to read");
        assert!(
            matches!(
                err,
                Error::OutOfBounds {
                    what: "typeListOffset",
                    ..
                }
            ),
            "got {err:?}"
        );
    }

    /// Prevents: a truncated fork parsing into short or missing resources instead
    /// of failing. Every strict prefix must be rejected — the map area is last,
    /// so losing any byte of it is a declared region that no longer fits — and no
    /// prefix may panic.
    #[test]
    fn truncated_forks_are_rejected_not_silently_shortened() {
        assert!(matches!(
            ResourceFork::parse(&[]),
            Err(Error::TooShort { len: 0 })
        ));
        assert!(matches!(
            ResourceFork::parse(&[0u8; HEADER_LEN - 1]),
            Err(Error::TooShort { .. })
        ));
        let s = build(false);
        for len in 0..s.bytes.len() {
            assert!(
                ResourceFork::parse(&s.bytes[..len]).is_err(),
                "prefix of {len} bytes must not parse"
            );
        }
        ResourceFork::parse(&s.bytes).expect("only the whole fork parses");
    }

    /// Prevents: a declared payload length overrunning the data area and pulling
    /// in the following map. 60 bytes from `REL_C` runs past `DATA_LEN` but stays
    /// inside the file, so whole-fork bounds accepted it and the resource's tail
    /// was the resource map.
    #[test]
    fn resource_length_past_the_data_area_is_rejected() {
        let mut s = build(false);
        let at = s.data_at + REL_C;
        put_u32(&mut s.bytes, at, 60);
        assert!(
            at + 4 + 60 <= s.bytes.len(),
            "the overrun must stay inside the file, or this proves nothing"
        );
        let err = ResourceFork::parse(&s.bytes).expect_err("must not extend a payload into the map");
        assert!(
            matches!(
                err,
                Error::ResourceOutOfBounds { res_type, id: 128, size: 60, .. }
                    if res_type == *b"snd "
            ),
            "got {err:?}"
        );
    }

    /// Prevents: the `0xFFFF` empty-count sentinel becoming 65536 phantom
    /// entries. It appears in two places — the type count and each type's
    /// resource count — and both must resolve to zero, not to a walk off the end
    /// of the map.
    #[test]
    fn empty_count_sentinel_yields_nothing_not_65536_entries() {
        let mut s = build(false);
        let field = s.map(TYPE_LIST);
        put_u16(&mut s.bytes, field, 0xFFFF); // numTypes - 1 == -1
        let fork = ResourceFork::parse(&s.bytes).expect("an empty map is valid, not an error");
        assert!(fork.is_empty());
        assert_eq!(fork.types().len(), 0);

        let mut s = build(false);
        let field = s.map(ENTRY_MANM + 4);
        put_u16(&mut s.bytes, field, 0xFFFF); // this type holds -1 + 1 == 0
        let fork = ResourceFork::parse(&s.bytes).expect("a type with no resources is valid");
        assert_eq!(fork.len(), 1, "only snd 128 survives");
        assert_eq!(fork.count_of(b"Manm"), 0);
        assert_regions(&fork, &s.bytes);
    }

    /// xorshift32. Deterministic and dependency-free, so any failure reproduces
    /// exactly from the seed.
    fn next(state: &mut u32) -> u32 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        *state = x;
        x
    }

    /// Arbitrary input must either error or produce a fork whose every byte is
    /// accounted for. Mutations are biased towards the header and the map because
    /// that is where structure lives: a flip inside a payload cannot change how
    /// anything resolves.
    #[test]
    fn mutated_forks_never_panic_and_never_cross_regions() {
        let base = build(false);
        let mut seed = 0x9E37_79B9;
        let mut parsed = 0usize;
        for _ in 0..4000 {
            let mut b = base.bytes.clone();
            for _ in 0..=next(&mut seed) % 4 {
                let pos = match next(&mut seed) % 4 {
                    0 => next(&mut seed) as usize % HEADER_LEN,
                    1 | 2 => base.map_at + next(&mut seed) as usize % MAP_LEN,
                    _ => next(&mut seed) as usize % b.len(),
                };
                b[pos] ^= (next(&mut seed) >> 13) as u8 | 1;
            }
            if next(&mut seed) % 8 == 0 {
                let keep = next(&mut seed) as usize % b.len();
                b.truncate(keep);
            }
            let Ok(fork) = ResourceFork::parse(&b) else {
                continue;
            };
            parsed += 1;
            assert_eq!(fork.len(), fork.all().len());
            assert_eq!(fork.is_empty(), fork.all().is_empty());
            let types = fork.types();
            assert!(
                types.windows(2).all(|w| w[0] < w[1]),
                "types() must be sorted and deduplicated"
            );
            assert_eq!(
                types.iter().map(|t| fork.count_of(t)).sum::<usize>(),
                fork.len(),
                "per-type counts must partition the resource list"
            );
            for r in fork.all() {
                let got = fork
                    .get(&r.res_type, r.id)
                    .expect("the index must resolve every resource it was built from");
                assert_eq!((got.res_type, got.id), (r.res_type, r.id));
            }
            assert_regions(&fork, &b);
        }
        assert!(
            parsed > 100,
            "only {parsed} of 4000 mutants parsed — the corpus stopped exercising success paths"
        );
    }
}
