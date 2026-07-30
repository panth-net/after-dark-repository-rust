//! The writer against real forks: rebuild one and check that nothing was lost.
//!
//! A synthetic round-trip test proves the writer is self-consistent. It does not
//! prove the writer preserves what the *shipped* forks actually contain —
//! attribute bytes that are set, names with MacRoman high bytes, empty
//! payloads, ids at both ends of `i16`, several hundred resources of one type.
//! Lunatic Fringe's 109 resources are the real sample, and they are checked
//! field by field rather than by comparing whole files, because the writer
//! normalises order and shares names on purpose.

use ad_resource::{write_fork, OwnedResource, ResourceFork};
use std::path::PathBuf;

/// Not shipped — Berkeley Systems' copyrighted resource fork, kept only in a
/// gitignored local cache. See `reference/README.md` for how to reconstitute it.
fn fork_bytes() -> Option<Vec<u8>> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../reference/lunatic-fringe/Lunatic Fringe.rsrc");
    std::fs::read(&p).ok()
}

#[test]
fn rebuilding_a_real_fork_loses_nothing() {
    let Some(bytes) = fork_bytes() else {
        println!("skipped: reference/lunatic-fringe/Lunatic Fringe.rsrc not present");
        return;
    };
    let original = ResourceFork::parse(&bytes).expect("parse original");

    let owned: Vec<OwnedResource> = original
        .all()
        .iter()
        .map(|r| OwnedResource {
            res_type: r.res_type,
            id: r.id,
            name_bytes: r.name_bytes.map(<[u8]>::to_vec),
            attrs: r.attrs,
            data: r.data.to_vec(),
        })
        .collect();

    let rebuilt_bytes = write_fork(&owned).expect("write");
    let rebuilt = ResourceFork::parse(&rebuilt_bytes).expect("parse rebuilt");

    assert_eq!(rebuilt.len(), original.len(), "resource count");
    for r in original.all() {
        let got = rebuilt
            .get(&r.res_type, r.id)
            .unwrap_or_else(|| panic!("'{}' {} disappeared", r.type_str(), r.id));
        assert_eq!(got.data, r.data, "'{}' {} payload", r.type_str(), r.id);
        assert_eq!(got.attrs, r.attrs, "'{}' {} attrs", r.type_str(), r.id);
        assert_eq!(
            got.name_bytes, r.name_bytes,
            "'{}' {} name bytes",
            r.type_str(),
            r.id
        );
    }

    // Rebuilding the rebuild is a fixed point: the writer's own output is
    // already in its canonical order, so a second pass changes nothing. This is
    // what makes a content hash of a saved fork meaningful.
    let owned2: Vec<OwnedResource> = rebuilt
        .all()
        .iter()
        .map(|r| OwnedResource {
            res_type: r.res_type,
            id: r.id,
            name_bytes: r.name_bytes.map(<[u8]>::to_vec),
            attrs: r.attrs,
            data: r.data.to_vec(),
        })
        .collect();
    assert_eq!(write_fork(&owned2).expect("write again"), rebuilt_bytes);
}

#[test]
fn the_sample_actually_exercises_the_hard_cases() {
    // A round-trip test that happens to run over 109 unnamed, unattributed,
    // non-empty resources would pass while proving very little. Assert the
    // sample's shape so this test cannot quietly stop being meaningful.
    let Some(bytes) = fork_bytes() else {
        println!("skipped: reference/lunatic-fringe/Lunatic Fringe.rsrc not present");
        return;
    };
    let fork = ResourceFork::parse(&bytes).expect("parse");
    assert!(
        fork.all().iter().any(|r| r.name_bytes.is_some()),
        "no named resource in the sample"
    );
    assert!(
        fork.all().iter().any(|r| r.attrs != 0),
        "no resource with attributes set in the sample"
    );
    assert!(
        fork.all().iter().any(|r| r.id < 0),
        "no negative id in the sample"
    );
    let types: std::collections::BTreeSet<[u8; 4]> =
        fork.all().iter().map(|r| r.res_type).collect();
    assert!(types.len() > 5, "only {} distinct types", types.len());
}
