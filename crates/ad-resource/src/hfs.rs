//! Reading resource forks straight out of an HFS disk image.
//!
//! Format reference: *Inside Macintosh: Files*, chapter 2, and Apple Technical
//! Note TN1150 for the B-tree layout HFS and HFS+ share.
//!
//! ```text
//! +0     ┌─ boot blocks (2 sectors, ignored) ──────────┐
//! +1024  ├─ master directory block ────────────────────┤
//!        │ 'BD', allocation block size and start,      │
//!        │ and the first extents of the two B-trees    │
//!        ├─ extents overflow B-tree ───────────────────┤
//!        │ where to find the rest of a fragmented file │
//!        ├─ catalog B-tree ────────────────────────────┤
//!        │ one record per file: name, type, creator,   │
//!        │ and the resource fork's length and extents  │
//!        └─────────────────────────────────────────────┘
//! ```
//!
//! # Why this is in this crate
//!
//! It is the same job as [`crate::fork`], one layer down: untrusted bytes in,
//! typed structures out, no I/O and no execution. The caller reads the image and
//! decides what to do with what comes back.
//!
//! It exists at all because the alternative was a Python script the user had to
//! run in a terminal before the application would start. Extraction is the first
//! thing a new player has to do and it cannot require a toolchain, so it belongs
//! in the product rather than in `tools/`.
//!
//! # Malformed images
//!
//! A disk image off the internet is untrusted input in the same sense a module
//! is, and a truncated or hand-edited one must not panic. Every read here is
//! bounds-checked and every offset computed with checked arithmetic; a bad
//! volume produces [`Error::HfsMalformed`], and a file whose extents do not add
//! up is skipped rather than returned short.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::macroman;

/// Bytes per sector, and per B-tree node. Both are fixed at 512 in HFS.
const SECTOR: usize = 512;
const NODE_SIZE: usize = 512;
/// The master directory block sits two sectors into the volume.
const MDB_OFFSET: usize = 1024;
/// `drSigWord` — what marks the start of an HFS volume.
const HFS_SIGNATURE: [u8; 2] = *b"BD";
/// Leaf records in the extents overflow tree have a 7-byte key.
const EXTENT_KEY_LEN: u8 = 7;
/// `cdrFilRec` — the catalog record type for a file.
const CATALOG_FILE_RECORD: u8 = 2;
/// `xkrFkType` for the resource fork; the data fork is 0.
const FORK_TYPE_RESOURCE: u8 = 0xFF;

/// One file's resource fork, lifted out of the volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkFile {
    /// The file's name on the original disk, decoded from MacRoman.
    pub name: String,
    /// Finder type (`APPL`, `ADgm`, `FFIL`, …), exactly as stored.
    pub file_type: [u8; 4],
    /// Finder creator code, exactly as stored.
    pub creator: [u8; 4],
    /// The resource fork's bytes, ready for [`crate::ResourceFork::parse`].
    pub data: Vec<u8>,
}

impl ForkFile {
    /// The name with anything awkward for a file system replaced by `_`.
    ///
    /// Classic Mac names allow `/` and `:` and start with whatever they like;
    /// this is the same reduction the original extraction script used, so a
    /// library extracted by either route has identical filenames.
    #[must_use]
    pub fn safe_file_name(&self) -> String {
        let mut out = String::with_capacity(self.name.len());
        let mut last_was_replacement = false;
        for ch in self.name.chars() {
            if ch.is_ascii_alphanumeric() || matches!(ch, ' ' | '.' | '_' | '!' | '\'' | '-') {
                out.push(ch);
                last_was_replacement = false;
            } else if !last_was_replacement {
                // Runs collapse to a single `_`, matching the `+` in the
                // script's regular expression.
                out.push('_');
                last_was_replacement = true;
            }
        }
        out
    }
}

