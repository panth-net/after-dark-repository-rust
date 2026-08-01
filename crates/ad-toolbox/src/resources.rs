//! Resource Manager backing store.
//!
//! `_GetResource` must hand the module a `Handle` whose block contains the
//! resource bytes. The module then reads through it, and may lock, unlock or
//! release it like any other handle.
//!
//! Resources are loaded **lazily and cached**: a module that asks for the same
//! resource twice must get the same handle back, because plenty of code compares
//! handles or releases what it fetched earlier.

use std::collections::BTreeMap;

use ad_memory::{Memory, globals};

/// Where a durable resource write goes.
///
/// The Toolbox does no file I/O — it has no idea what a path is, and keeping it
/// that way is what lets the same core run in a screen saver, a windowed player
/// and a headless lab. The host supplies the sink; the *module* decides when to
/// write, by calling `_WriteResource` or `_UpdateResFile`.
pub trait ResourceSink: std::fmt::Debug {
    /// Persist the current state of every changed resource.
    ///
    /// # Contract
    /// The write must be **atomic**: a process killed during it leaves either
    /// the previous contents or the new ones, never a half-written file. That is
    /// the whole point — a high score is either fully present or cleanly absent.
    ///
    /// # Errors
    /// A message describing the failure. The store keeps the resources marked
    /// dirty, so a later attempt still has them.
    fn persist(&mut self, changed: &[StoredResource]) -> Result<(), String>;
}

/// One resource available to the module.
///
/// `attrs` and `name_bytes` exist because the parser already preserves them and
/// a store that drops them cannot write a fork back without losing data. The
/// decoded `name` is for comparison and diagnostics; `name_bytes` is what gets
/// written, because MacRoman decode-then-encode is not guaranteed to be the
/// identity and a resource name is part of a module's identity.
#[derive(Debug, Clone, Default)]
pub struct StoredResource {
    pub res_type: [u8; 4],
    pub id: i16,
    pub name: Option<String>,
    /// Raw name bytes exactly as stored, authoritative when writing back.
    pub name_bytes: Option<Vec<u8>>,
    /// Resource attribute byte (`resSysHeap`, `resPurgeable`, `resLocked`, …).
    pub attrs: u8,
    pub data: Vec<u8>,
}

impl StoredResource {
    /// A resource the *host* supplies rather than one read from a fork: no
    /// attributes and no original name bytes to preserve.
    #[must_use]
    pub fn synthetic(res_type: [u8; 4], id: i16, name: Option<&str>, data: Vec<u8>) -> Self {
        Self {
            res_type,
            id,
            name_bytes: name.map(ad_resource::macroman::encode),
            name: name.map(str::to_owned),
            attrs: 0,
            data,
        }
    }
}

/// The module's resource fork, plus the handles handed out so far.
#[derive(Debug, Default)]
pub struct ResourceStore {
    entries: Vec<StoredResource>,
    /// `(type, id)` → handle, so repeat requests return the same handle.
    loaded: BTreeMap<([u8; 4], i16), u32>,
    /// Resources whose bytes, name or attributes differ from what was loaded.
    ///
    /// Only these are written when the module asks for a durable save. That is
    /// both the Resource Manager's own model (`resChanged`) and the reason the
    /// save file is an *overlay*: the original module is the user's licensed
    /// copy and is never rewritten.
    dirty: std::collections::BTreeSet<([u8; 4], i16)>,
}

