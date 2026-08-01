//! Ground-truth tests against the real Lunatic Fringe resource fork.
//!
//! Every expected value here was independently measured from
//! `AfterDark-original.img` or comes from Berkeley
//! Systems' own SDK. If the parser drifts, these fail.

#![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use ad_resource::{
    AdModule, Capabilities, CodeLayout, Control, GmMessage, ModuleSettings, ResourceFork,
    SegmentHeader,
    settings::{CONTROL_ID_BASE, is_settings_type},
};
use std::path::PathBuf;

/// 385,673 bytes, SHA-256
/// `8ac5c55c4971cb023ef9a1f1b889d51fc61baee0eab216d60e941b76525ca0e4`. Not
/// shipped — Berkeley Systems' copyrighted resource fork, kept only in a
/// gitignored local cache. See `reference/README.md` for how to reconstitute it.
fn fork_bytes() -> Option<Vec<u8>> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../reference/lunatic-fringe/Lunatic Fringe.rsrc");
    std::fs::read(&p).ok()
}

macro_rules! fork_bytes_or_skip {
    () => {
        match fork_bytes() {
            Some(b) => b,
            None => {
                println!("skipped: reference/lunatic-fringe/Lunatic Fringe.rsrc not present");
                return;
            }
        }
    };
}

#[test]
fn fork_is_the_expected_size() {
    let bytes = fork_bytes_or_skip!();
    assert_eq!(bytes.len(), 385_673, "resource fork size changed");
}

#[test]
fn parses_all_109_resources_with_expected_type_counts() {
    let bytes = fork_bytes_or_skip!();
    let fork = ResourceFork::parse(&bytes).expect("parse");

    assert_eq!(fork.len(), 109, "total resource count");
    assert_eq!(fork.types().len(), 20, "distinct resource types");

    // Measured counts from the disk.
    for (ty, want) in [
        (b"Manm", 62),
        (b"snd ", 21),
        (b"CCOD", 3),
        (b"ADgm", 1),
        (b"PICT", 2),
        (b"clut", 1),
        (b"DITL", 3),
        (b"TEXT", 1),
        (b"LFky", 1),
        (b"vers", 2),
        (b"STR ", 1),
        (b"sUnt", 1),
        (b"ALRT", 2),
        (b"DLOG", 1),
        (b"sysz", 1),
        (b"bVal", 2),
        (b"sReq", 1),
        (b"Ignr", 1),
        (b"Chnl", 1),
        (b"sVal", 1),
    ] {
        assert_eq!(
            fork.count_of(ty),
            want,
            "count of {:?}",
            String::from_utf8_lossy(ty)
        );
    }

    // Payload byte totals for the two biggest types.
    let manm: usize = fork.of_type(b"Manm").iter().map(|r| r.data.len()).sum();
    let snd: usize = fork.of_type(b"snd ").iter().map(|r| r.data.len()).sum();
    assert_eq!(manm, 213_284, "Manm payload bytes");
    assert_eq!(snd, 86_722, "snd payload bytes");
}

#[test]
fn code_resource_is_found_by_type_not_id() {
    let bytes = fork_bytes_or_skip!();
    let module = AdModule::new(ResourceFork::parse(&bytes).expect("parse"));

    let code = module.code().expect("ADgm present");
    assert_eq!(code.id, 0, "Lunatic Fringe uses ADgm 0");
    assert_eq!(code.name.as_deref(), Some("Asteroids"));
    assert_eq!(code.data.len(), 23_752);
}

#[test]
fn code_header_and_entry_point_resolve() {
    let bytes = fork_bytes_or_skip!();
    let module = AdModule::new(ResourceFork::parse(&bytes).expect("parse"));
    let layout = module.code_layout().expect("layout");

    let header = layout
        .header
        .expect("Lunatic Fringe has the 16-byte ADgm header");
    assert_eq!(
        header.declared_id, 0,
        "header-declared id should match the resource id"
    );
    assert!(
        layout.resolved_via_stub,
        "the LEA/NOP/NOP/BRA.W stub should resolve the entry point"
    );
    // BRA.W at +24 with displacement +0x02E2 measured from the disassembly.
    assert_eq!(layout.entry_offset, 26 + 0x02E2);
    assert!(layout.entry_offset < module.code().unwrap().data.len());
}

#[test]
fn bare_code_resource_has_no_header_and_entry_at_zero() {
    // Hard Rain's shape: Pascal prologue `LINK A6,#-26` at offset 0.
    let bare = [0x4E, 0x56, 0xFF, 0xE6, 0x48, 0xE7, 0x1F, 0x3C];
    let layout = CodeLayout::detect(&bare);
    assert!(layout.header.is_none());
    assert_eq!(layout.entry_offset, 0);
    assert!(!layout.resolved_via_stub);
}