/// Every resource fork on the volume, in catalog order.
///
/// Files with an empty resource fork are omitted: on an After Dark disk those
/// are documents and installer payloads, and nothing downstream can use one.
///
/// # Errors
///
/// [`Error::NotAnHfsVolume`] when no HFS volume header is found, and
/// [`Error::HfsMalformed`] when one is found but its structures do not parse.
pub fn resource_forks(image: &[u8]) -> Result<Vec<ForkFile>> {
    let volume = find_volume(image)?;
    let mdb = Mdb::parse(volume)?;

    // The extents overflow tree first: reading any *other* fragmented file may
    // need it, including the catalog tree itself is not possible — its own
    // extents must fit in the MDB, which the format guarantees.
    let extents_tree = mdb.read_fork_from(
        volume,
        mdb.extents_file_size,
        &mdb.extents_first_extents,
        &BTreeMap::new(),
        0,
        false,
    )?;
    let overflow = parse_overflow(&extents_tree)?;

    let catalog = mdb.read_fork_from(
        volume,
        mdb.catalog_file_size,
        &mdb.catalog_first_extents,
        &overflow,
        // CNID 4 is the catalog file itself, per Inside Macintosh: Files.
        4,
        false,
    )?;

    let mut out = Vec::new();
    for record in leaf_records(&catalog)? {
        let Some(file) = parse_catalog_file(record) else {
            continue;
        };
        if file.resource_len == 0 {
            continue;
        }
        // A file whose extents do not add up is skipped, not returned short: a
        // truncated resource fork parses into subtly wrong sprites, which is
        // worse than a module that simply is not listed.
        let Ok(data) = mdb.read_fork_from(
            volume,
            file.resource_len,
            &file.resource_extents,
            &overflow,
            file.cnid,
            true,
        ) else {
            continue;
        };
        out.push(ForkFile {
            name: file.name,
            file_type: file.file_type,
            creator: file.creator,
            data,
        });
    }
    Ok(out)
}

/// Find the start of the HFS volume within the image.
///
/// Disk images are not all bare volumes: some carry a partition map, some a
/// leading header from whatever wrote them. Rather than decode every container
/// this scans sector boundaries for the volume signature, which is what the
/// original extraction script did and what makes an image from any of the
/// archives work without asking the user which kind they downloaded.
fn find_volume(image: &[u8]) -> Result<&[u8]> {
    let mut start = 0usize;
    while let Some(sig_at) = start.checked_add(MDB_OFFSET) {
        let Some(sig) = take(image, sig_at, 2) else {
            break;
        };
        if sig == HFS_SIGNATURE {
            return image.get(start..).ok_or(Error::NotAnHfsVolume);
        }
        let Some(next) = start.checked_add(SECTOR) else {
            break;
        };
        start = next;
    }
    Err(Error::NotAnHfsVolume)
}

/// The fields of the master directory block this reader needs.
struct Mdb {
    /// `drAlBlkSiz` — bytes per allocation block.
    alloc_block_size: u32,
    /// `drAlBlSt` — the sector the first allocation block starts at.
    alloc_block_start: u16,
    extents_file_size: u32,
    extents_first_extents: Vec<(u16, u16)>,
    catalog_file_size: u32,
    catalog_first_extents: Vec<(u16, u16)>,
}

impl Mdb {
    fn parse(volume: &[u8]) -> Result<Self> {
        let mdb = volume.get(MDB_OFFSET..).ok_or(Error::HfsMalformed {
            what: "master directory block",
        })?;
        Ok(Self {
            alloc_block_size: be_u32(mdb, 20).ok_or(Error::HfsMalformed { what: "drAlBlkSiz" })?,
            alloc_block_start: be_u16(mdb, 28).ok_or(Error::HfsMalformed { what: "drAlBlSt" })?,
            extents_file_size: be_u32(mdb, 130)
                .ok_or(Error::HfsMalformed { what: "drXTFlSize" })?,
            extents_first_extents: extent_record(
                take(mdb, 134, 12).ok_or(Error::HfsMalformed { what: "drXTExtRec" })?,
            ),
            catalog_file_size: be_u32(mdb, 146)
                .ok_or(Error::HfsMalformed { what: "drCTFlSize" })?,
            catalog_first_extents: extent_record(
                take(mdb, 150, 12).ok_or(Error::HfsMalformed { what: "drCTExtRec" })?,
            ),
        })
    }

    /// Byte offset of an allocation block within the volume.
    fn block_offset(&self, block: u32) -> Option<usize> {
        let base = SECTOR.checked_mul(usize::from(self.alloc_block_start))?;
        let within = usize::try_from(self.alloc_block_size)
            .ok()?
            .checked_mul(usize::try_from(block).ok()?)?;
        base.checked_add(within)
    }

