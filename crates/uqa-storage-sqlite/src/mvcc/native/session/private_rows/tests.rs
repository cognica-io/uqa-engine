//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::native::{NativeRecord, NativeRecordFamily, NativeRecordOwner};
use std::sync::Arc;
use uqa_core::memory::MemoryBudget;
use uqa_storage::mvcc::{MemoryVersionStore, PrivateRecordChanges, RecordWrite};

#[test]
fn spilled_private_rows_keep_metadata_and_retained_values_across_advancement() {
    let control = StorageReadControl::with_limit(512 << 10);
    let changes = PrivateRecordChanges::new(control.memory());
    let owner = NativeRecordOwner::Object {
        identity: [3; 16],
        generation: [7; 16],
    };
    let identity = NativeRecordIdentity::new(NativeRecordFamily::Documents, owner).unwrap();
    let body = format!("{{\"payload\":\"{}\"}}", "x".repeat(64 << 10));
    for id in 0..96 {
        let row = NativeRecord::encode(
            NativeRecordFamily::Documents,
            owner,
            &[
                ValueRef::Text(b"docs"),
                ValueRef::Integer(id),
                ValueRef::Text(body.as_bytes()),
                ValueRef::Null,
            ],
            &control,
        )
        .unwrap();
        changes
            .apply(
                &[RecordWrite {
                    key: row.key(),
                    expected: None,
                    value: (id != 31).then_some(row.row()),
                }],
                &control,
            )
            .unwrap();
    }
    let committed = MemoryVersionStore::new(&MemoryBudget::new(1 << 20));
    let view = MergedRecordSnapshot::new(
        Arc::new(committed.snapshot().unwrap()),
        changes.snapshot().unwrap(),
    );
    let replacement = identity
        .encode_key(&[ValueRef::Integer(16)], &control)
        .unwrap();
    changes
        .apply(
            &[RecordWrite {
                key: &replacement,
                expected: None,
                value: None,
            }],
            &control,
        )
        .unwrap();

    // These 96 bodies exceed the write allowance and have spilled; the read allowance cannot load one body.
    let read = StorageReadControl::with_limit(32 << 10);
    let mut rows = PrivateRows::after(&view, identity, &[], Some(7), &read).unwrap();
    for id in 8..96 {
        assert_eq!(rows.peek(), Some(id));
        assert_eq!(rows.live().unwrap(), id != 31);
        rows.advance().unwrap();
    }
    assert_eq!(rows.peek(), None);
    drop(rows);
    assert_eq!(read.memory().used(), 0);

    // Loading a selected payload uses its retained entry even after the current transaction changed it.
    let read = StorageReadControl::with_limit(256 << 10);
    let mut rows = PrivateRows::after(&view, identity, &[], Some(15), &read).unwrap();
    for id in 16..=17 {
        assert_eq!(rows.peek(), Some(id));
        assert_eq!(
            rows.visit(&mut |row| {
                assert_eq!(row[1].as_i64().unwrap(), id);
                assert_eq!(row[2].as_str().unwrap(), body);
                Ok(true)
            })
            .unwrap(),
            Some(true)
        );
        rows.advance().unwrap();
    }
    drop(rows);
    assert_eq!(read.memory().used(), 0);
}
