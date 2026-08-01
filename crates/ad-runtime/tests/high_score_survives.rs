//! The persistence gate, end to end: a module saves, the process ends, a fresh
//! runtime reads the save back.
//!
//! This drives the **real trap path** — `_NewHandle`, `_RmveResource`,
//! `_AddResource`, `_UpdateResFile` — in the order Lunatic Fringe's high-score
//! save uses, against a real [`ForkSink`] on a real directory.
//! A test that called `ForkSink::persist` directly would prove the writer works
//! and say nothing about whether a module's traps ever reach it.

use ad_m68k::{Bus as _, Registers};
use ad_runtime::ForkSink;
use ad_toolbox::Toolbox;
use ad_toolbox::resources::{ResourceStore, StoredResource};
use std::path::PathBuf;

/// Stack pointer for the synthetic Pascal calls, well clear of the heap.
const SP: u32 = 0x0007_0000;

#[derive(Debug, Default)]
struct Regs {
    d: [u32; 8],
    a: [u32; 8],
    sp: u32,
    resume: Option<u32>,
    ccr: Option<u8>,
}

impl Registers for Regs {
    fn data(&self, n: u8) -> u32 {
        self.d[usize::from(n) & 7]
    }
    fn set_data(&mut self, n: u8, v: u32) {
        self.d[usize::from(n) & 7] = v;
    }
    fn addr(&self, n: u8) -> u32 {
        self.a[usize::from(n) & 7]
    }
    fn set_addr(&mut self, n: u8, v: u32) {
        self.a[usize::from(n) & 7] = v;
    }
    fn sp(&self) -> u32 {
        self.sp
    }
    fn set_sp(&mut self, v: u32) {
        self.sp = v;
    }
    fn trap_pc(&self) -> u32 {
        0x1000
    }
    fn set_resume_pc(&mut self, pc: u32) {
        self.resume = Some(pc);
    }
    fn resume_pc(&self) -> Option<u32> {
        self.resume
    }
    fn set_condition_codes(&mut self, c: u8) {
        self.ccr = Some(c);
    }
    fn condition_codes(&self) -> Option<u8> {
        self.ccr
    }
}

/// One Pascal call: push the arguments (left to right), run the trap, and hand
/// back the stack pointer, which is where a result would be.
fn call(tb: &mut Toolbox, trap: u16, args: &[Arg]) -> u32 {
    let mut sp = SP;
    for a in args {
        match a {
            Arg::W(v) => {
                sp -= 2;
                tb.mem.write_u16(sp, *v as u16);
            }
            Arg::L(v) => {
                sp -= 4;
                tb.mem.write_u32(sp, *v);
            }
        }
    }
    let mut regs = Regs {
        sp,
        ..Default::default()
    };
    tb.trap(trap, &mut regs)
        .unwrap_or_else(|e| panic!("{trap:#06x}: {}", e.detail));
    regs.sp()
}