    /// Concatenate the bytes an extent list points at.
    fn gather(&self, volume: &[u8], extents: &[(u16, u16)]) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        for &(start, count) in extents {
            let from = self.block_offset(u32::from(start))?;
            let to = self.block_offset(u32::from(start).checked_add(u32::from(count))?)?;
            out.extend_from_slice(volume.get(from..to)?);
        }
        Some(out)
    }

    /// Read a fork of `size` bytes, following the overflow tree when its first
    /// three extents are not the whole story.
    fn read_fork_from(
        &self,
        volume: &[u8],
        size: u32,
        first: &[(u16, u16)],
        overflow: &Overflow,
        cnid: u32,
        resource_fork: bool,
    ) -> Result<Vec<u8>> {
        let malformed = Error::HfsMalformed {
            what: "fork extents",
        };
        if size == 0 {
            return Ok(Vec::new());
        }
        let block_size = self.alloc_block_size;
        if block_size == 0 {
            return Err(Error::HfsMalformed {
                what: "drAlBlkSiz is zero",
            });
        }
        // Blocks needed, rounded up.
        let needed = size
            .checked_add(block_size.saturating_sub(1))
            .ok_or(malformed.clone())?
            .checked_div(block_size)
            .ok_or(malformed.clone())?;

        let mut extents: Vec<(u16, u16)> = Vec::new();
        let mut have: u32 = 0;
        for &(start, count) in first {
            have = have
                .checked_add(u32::from(count))
                .ok_or(malformed.clone())?;
            extents.push((start, count));
        }
        // Each overflow record is keyed by the block index it continues from, so
        // this walks forward and cannot loop: `have` strictly increases or the
        // lookup fails.
        while have < needed {
            let key = (
                cnid,
                resource_fork,
                u16::try_from(have).map_err(|_| malformed.clone())?,
            );
            let more = overflow.get(&key).ok_or(malformed.clone())?;
            let before = have;
            for &(start, count) in more {
                have = have
                    .checked_add(u32::from(count))
                    .ok_or(malformed.clone())?;
                extents.push((start, count));
            }
            if have == before {
                return Err(malformed);
            }
        }

        let mut bytes = self.gather(volume, &extents).ok_or(malformed.clone())?;
        let want = usize::try_from(size).map_err(|_| malformed.clone())?;
        if bytes.len() < want {
            return Err(malformed);
        }
        bytes.truncate(want);
        Ok(bytes)
    }
}

/// Extents keyed by (file, which fork, first block this extent covers).
type Overflow = BTreeMap<(u32, bool, u16), Vec<(u16, u16)>>;

/// Decode a 12-byte extent record: three (start block, block count) pairs.
///
/// A zero count terminates the list — the remaining pairs are padding.
fn extent_record(bytes: &[u8]) -> Vec<(u16, u16)> {
    let mut out = Vec::new();
    for pair in 0..3usize {
        let Some(at) = pair.checked_mul(4) else { break };
        let (Some(start), Some(count)) = (be_u16(bytes, at), be_u16(bytes, at.saturating_add(2)))
        else {
            break;
        };
        if count == 0 {
            continue;
        }
        out.push((start, count));
    }
    out
}

fn parse_overflow(tree: &[u8]) -> Result<Overflow> {
    let mut map = Overflow::new();
    for record in leaf_records(tree)? {
        // Key length, then the key: fork type, file number, first block.
        if record.first() != Some(&EXTENT_KEY_LEN) {
            continue;
        }
        let (Some(&fork_type), Some(cnid), Some(first_block)) =
            (record.get(1), be_u32(record, 2), be_u16(record, 6))
        else {
            continue;
        };
        let Some(data) = take(record, 8, 12) else {
            continue;
        };
        map.insert(
            (cnid, fork_type == FORK_TYPE_RESOURCE, first_block),
            extent_record(data),
        );
    }
    Ok(map)
}

/// What a catalog file record says about one file.
struct CatalogFile {
    name: String,
    file_type: [u8; 4],
    creator: [u8; 4],
    cnid: u32,
    resource_len: u32,
    resource_extents: Vec<(u16, u16)>,
}