#[test]
fn segment_jump_table_chain_is_intact() {
    let bytes = fork_bytes_or_skip!();
    let module = AdModule::new(ResourceFork::parse(&bytes).expect("parse"));

    let segs = module.segments();
    assert_eq!(segs.len(), 3, "three CCOD segments");
    // Ascending id order is -2045, -2044, -2043.
    assert_eq!(
        segs.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![-2045, -2044, -2043]
    );

    let headers: Vec<SegmentHeader> = segs
        .iter()
        .map(|r| SegmentHeader::parse(r.data).expect("segment header"))
        .collect();
    assert_eq!(
        headers
            .iter()
            .map(|h| (h.jump_table_offset, h.entry_count))
            .collect::<Vec<_>>(),
        vec![(384, 33), (648, 17), (784, 3)],
        "measured jump-table offsets and entry counts"
    );

    // offset(n+1) == offset(n) + 8 * count(n)
    module
        .verify_segment_chain()
        .expect("jump table chain must be contiguous");
}

#[test]
fn segment_chain_arithmetic() {
    // Each jump table entry is 8 bytes, so the next segment's declared offset
    // must be offset + 8 * count. These are Lunatic Fringe's real values.
    let a = SegmentHeader {
        jump_table_offset: 384,
        entry_count: 33,
    };
    assert_eq!(a.table_bytes(), 264);
    assert_eq!(a.next_offset(), 648);

    let b = SegmentHeader {
        jump_table_offset: 648,
        entry_count: 17,
    };
    assert_eq!(b.next_offset(), 784);

    // Clock's six-segment chain, also measured from the disk.
    let clock = [
        (680u16, 19u16),
        (832, 4),
        (864, 4),
        (896, 11),
        (984, 7),
        (1040, 7),
    ];
    for w in clock.windows(2) {
        let (off, count) = w[0];
        let (next_off, _) = w[1];
        let h = SegmentHeader {
            jump_table_offset: off,
            entry_count: count,
        };
        assert_eq!(
            h.next_offset(),
            u32::from(next_off),
            "chain break after offset {off}"
        );
    }

    // A header at the end of the table must not overflow.
    let max = SegmentHeader {
        jump_table_offset: u16::MAX,
        entry_count: u16::MAX,
    };
    assert_eq!(max.next_offset(), 65_535 + 8 * 65_535);
}

#[test]
fn segment_header_rejects_short_data() {
    assert_eq!(SegmentHeader::parse(&[]), None);
    assert_eq!(SegmentHeader::parse(&[0x01, 0x80, 0x00]), None);
    assert_eq!(
        SegmentHeader::parse(&[0x01, 0x80, 0x00, 0x21]),
        Some(SegmentHeader {
            jump_table_offset: 384,
            entry_count: 33
        })
    );
}

#[test]
fn settings_decode_matches_the_sdk_spec() {
    let bytes = fork_bytes_or_skip!();
    let fork = ResourceFork::parse(&bytes).expect("parse");
    let s = ModuleSettings::from_fork(&fork);

    // sVal 1003 "Starting Level:" -> slot 3
    let slot3 = s.controls[3].as_ref().expect("slot 3 populated");
    match slot3 {
        Control::Slider {
            label,
            value,
            units,
        } => {
            assert_eq!(label.as_deref(), Some("Starting Level:"));
            assert_eq!(*value, 0);
            // sUnt 1003 labels the ten levels 1..10.
            assert_eq!(units.len(), 10, "ten slider unit labels");
            assert_eq!(units[0].text, "1");
            assert_eq!(units[0].lower_limit, 0);
            assert_eq!(units[9].text, "10");
            assert_eq!(units[9].lower_limit, 90);
        }
        other => panic!("expected a slider in slot 3, got {other:?}"),
    }

    // bVal 1000 "Clear Scores…" = 9, bVal 1001 "Keys…" = 8.
    // ButtonMessage is 8, so these are buttons 1 and 2.
    let slot0 = s.controls[0].as_ref().expect("slot 0 populated");
    let slot1 = s.controls[1].as_ref().expect("slot 1 populated");
    assert!(matches!(slot0, Control::Button { message: 9, .. }));
    assert!(matches!(slot1, Control::Button { message: 8, .. }));
    assert_eq!(slot0.label(), Some("Clear Scores…"));
    assert_eq!(slot1.label(), Some("Keys…"));

    let mut buttons = s.buttons();
    buttons.sort_by_key(|(m, _)| *m);
    assert_eq!(
        buttons,
        vec![(8, Some("Keys…")), (9, Some("Clear Scores…"))],
        "button selectors must be ButtonMessage-relative"
    );
    assert!(buttons.iter().all(|(m, _)| *m >= GmMessage::BUTTON_BASE));

    // Chnl 0 "Sound" = channel kind 5, volume 1.
    let sound = s.sound.expect("Chnl 0 present");
    assert_eq!(sound.channel_kind, 5);
    assert_eq!(sound.volume, 1);

    // No Cals resource, so only the original four selectors are supported.
    assert!(fork.get(b"Cals", 0).is_none());
    assert_eq!(s.capabilities, Capabilities::default());
    assert!(s.capabilities.draw_frame && !s.capabilities.do_about);

    // sysz 128 is off-spec (should be 0 or 1) — do not size RAM from it.
    assert_eq!(s.memory.desired, None);
    assert_eq!(s.memory.minimum, None);
    assert_eq!(s.memory.off_spec, vec![(128, 0x0000_6400)]);

    // controlValues[] as marshalled into GMParamBlock.
    assert_eq!(s.control_values(), [9, 8, 0, 0]);
}

