//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::inverted_index::native::mutation::{Address, Projection};

fn address(cluster: u64) -> Address<'static> {
    Address {
        table: "records",
        projection: Some(Projection::Score),
        field: Some("body"),
        term: Some(b"term"),
        document: None,
        cluster: Some(cluster),
        ordinal: None,
    }
}

#[test]
fn spilled_projection_rows_preserve_replacements_and_prefix_deletion() {
    let control = StorageReadControl::with_limit(256 << 10);
    let mut rows = Drafts::new(&control);
    let value = vec![7; 4096];
    for cluster in 0..192_u64 {
        let draft = Draft {
            address: address(cluster).encode(&control).unwrap(),
            values: [
                super::super::owned_value(Some(&value), &control).unwrap(),
                super::super::owned_value(Some(b"positions"), &control).unwrap(),
            ],
            _key: control.memory().empty_reservation(),
            _entry: control.memory().empty_reservation(),
            merge: cluster % 2 == 0,
        };
        rows.put(&cluster.to_be_bytes(), draft).unwrap();
    }
    let mut count = 0;
    rows.visit(|key, draft| {
        let cluster = u64::from_be_bytes(key.try_into().unwrap());
        assert_eq!(draft.merge, cluster % 2 == 0);
        assert_eq!(draft.values[0].as_deref(), Some(&value[..]));
        assert_eq!(draft.values[1].as_deref(), Some(&b"positions"[..]));
        count += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(count, 192);

    let prefix = Address {
        cluster: None,
        ..address(0)
    }
    .encode(&control)
    .unwrap();
    rows.remove_prefix(&prefix).unwrap();
    drop(prefix);
    for cluster in [0_u64, 95, 191] {
        let draft = rows.get(&cluster.to_be_bytes()).unwrap().unwrap();
        assert!(draft.values[0].is_none());
        assert_eq!(draft.values[1].as_deref(), Some(&b"positions"[..]));
    }
    let key = 95_u64.to_be_bytes();
    let mut draft = rows.get(&key).unwrap().unwrap();
    draft.values[0] = super::super::owned_value(Some(b""), &control).unwrap();
    draft.merge = false;
    rows.put(&key, draft).unwrap();
    let draft = rows.get(&key).unwrap().unwrap();
    assert_eq!(draft.values[0].as_deref(), Some(&b""[..]));
    assert!(!draft.merge);
    drop(draft);
    rows.clear();
    assert!(rows.get(&key).unwrap().is_none());
    drop(rows);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn staged_projection_reads_preserve_cancellation() {
    let control = StorageReadControl::with_limit(256 << 10);
    let mut rows = Drafts::new(&control);
    control.cancellation().cancel();
    assert!(matches!(
        rows.get(b"key"),
        Err(uqa_storage::StorageBackendError::Cancelled(_))
    ));
}

#[test]
fn projection_staging_shares_the_allowance_with_other_retained_owners() {
    let control = StorageReadControl::with_limit(256 << 10);
    let peer = control.memory().reserve(208 << 10).unwrap();
    let mut rows = Drafts::new(&control);
    let value = vec![7; 4096];
    for cluster in 0..64_u64 {
        let draft = Draft {
            address: address(cluster).encode(&control).unwrap(),
            values: [
                super::super::owned_value(Some(&value), &control).unwrap(),
                super::super::owned_value(Some(b"positions"), &control).unwrap(),
            ],
            _key: control.memory().empty_reservation(),
            _entry: control.memory().empty_reservation(),
            merge: true,
        };
        rows.put(&cluster.to_be_bytes(), draft).unwrap();
    }
    let prefix = Address {
        cluster: None,
        ..address(0)
    }
    .encode(&control)
    .unwrap();
    rows.remove_prefix(&prefix).unwrap();
    let mut count = 0;
    rows.visit(|_, draft| {
        assert!(draft.values[0].is_none());
        assert_eq!(draft.values[1].as_deref(), Some(&b"positions"[..]));
        count += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(count, 64);
    drop((prefix, rows, peer));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn large_projection_row_releases_its_source_before_retaining_the_encoding() {
    let control = StorageReadControl::with_limit(256 << 10);
    let peer = control.memory().reserve(128 << 10).unwrap();
    let mut rows = Drafts::new(&control);
    let value = vec![7; 48 << 10];
    rows.put(
        b"row",
        Draft {
            address: address(1).encode(&control).unwrap(),
            values: [
                super::super::owned_value(Some(&value), &control).unwrap(),
                None,
            ],
            _key: control.memory().empty_reservation(),
            _entry: control.memory().empty_reservation(),
            merge: true,
        },
    )
    .unwrap();
    let mut count = 0;
    rows.visit(|key, draft| {
        assert_eq!(key, b"row");
        assert_eq!(draft.values[0].as_deref(), Some(&value[..]));
        assert!(draft.values[1].is_none());
        count += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(count, 1);
    drop((rows, peer));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn adjacent_projections_transfer_the_pending_row_without_serializing_it_twice() {
    let control = StorageReadControl::with_limit(256 << 10);
    let mut rows = Drafts::new(&control);
    let score = super::super::owned_value(Some(b"score"), &control).unwrap();
    let pointer = score.as_ref().unwrap().as_ptr();
    let draft = Draft {
        address: address(1).encode(&control).unwrap(),
        values: [score, None],
        _key: control.memory().empty_reservation(),
        _entry: control.memory().empty_reservation(),
        merge: true,
    };
    rows.put(b"row", draft).unwrap();
    assert!(!rows.records.has_written());
    let mut draft = rows.take_for_update(b"row").unwrap().unwrap();
    assert_eq!(draft.values[0].as_ref().unwrap().as_ptr(), pointer);
    draft.values[1] = super::super::owned_value(Some(b"positions"), &control).unwrap();
    rows.put(b"row", draft).unwrap();
    assert!(!rows.records.has_written());
    let mut count = 0;
    rows.visit(|key, draft| {
        assert_eq!(key, b"row");
        assert_eq!(draft.values[0].as_deref(), Some(&b"score"[..]));
        assert_eq!(draft.values[1].as_deref(), Some(&b"positions"[..]));
        count += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(count, 1);
    assert!(rows.records.has_written());
}
