//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Structural projections preserve decoded names, legacy defaults and controlled failure cleanup.

use super::*;

#[test]
fn registry_projection_ignores_counters_order_and_equivalent_escaped_names() {
    let control = StorageReadControl::with_limit(16384);
    let old = Registry::decode(
        Some(r#"{"labels":{"P":3,"Q":4},"kinds":{"Q":"e"},"sequences":{"3":7}}"#),
        &control,
    )
    .unwrap()
    .unwrap();
    let new = Registry::decode(Some(r#"{"next_label_id":99,"sequences":{"3":900},"kinds":{"Q":"e","P":"v"},"labels":{"Q":4,"\u0050":3}}"#), &control).unwrap().unwrap();
    old.visit_changes(&new, &control, |_| panic!("no definition changed"))
        .unwrap();
    drop((old, new));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn registry_projection_retains_last_duplicate_and_tracks_id_kind_and_presence() {
    let control = StorageReadControl::with_limit(16384);
    let old = Registry::decode(
        Some(r#"{"labels":{"P":7,"P":3,"Q":4},"kinds":{"P":"e","P":"v"}}"#),
        &control,
    )
    .unwrap()
    .unwrap();
    let same = Registry::decode(Some(r#"{"labels":{"Q":4,"P":3}}"#), &control)
        .unwrap()
        .unwrap();
    old.visit_changes(&same, &control, |_| {
        panic!("last duplicate defines the value")
    })
    .unwrap();
    let new = Registry::decode(
        Some(r#"{"labels":{"P":5,"R":6},"kinds":{"P":"e"},"dropped_label_ids":[1,1,7]}"#),
        &control,
    )
    .unwrap()
    .unwrap();
    let mut changes = Vec::new();
    old.visit_changes(&new, &control, |name| {
        changes.push(*name);
        Ok(())
    })
    .unwrap();
    let mut expected: Vec<[u8; 32]> = ["P", "Q", "R"]
        .iter()
        .map(|name| Sha256::digest(name.as_bytes()).into())
        .collect();
    expected.sort_unstable();
    assert_eq!(changes, expected);
    assert!(new.dropped(1));
    assert!(!new.dropped(2));
    assert!(!old.dropped(1));
}

#[test]
fn registry_projection_preserves_opaque_records_and_releases_failed_reservations() {
    let control = StorageReadControl::with_limit(8192);
    for source in [
        "opaque",
        "[]",
        r#"{"labels":{"P":"bad"}}"#,
        r#"{"kinds":{"P":"invalid"}}"#,
        r#"{"dropped_label_ids":["bad"]}"#,
    ] {
        assert!(Registry::decode(Some(source), &control).unwrap().is_none());
        assert_eq!(control.memory().used(), 0);
    }
    let tiny = StorageReadControl::with_limit(1);
    assert!(Registry::decode(Some(r#"{"labels":{"P":3}}"#), &tiny).is_err());
    assert_eq!(tiny.memory().used(), 0);
    control.cancellation().cancel();
    assert!(matches!(
        Registry::decode(None, &control),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}