#[test]
fn control_slot_maps_to_resource_id() {
    assert_eq!(CONTROL_ID_BASE, 1000);
    let bytes = fork_bytes_or_skip!();
    let fork = ResourceFork::parse(&bytes).expect("parse");
    // The one sVal is id 1003 and must land in slot 3.
    let sval = fork.of_type(b"sVal");
    assert_eq!(sval.len(), 1);
    assert_eq!(sval[0].id, 1003);
    let s = ModuleSettings::from_fork(&fork);
    assert!(s.controls[3].is_some());
    assert!(s.controls[2].is_none(), "slots may be sparse");
}

#[test]
fn descriptor_and_strings_decode_macroman() {
    let bytes = fork_bytes_or_skip!();
    let fork = ResourceFork::parse(&bytes).expect("parse");

    // STR  128 "credits" is a Pascal string with a © in it.
    let credits = fork.get(b"STR ", 128).expect("STR 128");
    assert_eq!(credits.name.as_deref(), Some("credits"));
    let (text, _) = ad_resource::macroman::pascal_string(credits.data, 0).expect("pascal");
    assert!(
        text.starts_with("Lunatic Fringe by Ben Haller."),
        "got {text:?}"
    );
    assert!(text.contains('©'), "MacRoman 0xA9 must decode to ©");

    // The help TEXT confirms the Caps Lock behaviour and the ~600K requirement.
    let help = fork.get(b"TEXT", 1000).expect("TEXT 1000");
    let help = ad_resource::macroman::decode(help.data);
    assert!(help.contains("Caps Lock"));
    assert!(help.contains("600K"));
    assert!(help.contains("Sleep Now"));
}

#[test]
fn settings_types_are_recognised() {
    assert!(is_settings_type(b"sVal"));
    assert!(is_settings_type(b"bVal"));
    assert!(is_settings_type(b"Chnl"));
    assert!(
        is_settings_type(b"\xB5Val"),
        "micro-sign type must be recognised"
    );
    assert!(!is_settings_type(b"Manm"));
    assert!(!is_settings_type(b"snd "));
}

#[test]
fn resource_type_filenames_are_portable() {
    use ad_resource::macroman::type_to_filename;
    let bytes = fork_bytes_or_skip!();
    let fork = ResourceFork::parse(&bytes).expect("parse");
    let mut names: Vec<String> = fork.types().iter().map(type_to_filename).collect();
    let count = names.len();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), count, "encoded type names must stay distinct");
    for n in &names {
        assert!(
            n.chars().all(|c| c.is_ascii_alphanumeric() || c == '%'),
            "unsafe filename {n:?}"
        );
    }
    assert!(names.contains(&"snd%20".to_string()), "got {names:?}");
}

// ---------------------------------------------------------------- malformed input

#[test]
fn rejects_truncated_fork() {
    assert!(ResourceFork::parse(&[0u8; 8]).is_err());
    assert!(ResourceFork::parse(&[]).is_err());
}

#[test]
fn rejects_out_of_range_header_offsets() {
    // dataOffset/mapOffset point far beyond the buffer.
    let mut b = vec![0u8; 32];
    b[0..4].copy_from_slice(&0xFFFF_0000u32.to_be_bytes());
    assert!(ResourceFork::parse(&b).is_err());
}

#[test]
fn never_panics_on_truncation() {
    // Every prefix of a real fork must error or parse, never panic.
    let bytes = fork_bytes_or_skip!();
    for len in [16, 17, 64, 255, 1024, 4096, 65_536, 200_000, 385_672] {
        let slice = &bytes[..len.min(bytes.len())];
        let _ = ResourceFork::parse(slice);
    }
}

#[test]
fn never_panics_on_corrupted_bytes() {
    // Flip bytes across the map area and confirm no panic.
    let original = fork_bytes_or_skip!();
    for pos in (0..original.len()).step_by(4093) {
        let mut b = original.clone();
        b[pos] ^= 0xFF;
        let _ = ResourceFork::parse(&b);
    }
}