enum Arg {
    W(i16),
    L(u32),
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ad-persist-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// The shipped default the module would find with nothing saved.
fn shipped_defaults() -> Vec<StoredResource> {
    vec![StoredResource::synthetic(
        *b"LFhs",
        128,
        Some("High Scores"),
        vec![0u8; 8],
    )]
}

#[test]
fn a_high_score_written_through_the_traps_survives_a_fresh_runtime() {
    let dir = scratch("roundtrip");
    let title = "Lunatic Fringe";

    // ---- session one: the module saves ----
    let saved_bytes: Vec<u8> = vec![0x00, 0x64, b'R', b'U', b'M', b'I', 0x00, 0x01];
    {
        let mut tb = Toolbox::new();
        tb.resources = ResourceStore::new(shipped_defaults());
        tb.sink = Some(Box::new(ForkSink::new(&dir, title)));

        // Get1Resource('LFhs', 128) -> the shipped default.
        let slot = call(
            &mut tb,
            0xA81F,
            &[Arg::L(0), Arg::L(u32::from_be_bytes(*b"LFhs")), Arg::W(128)],
        );
        let old = tb.mem.read_u32(slot);
        assert_ne!(old, 0, "the shipped default must load");

        // RmveResource(old); then build a fresh handle and AddResource it — the
        // exact sequence in Lunatic Fringe's save routine.
        call(&mut tb, 0xA9AD, &[Arg::L(old)]);
        let h = tb.mem.new_handle(saved_bytes.len() as u32, false);
        let block = tb.mem.deref_handle(h).expect("fresh handle");
        tb.mem.write_bytes(block, &saved_bytes);

        // AddResource(h, 'LFhs', 128, name) with a real Str255.
        let name_at = 0x0006_0000u32;
        tb.mem.write_u8(name_at, 11);
        tb.mem.write_bytes(name_at + 1, b"High Scores");
        call(
            &mut tb,
            0xA9AB,
            &[
                Arg::L(h),
                Arg::L(u32::from_be_bytes(*b"LFhs")),
                Arg::W(128),
                Arg::L(name_at),
            ],
        );

        // UpdateResFile(refNum) — the durable write.
        call(&mut tb, 0xA999, &[Arg::W(0)]);
        assert_eq!(
            tb.mem.read_u16(ad_memory::globals::RES_ERR) as i16,
            0,
            "UpdateResFile must report noErr"
        );
    }

    // The file exists, and it is a resource fork the ordinary parser reads.
    let sink = ForkSink::new(&dir, title);
    assert!(
        sink.path().exists(),
        "{} was not written",
        sink.path().display()
    );

    // ---- session two: a fresh runtime finds the saved score ----
    {
        let mut tb = Toolbox::new();
        tb.resources = ResourceStore::new(shipped_defaults());
        let overlay = ForkSink::load(&dir, title).expect("load overlay");
        assert_eq!(overlay.len(), 1, "only the changed resource is saved");
        assert_eq!(
            overlay[0].name_bytes.as_deref(),
            Some(b"High Scores".as_slice()),
            "the module's own Str255 bytes must survive the round trip"
        );
        for entry in overlay {
            tb.resources.put(entry);
        }
        let slot = call(
            &mut tb,
            0xA81F,
            &[Arg::L(0), Arg::L(u32::from_be_bytes(*b"LFhs")), Arg::W(128)],
        );
        let h = tb.mem.read_u32(slot);
        assert_ne!(h, 0, "the saved resource must load");
        let block = tb.mem.deref_handle(h).expect("deref");
        let got = tb.mem.read_bytes(block, saved_bytes.len());
        assert_eq!(got, saved_bytes, "the saved bytes must come back exactly");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn nothing_is_written_when_no_sink_is_installed() {
    // The lab's configuration. A module must not fail because the host gave it
    // nowhere to save, and it must not leave files behind either.
    let dir = scratch("nosink");
    let mut tb = Toolbox::new();
    tb.resources = ResourceStore::new(shipped_defaults());

    let slot = call(
        &mut tb,
        0xA81F,
        &[Arg::L(0), Arg::L(u32::from_be_bytes(*b"LFhs")), Arg::W(128)],
    );
    let h = tb.mem.read_u32(slot);
    let block = tb.mem.deref_handle(h).expect("deref");
    tb.mem.write_bytes(block, &[9u8; 8]);
    call(&mut tb, 0xA9AA, &[Arg::L(h)]); // ChangedResource
    call(&mut tb, 0xA9B0, &[Arg::L(h)]); // WriteResource
    assert_eq!(
        tb.mem.read_u16(ad_memory::globals::RES_ERR) as i16,
        0,
        "a module with no save location must still see noErr"
    );
    assert!(!dir.exists(), "no directory should have been created");
}

#[test]
fn releasing_a_resource_is_not_a_durable_change_even_if_its_bytes_differ() {
    // The Resource Manager writes a resource when the module *says* so —
    // `_ChangedResource`, `_WriteResource`, `_AddResource` — never merely because
    // its in-memory bytes differ from what was loaded.
    //
    // A real run is what showed why this matters. Lunatic Fringe's segment loader
    // patches its own jump table in place, so releasing `CCOD -2045` hands back
    // 31 KB that differ from the fork. Marking that dirty made the very first
    // save a *code segment* instead of a high score, and overlaying pre-patched
    // code on the next run is a corrupted module, not saved state.
    let mut tb = Toolbox::new();
    tb.resources = ResourceStore::new(shipped_defaults());
    let slot = call(
        &mut tb,
        0xA81F,
        &[Arg::L(0), Arg::L(u32::from_be_bytes(*b"LFhs")), Arg::W(128)],
    );
    let h = tb.mem.read_u32(slot);
    assert!(!tb.resources.has_changes(), "loading is not a change");

    // Write through the handle, as self-patching code does, and release.
    let block = tb.mem.deref_handle(h).expect("deref");
    tb.mem
        .write_bytes(block, &[0x4E, 0x71, 0x4E, 0x75, 0, 0, 0, 0]);
    call(&mut tb, 0xA9A3, &[Arg::L(h)]); // ReleaseResource
    assert!(
        !tb.resources.has_changes(),
        "a patched-and-released resource must not be persisted"
    );

    // …but the bytes *are* synced, so an in-memory reload sees them. That sync is
    // what `_AddResource` followed by a write and a release depends on.
    let slot = call(
        &mut tb,
        0xA81F,
        &[Arg::L(0), Arg::L(u32::from_be_bytes(*b"LFhs")), Arg::W(128)],
    );
    let h2 = tb.mem.read_u32(slot);
    let block = tb.mem.deref_handle(h2).expect("deref");
    assert_eq!(tb.mem.read_bytes(block, 4), vec![0x4E, 0x71, 0x4E, 0x75]);
}

#[test]
fn a_write_resource_call_persists_bytes_changed_through_a_live_handle() {
    // `_WriteResource` is the case the release-time sync cannot cover: the module
    // writes through the handle and never releases it.
    let dir = scratch("writeresource");
    let title = "Fish!";
    {
        let mut tb = Toolbox::new();
        tb.resources = ResourceStore::new(shipped_defaults());
        tb.sink = Some(Box::new(ForkSink::new(&dir, title)));
        let slot = call(
            &mut tb,
            0xA81F,
            &[Arg::L(0), Arg::L(u32::from_be_bytes(*b"LFhs")), Arg::W(128)],
        );
        let h = tb.mem.read_u32(slot);
        let block = tb.mem.deref_handle(h).expect("deref");
        tb.mem.write_bytes(block, &[0xEE; 8]);
        call(&mut tb, 0xA9B0, &[Arg::L(h)]); // WriteResource
    }
    let back = ForkSink::load(&dir, title).expect("load");
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].data, vec![0xEE; 8]);
    let _ = std::fs::remove_dir_all(&dir);
}