impl ResourceStore {
    /// Build a store from decoded resources.
    #[must_use]
    pub fn new(entries: Vec<StoredResource>) -> Self {
        Self {
            entries,
            loaded: BTreeMap::new(),
            dirty: std::collections::BTreeSet::new(),
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The raw bytes of one resource, without allocating an emulated handle.
    ///
    /// For host-side work that needs a resource's contents before, or instead
    /// of, handing it to the module — expanding a compressed code resource with
    /// the `dcmp` the file carries, for one.
    #[must_use]
    pub fn find(&self, res_type: &[u8; 4], id: i16) -> Option<&[u8]> {
        self.entries
            .iter()
            .find(|e| &e.res_type == res_type && e.id == id)
            .map(|e| e.data.as_slice())
    }

    /// `_CountResources`.
    #[must_use]
    pub fn count_of(&self, res_type: &[u8; 4]) -> u16 {
        u16::try_from(
            self.entries
                .iter()
                .filter(|e| &e.res_type == res_type)
                .count(),
        )
        .unwrap_or(u16::MAX)
    }

    /// `_GetResource` / `_Get1Resource`.
    ///
    /// Returns 0 and sets `ResErr` to `resNotFound` when absent, which is what
    /// the real Resource Manager does — modules branch on the nil handle.
    pub fn get(&mut self, mem: &mut Memory, res_type: &[u8; 4], id: i16) -> u32 {
        if let Some(h) = self.loaded.get(&(*res_type, id)) {
            globals::LowMem::set_res_err(mem, 0);
            return *h;
        }
        let Some(entry) = self
            .entries
            .iter()
            .find(|e| &e.res_type == res_type && e.id == id)
        else {
            globals::LowMem::set_res_err(mem, crate::oserr::RES_NOT_FOUND);
            return 0;
        };
        let size = u32::try_from(entry.data.len()).unwrap_or(0);
        let h = mem.new_handle(size.max(1), false);
        if h == 0 {
            globals::LowMem::set_res_err(mem, crate::oserr::MEM_FULL_ERR);
            return 0;
        }
        if let Some(block) = mem.deref_handle(h) {
            let bytes = entry.data.clone();
            mem.write_bytes(block, &bytes);
        }
        self.loaded.insert((*res_type, id), h);
        globals::LowMem::set_res_err(mem, 0);
        h
    }

    /// `_GetIndResource`. The index is 1-based.
    pub fn get_indexed(&mut self, mem: &mut Memory, res_type: &[u8; 4], index: i16) -> u32 {
        if index < 1 {
            globals::LowMem::set_res_err(mem, crate::oserr::RES_NOT_FOUND);
            return 0;
        }
        let mut ids: Vec<i16> = self
            .entries
            .iter()
            .filter(|e| &e.res_type == res_type)
            .map(|e| e.id)
            .collect();
        ids.sort_unstable();
        match ids.get(usize::from(index.unsigned_abs()).saturating_sub(1)) {
            Some(id) => {
                let id = *id;
                self.get(mem, res_type, id)
            }
            None => {
                globals::LowMem::set_res_err(mem, crate::oserr::RES_NOT_FOUND);
                0
            }
        }
    }

    /// `_GetResInfo`: what `(type, id, name)` a loaded handle came from.
    #[must_use]
    pub fn info_for(&self, handle: u32) -> Option<(&StoredResource, [u8; 4], i16)> {
        let ((ty, id), _) = self.loaded.iter().find(|(_, h)| **h == handle)?;
        let e = self
            .entries
            .iter()
            .find(|e| &e.res_type == ty && e.id == *id)?;
        Some((e, *ty, *id))
    }

    /// `_GetNamedResource`. Resource names are MacRoman and compared exactly.
    pub fn get_named(&mut self, mem: &mut Memory, res_type: &[u8; 4], name: &str) -> u32 {
        let id = self
            .entries
            .iter()
            .find(|e| &e.res_type == res_type && e.name.as_deref() == Some(name))
            .map(|e| e.id);
        match id {
            Some(id) => self.get(mem, res_type, id),
            None => {
                globals::LowMem::set_res_err(mem, crate::oserr::RES_NOT_FOUND);
                0
            }
        }
    }

    /// `_DetachResource`. The handle stays valid and owned by the caller, but
    /// stops being tracked as a resource, so a later `_GetResource` reloads a
    /// fresh copy rather than returning the detached block.
    pub fn detach(&mut self, handle: u32) {
        self.loaded.retain(|_, h| *h != handle);
    }

    /// `_AddResource`: adopt `handle`'s bytes as resource `(type, id)`.
    ///
    /// Lunatic Fringe saves high scores exactly this way: remove the old
    /// `LFhs 128`, build a fresh handle, `AddResource`, release. The data is
    /// copied into the store immediately, and `release` syncs again, so the
    /// scores survive the handle.
    pub fn add(
        &mut self,
        mem: &mut Memory,
        res_type: [u8; 4],
        id: i16,
        name_bytes: Option<Vec<u8>>,
        handle: u32,
    ) {
        let Some(block) = mem.deref_handle(handle) else {
            globals::LowMem::set_res_err(mem, crate::oserr::NIL_HANDLE_ERR);
            return;
        };
        let size = mem.handle_size(handle).unwrap_or(0);
        let data = mem.read_bytes(block, size as usize);
        self.entries
            .retain(|e| !(e.res_type == res_type && e.id == id));
        self.entries.push(StoredResource {
            res_type,
            id,
            // The module supplied a Str255, so *its* bytes are authoritative and
            // the decoded form is derived, never the other way round.
            name: name_bytes.as_deref().map(ad_resource::macroman::decode),
            name_bytes,
            // A newly added resource has no attributes set. `_SetResAttrs` is
            // how a module asks for `resChanged`/`resPurgeable`, and it is
            // honoured; inventing them here would be a fabricated fact.
            attrs: 0,
            data,
        });
        self.loaded.insert((res_type, id), handle);
        self.dirty.insert((res_type, id));
        globals::LowMem::set_res_err(mem, 0);
    }

    /// `_GetResAttrs` / `_SetResAttrs` for a loaded handle.
    ///
    /// Fish! is the one module on the disk that sets attributes, and it is the
    /// reason these are not no-ops: a store that discards the attribute byte
    /// cannot write a fork back without losing it.
    pub fn attrs_of(&self, handle: u32) -> Option<u8> {
        let ((ty, id), _) = self.loaded.iter().find(|(_, h)| **h == handle)?;
        self.entries
            .iter()
            .find(|e| &e.res_type == ty && e.id == *id)
            .map(|e| e.attrs)
    }

    /// Set a loaded resource's attribute byte. Returns false if the handle is
    /// not a resource, which is `resNotFound` to the caller.
    pub fn set_attrs(&mut self, handle: u32, attrs: u8) -> bool {
        let Some(((ty, id), _)) = self.loaded.iter().find(|(_, h)| **h == handle) else {
            return false;
        };
        let (ty, id) = (*ty, *id);
        match self
            .entries
            .iter_mut()
            .find(|e| e.res_type == ty && e.id == id)
        {
            Some(e) => {
                let changed = e.attrs != attrs;
                e.attrs = attrs;
                if changed {
                    self.dirty.insert((ty, id));
                }
                true
            }
            None => false,
        }
    }

    /// Replace a loaded resource's bytes from its handle — used by
    /// `_WriteResource`, which writes without ever releasing the handle.
    pub fn set_bytes_of_handle(&mut self, handle: u32, bytes: Vec<u8>) {
        let Some((key, _)) = self.loaded.iter().find(|(_, h)| **h == handle) else {
            return;
        };
        let key = *key;
        if let Some(e) = self.entries.iter_mut().find(|e| (e.res_type, e.id) == key) {
            if e.data != bytes {
                e.data = bytes;
                self.dirty.insert(key);
            }
        }
    }

    /// Mark a loaded handle's resource as changed — `_ChangedResource`.
    pub fn mark_changed(&mut self, handle: u32) {
        if let Some((key, _)) = self.loaded.iter().find(|(_, h)| **h == handle) {
            let key = *key;
            self.dirty.insert(key);
        }
    }

    /// Every resource whose bytes, name or attributes differ from the fork the
    /// module was loaded from — what a durable save writes.
    ///
    /// Returns owned copies rather than references so the caller can hand them
    /// to a writer without borrowing the store across the write.
    #[must_use]
    pub fn changed(&self) -> Vec<StoredResource> {
        self.entries
            .iter()
            .filter(|e| self.dirty.contains(&(e.res_type, e.id)))
            .cloned()
            .collect()
    }

    /// True if anything is waiting to be written.
    #[must_use]
    pub fn has_changes(&self) -> bool {
        !self.dirty.is_empty()
    }

    /// Called once a save has actually reached the disk.
    ///
    /// Deliberately separate from [`Self::changed`]: a write that fails must
    /// leave the resources dirty so the next attempt still has them, rather
    /// than reporting success by forgetting.
    pub fn mark_saved(&mut self) {
        self.dirty.clear();
    }

    /// Every stored resource, for host-side persistence and fork writing.
    #[must_use]
    pub fn entries(&self) -> &[StoredResource] {
        &self.entries
    }

    /// Replace or insert a resource from outside the emulated machine — how a
    /// saved overlay fork is merged back in at load time.
    ///
    /// The entry is marked **dirty**, and that is not an oversight. An overlay
    /// resource differs from the module's own fork by definition, so the next
    /// save has to write it again; leaving it clean would make a save triggered
    /// by some *other* resource write an overlay that no longer contains this
    /// one, silently dropping a previously saved high score.
    pub fn put(&mut self, entry: StoredResource) {
        let key = (entry.res_type, entry.id);
        self.entries.retain(|e| (e.res_type, e.id) != key);
        self.loaded.remove(&key);
        self.entries.push(entry);
        self.dirty.insert(key);
    }

    /// `_RmveResource`: detach the resource this handle came from. The handle
    /// itself stays alive and belongs to the caller afterwards.
    ///
    /// A removal is not recorded as a durable *deletion*. The overlay format is
    /// additive, and every module on the disk that removes a resource — Lunatic
    /// Fringe's high-score save is the pattern — immediately `AddResource`s a
    /// replacement, so the net effect is a change and is saved. A tombstone
    /// format would be untestable against any real module's behaviour, so the
    /// limitation is stated rather than guessed at.
    pub fn remove_by_handle(&mut self, mem: &mut Memory, handle: u32) {
        if let Some(((ty, id), _)) = self.loaded.iter().find(|(_, h)| **h == handle) {
            let (ty, id) = (*ty, *id);
            self.entries.retain(|e| !(e.res_type == ty && e.id == id));
            self.loaded.remove(&(ty, id));
            self.dirty.remove(&(ty, id));
        }
        globals::LowMem::set_res_err(mem, 0);
    }

    /// `_ReleaseResource`. Sync a still-tracked resource's bytes from its
    /// handle (covers writes made after `_AddResource`), then forget the
    /// cached handle so a later `_GetResource` loads it afresh.
    pub fn release(&mut self, mem: &mut Memory, handle: u32) {
        if handle == 0 {
            return;
        }
        if let Some(((ty, id), _)) = self.loaded.iter().find(|(_, h)| **h == handle) {
            let (ty, id) = (*ty, *id);
            if let (Some(block), Some(size)) = (mem.deref_handle(handle), mem.handle_size(handle)) {
                let data = mem.read_bytes(block, size as usize);
                if let Some(e) = self
                    .entries
                    .iter_mut()
                    .find(|e| e.res_type == ty && e.id == id)
                {
                    // The bytes are synced so an in-memory round trip works, but
                    // releasing a resource is **not** a durable change and does
                    // not mark it. The real Resource Manager writes a resource
                    // when the module says so — `_ChangedResource`,
                    // `_WriteResource`, `_AddResource` — and never merely because
                    // its bytes differ from what was loaded.
                    //
                    // Marking here was wrong in a way only a real run showed.
                    // Lunatic Fringe's segment loader patches its own jump table
                    // in place, so `CCOD -2045` comes back 31 KB different from
                    // the fork; the first save was that code segment, not a high
                    // score. Overlaying pre-patched code on the next run is not
                    // saved state, it is a corrupted module.
                    e.data = data;
                }
            }
        }
        self.loaded.retain(|_, h| *h != handle);
        mem.dispose_handle(handle);
        globals::LowMem::set_res_err(mem, 0);
    }

    /// A stored resource's current bytes, for host-side persistence.
    #[must_use]
    pub fn bytes_of(&self, res_type: &[u8; 4], id: i16) -> Option<&[u8]> {
        self.entries
            .iter()
            .find(|e| &e.res_type == res_type && e.id == id)
            .map(|e| e.data.as_slice())
    }
}

/// Resources the System file provided that modules ask for by ID.
///
/// `KCHR 0` is the US keyboard layout. Lunatic Fringe fetches it twelve times
/// while starting a game: its input pipeline translates every virtual key code
/// through it before comparing against the configured control keys, so without
/// a KCHR the game runs but no control key is ever recognised.
#[must_use]
pub fn system_resources() -> Vec<StoredResource> {
    vec![StoredResource::synthetic(
        *b"KCHR",
        0,
        Some("U.S."),
        us_kchr(),
    )]
}

/// The character a virtual key code produces under the US layout — the same
/// mapping the synthesized `KCHR` carries. Used to fill the character byte of
/// synthesized keyDown/keyUp event messages.
#[must_use]
pub fn us_char_for(code: u8) -> u8 {
    *US_TABLE.get(usize::from(code)).unwrap_or(&0)
}

static US_TABLE: std::sync::LazyLock<[u8; 128]> = std::sync::LazyLock::new(build_us_table);

/// Build a minimal US `KCHR`: version, 256 modifier-state table selectors
/// (all pointing at table 0), one 128-entry keycode→character table, and no
/// dead keys.
fn us_kchr() -> Vec<u8> {
    let table = *US_TABLE;
    let mut out = Vec::with_capacity(2 + 256 + 2 + 128 + 2);
    out.extend_from_slice(&0u16.to_be_bytes()); // version
    out.extend_from_slice(&[0u8; 256]); // every modifier state -> table 0
    out.extend_from_slice(&1u16.to_be_bytes()); // one table
    out.extend_from_slice(&table);
    out.extend_from_slice(&0u16.to_be_bytes()); // no dead keys
    out
}

/// Byte offsets inside a `KCHR`, stated once.
pub mod kchr {
    /// `version`, a word.
    pub const VERSION: u32 = 0;
    /// 256 bytes: modifier byte -> which key table to use.
    pub const SELECTORS: u32 = 2;
    /// A word: how many key tables follow.
    pub const TABLE_COUNT: u32 = SELECTORS + 256;
    /// The first key table. Each is 128 bytes, indexed by virtual key code.
    pub const TABLES: u32 = TABLE_COUNT + 2;
    /// Bytes in one key table.
    pub const TABLE_LEN: u32 = 128;
}

/// The character a `KCHR` maps `code` to under `modifiers`.
///
/// This is what `_KeyTranslate` ($A9C3) does: pick a key table using the
/// modifier byte as an index into the 256-entry selector array, then index that
/// table by the virtual key code. Returns `None` when the layout is malformed or
/// the code is out of range, so a caller reports nothing rather than a wrong key.
#[must_use]
pub fn kchr_char(layout: &[u8], modifiers: u8, code: u8) -> Option<u8> {
    let at = |o: u32| -> Option<u8> { layout.get(o as usize).copied() };
    let selector = at(kchr::SELECTORS + u32::from(modifiers))?;
    let hi = at(kchr::TABLE_COUNT)?;
    let lo = at(kchr::TABLE_COUNT + 1)?;
    let count = u32::from(u16::from_be_bytes([hi, lo]));
    // A selector past the last table is a malformed layout, not table zero:
    // guessing would silently produce the wrong character.
    if u32::from(selector) >= count {
        return None;
    }
    // Bit 7 of the key code is the key-up flag, not part of the code.
    let index = u32::from(code & 0x7F);
    at(kchr::TABLES + u32::from(selector) * kchr::TABLE_LEN + index)
}

fn build_us_table() -> [u8; 128] {
    let mut table = [0u8; 128];
    // The classic US layout, virtual key code → MacRoman character.
    let pairs: &[(u8, u8)] = &[
        (0x00, b'a'),
        (0x01, b's'),
        (0x02, b'd'),
        (0x03, b'f'),
        (0x04, b'h'),
        (0x05, b'g'),
        (0x06, b'z'),
        (0x07, b'x'),
        (0x08, b'c'),
        (0x09, b'v'),
        (0x0B, b'b'),
        (0x0C, b'q'),
        (0x0D, b'w'),
        (0x0E, b'e'),
        (0x0F, b'r'),
        (0x10, b'y'),
        (0x11, b't'),
        (0x12, b'1'),
        (0x13, b'2'),
        (0x14, b'3'),
        (0x15, b'4'),
        (0x16, b'6'),
        (0x17, b'5'),
        (0x18, b'='),
        (0x19, b'9'),
        (0x1A, b'7'),
        (0x1B, b'-'),
        (0x1C, b'8'),
        (0x1D, b'0'),
        (0x1E, b']'),
        (0x1F, b'o'),
        (0x20, b'u'),
        (0x21, b'['),
        (0x22, b'i'),
        (0x23, b'p'),
        (0x24, 0x0D),
        (0x25, b'l'),
        (0x26, b'j'),
        (0x27, b'\''),
        (0x28, b'k'),
        (0x29, b';'),
        (0x2A, b'\\'),
        (0x2B, b','),
        (0x2C, b'/'),
        (0x2D, b'n'),
        (0x2E, b'm'),
        (0x2F, b'.'),
        (0x30, 0x09),
        (0x31, b' '),
        (0x32, b'`'),
        (0x33, 0x08),
        (0x34, 0x03),
        (0x35, 0x1B),
        // Keypad.
        (0x41, b'.'),
        (0x43, b'*'),
        (0x45, b'+'),
        (0x47, 0x1B),
        (0x4B, b'/'),
        (0x4C, 0x03),
        (0x4E, b'-'),
        (0x51, b'='),
        (0x52, b'0'),
        (0x53, b'1'),
        (0x54, b'2'),
        (0x55, b'3'),
        (0x56, b'4'),
        (0x57, b'5'),
        (0x58, b'6'),
        (0x59, b'7'),
        (0x5B, b'8'),
        (0x5C, b'9'),
        // Arrows, as the classic control characters.
        (0x7B, 0x1C),
        (0x7C, 0x1D),
        (0x7E, 0x1E),
        (0x7D, 0x1F),
    ];
    for &(code, ch) in pairs {
        table[usize::from(code)] = ch;
    }
    table
}