#[test]
fn empty_resource_map_yields_zero_resources() {
    // A real empty fork stores `numTypes - 1` as -1 (0xFFFF). Naively adding one
    // to the unsigned value asks for 65536 type entries; the correct answer is 0.
    // Layout mirrors `DesktopPrinters DB` from the After Dark disk.
    let mut b = vec![0u8; 286];
    b[0..4].copy_from_slice(&256u32.to_be_bytes()); // dataOffset
    b[4..8].copy_from_slice(&256u32.to_be_bytes()); // mapOffset
    b[8..12].copy_from_slice(&0u32.to_be_bytes()); // dataLength
    b[12..16].copy_from_slice(&30u32.to_be_bytes()); // mapLength
    b[256 + 24..256 + 26].copy_from_slice(&28u16.to_be_bytes()); // typeListOffset
    b[256 + 26..256 + 28].copy_from_slice(&30u16.to_be_bytes()); // nameListOffset
    b[256 + 28..256 + 30].copy_from_slice(&0xFFFFu16.to_be_bytes()); // numTypes - 1 == -1

    let fork = ResourceFork::parse(&b).expect("an empty map is valid, not an error");
    assert!(fork.is_empty());
    assert_eq!(fork.len(), 0);
    assert_eq!(fork.types().len(), 0);
    assert!(fork.of_type(b"ADgm").is_empty());
}

/// `LFky` — the module's key configuration, decoded with the layout proven by
/// its own loader and consumer code.
///
/// The loader (CCOD -2043 +0x878) does
/// `BlockMove(h, primary, 0x38); BlockMove(h+0x38, alternate, 0x38)`,
/// and both scanners index with `asl.l #3` over 7 entries, so the resource is
/// two 7-entry tables of 8-byte records:
///
/// * `[0]` kind — `0` matched by keycode through the patched `_PostEvent`,
///   nonzero matched by mask against the low-memory `KeyMap`
/// * `[1]` character, `[2]` keycode, `[4..8]` `KeyMap` mask
///
/// Function order is fixed by which flag each consumer reads out of the
/// pressed-array at `A4+0x3c54`.
#[test]
fn key_configuration_decodes_to_the_shipped_bindings() {
    let bytes = fork_bytes_or_skip!();
    let fork = ResourceFork::parse(&bytes).expect("parse");
    let lfky = fork
        .all()
        .iter()
        .find(|r| &r.res_type == b"LFky")
        .expect("LFky 128 present");
    assert_eq!(lfky.data.len(), 112, "two 0x38-byte tables");

    let entry = |table: usize, f: usize| -> (u8, u8, u8, u32) {
        let at = table * 0x38 + f * 8;
        let e = &lfky.data[at..at + 8];
        let mask = u32::from_be_bytes([e[4], e[5], e[6], e[7]]);
        (e[0], e[1], e[2], mask)
    };

    // (kind, char, keycode, mask) per function, primary table.
    // Verified behaviourally in the lab: thrust drops the collision count from
    // 553 to 205 (the ship leaves its base) and fire then plays 'Normal Shot'.
    assert_eq!(entry(0, 0), (0, b'4', 0x56, 0), "rotate one way: keypad 4");
    assert_eq!(
        entry(0, 1),
        (0, b'6', 0x58, 0),
        "rotate other way: keypad 6"
    );
    assert_eq!(entry(0, 3), (0, b'5', 0x57, 0), "thrust: keypad 5");
    assert_eq!(entry(0, 4), (0, b'8', 0x5B, 0), "super-thrust: keypad 8");
    assert_eq!(entry(0, 5), (0, b'0', 0x52, 0), "shield: keypad 0");
    assert_eq!(entry(0, 6), (0, b'a', 0x00, 0), "abort: A");

    // Alternate bindings, the letter cluster.
    assert_eq!(entry(1, 0), (0, b'l', 0x25, 0));
    assert_eq!(entry(1, 1), (0, b'\'', 0x27, 0));
    assert_eq!(entry(1, 3), (0, b';', 0x29, 0), "thrust: semicolon");
    assert_eq!(entry(1, 4), (0, b'p', 0x23, 0));
    assert_eq!(entry(1, 5), (0, b' ', 0x31, 0), "shield: space");
    assert_eq!(entry(1, 6), (0, b'a', 0x00, 0));

    // Fire is the odd one out in both tables: kind 1, so it is matched against
    // the KeyMap rather than delivered as an event. Bit 15 of the long at
    // $178 is byte $17A bit 7, i.e. keycode 0x37 — the Command key.
    for table in 0..2 {
        let (kind, _, code, mask) = entry(table, 2);
        assert_eq!(kind, 1, "fire is KeyMap-matched");
        assert_eq!(code, 0, "fire carries no keycode");
        assert_eq!(mask, 0x0000_8000, "fire mask selects keycode 0x37");
    }
}