fn parse_catalog_file(record: &[u8]) -> Option<CatalogFile> {
    let key_len = usize::from(*record.first()?);
    if key_len == 0 {
        return None;
    }
    // Key: reserved byte, parent ID, then a Pascal-string name.
    let name_len = usize::from(*record.get(6)?);
    let name = macroman::decode(take(record, 7, name_len)?);

    // The record's data begins at the next even offset after the key.
    let after_key = key_len.checked_add(1)?;
    let data_at = after_key.checked_add(after_key & 1)?;
    let data = record.get(data_at..)?;
    if data.first() != Some(&CATALOG_FILE_RECORD) {
        return None;
    }
    // Past cdrType and cdrResrv2 is the file record proper.
    let file = data.get(2..)?;

    let mut file_type = [0u8; 4];
    let mut creator = [0u8; 4];
    file_type.copy_from_slice(take(file, 2, 4)?);
    creator.copy_from_slice(take(file, 6, 4)?);

    Some(CatalogFile {
        name,
        file_type,
        creator,
        cnid: be_u32(file, 18)?,
        resource_len: be_u32(file, 34)?,
        resource_extents: extent_record(take(file, 84, 12)?),
    })
}

/// Every record in every leaf node of a B-tree, in order.
///
/// The leaves are a linked list, so this follows `bthFNode` to `bthLNode`
/// rather than descending from the root — the whole tree is wanted, and the
/// index nodes have nothing to add. Visited nodes are tracked because a corrupt
/// image can point a leaf's forward link back into the chain, and this is a
/// `while` loop over untrusted data.
fn leaf_records(tree: &[u8]) -> Result<Vec<&[u8]>> {
    let malformed = Error::HfsMalformed {
        what: "B-tree header",
    };
    let (_, header_records) = node_records(tree, 0).ok_or(malformed.clone())?;
    let header = header_records.first().ok_or(malformed.clone())?;
    // bthDepth(2) bthRoot(4) bthNRecs(4) bthFNode(4) bthLNode(4)
    let first_leaf = be_u32(header, 10).ok_or(malformed.clone())?;
    let last_leaf = be_u32(header, 14).ok_or(malformed.clone())?;

    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut node = first_leaf;
    loop {
        if !seen.insert(node) {
            return Err(Error::HfsMalformed {
                what: "B-tree leaf chain loops",
            });
        }
        let index = usize::try_from(node).map_err(|_| malformed.clone())?;
        let (next, records) = node_records(tree, index).ok_or(malformed.clone())?;
        out.extend(records);
        if node == last_leaf {
            break;
        }
        node = next;
    }
    Ok(out)
}

/// Split one B-tree node into its records.
///
/// Records are located by a table of offsets stored backwards at the end of the
/// node, which is why this returns them re-reversed into record order.
fn node_records(tree: &[u8], node: usize) -> Option<(u32, Vec<&[u8]>)> {
    let start = node.checked_mul(NODE_SIZE)?;
    let bytes = take(tree, start, NODE_SIZE)?;
    let forward_link = be_u32(bytes, 0)?;
    let count = usize::from(be_u16(bytes, 10)?);

    // One offset per record, plus a terminator marking the end of the last.
    let slots = count.checked_add(1)?;
    let table_at = NODE_SIZE.checked_sub(slots.checked_mul(2)?)?;
    let mut offsets = Vec::with_capacity(slots);
    for slot in 0..slots {
        let at = table_at.checked_add(slot.checked_mul(2)?)?;
        offsets.push(usize::from(be_u16(bytes, at)?));
    }
    offsets.reverse();

    let mut records = Vec::with_capacity(count);
    for pair in offsets.windows(2) {
        let (Some(&from), Some(&to)) = (pair.first(), pair.get(1)) else {
            continue;
        };
        // A node whose offset table is not monotonic, or points past the node,
        // yields no record rather than a slice from somewhere else.
        if from > to || to > table_at {
            continue;
        }
        if let Some(record) = bytes.get(from..to) {
            records.push(record);
        }
    }
    Some((forward_link, records))
}

fn be_u16(bytes: &[u8], at: usize) -> Option<u16> {
    let end = at.checked_add(2)?;
    let slice = bytes.get(at..end)?;
    Some(u16::from_be_bytes([*slice.first()?, *slice.get(1)?]))
}

fn be_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    let slice = bytes.get(at..end)?;
    Some(u32::from_be_bytes([
        *slice.first()?,
        *slice.get(1)?,
        *slice.get(2)?,
        *slice.get(3)?,
    ]))
}

fn take(bytes: &[u8], at: usize, len: usize) -> Option<&[u8]> {
    bytes.get(at..at.checked_add(len)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// An empty or tiny buffer is "not a volume", not a panic.
    #[test]
    fn a_buffer_with_no_volume_header_is_rejected() {
        assert_eq!(resource_forks(&[]).unwrap_err(), Error::NotAnHfsVolume);
        assert_eq!(
            resource_forks(&[0u8; 64]).unwrap_err(),
            Error::NotAnHfsVolume
        );
        assert_eq!(
            resource_forks(&[0u8; 4096]).unwrap_err(),
            Error::NotAnHfsVolume
        );
    }

    /// A volume signature with nothing behind it fails as malformed rather than
    /// reading past the end of the buffer.
    #[test]
    fn a_signature_with_a_truncated_volume_is_malformed_not_a_panic() {
        let mut image = vec![0u8; MDB_OFFSET + 2];
        image[MDB_OFFSET] = b'B';
        image[MDB_OFFSET + 1] = b'D';
        assert!(matches!(
            resource_forks(&image),
            Err(Error::HfsMalformed { .. })
        ));
    }

    /// The signature is found at a sector boundary anywhere in the image, so an
    /// image with a partition map or a leading header still reads.
    #[test]
    fn the_volume_is_found_at_an_offset() {
        let offset = SECTOR * 3;
        let mut image = vec![0u8; offset + MDB_OFFSET + 2];
        image[offset + MDB_OFFSET] = b'B';
        image[offset + MDB_OFFSET + 1] = b'D';
        let found = find_volume(&image).expect("signature is at sector 3");
        assert_eq!(found.len(), image.len() - offset);
    }

    /// A zero block count ends the extent list; the padding after it is not
    /// mistaken for a real extent.
    #[test]
    fn extent_records_stop_at_a_zero_count() {
        // (start 1, count 2), (start 9, count 0), (start 7, count 3)
        let bytes = [0, 1, 0, 2, 0, 9, 0, 0, 0, 7, 0, 3];
        assert_eq!(extent_record(&bytes), vec![(1, 2), (7, 3)]);
        assert_eq!(extent_record(&[]), vec![]);
    }

    /// Names are reduced the same way the original Python script reduced them,
    /// so a library extracted by either route has identical filenames.
    #[test]
    fn file_names_are_reduced_to_something_a_file_system_accepts() {
        let named = |name: &str| ForkFile {
            name: String::from(name),
            file_type: *b"ADgm",
            creator: *b"ADrk",
            data: Vec::new(),
        };
        assert_eq!(named("Flying Toasters").safe_file_name(), "Flying Toasters");
        assert_eq!(named("Mowin' Man").safe_file_name(), "Mowin' Man");
        assert_eq!(named("Fish!").safe_file_name(), "Fish!");
        // Runs of awkward characters collapse to a single underscore.
        assert_eq!(named("a/b:c").safe_file_name(), "a_b_c");
        assert_eq!(named("a///b").safe_file_name(), "a_b");
    }

    /// A node whose offset table is nonsense yields no records instead of
    /// slicing from wherever the numbers happened to point.
    #[test]
    fn a_node_with_a_bad_offset_table_yields_no_records() {
        let mut node = vec![0u8; NODE_SIZE];
        // Claim one record, then give a descending offset pair.
        node[10] = 0;
        node[11] = 1;
        // Table at the end: terminator then record start, stored backwards.
        let table = NODE_SIZE - 4;
        node[table] = 0x00;
        node[table + 1] = 0x0E; // terminator: 14
        node[table + 2] = 0x01;
        node[table + 3] = 0x00; // record start: 256 — past the terminator
        let (_, records) = node_records(&node, 0).expect("node is full length");
        assert!(records.is_empty(), "a descending pair must not slice");
    }
}
