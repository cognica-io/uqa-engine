//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    diskann_index::format::DiskANNGeneration,
    mvcc::{MemoryVersionStore, RecordWrite},
};

fn state(status: u8) -> [u8; STATE_BYTES] {
    // Independent bytes: leased owner 7, status discriminator at byte 1.
    let mut bytes = [0; STATE_BYTES];
    bytes[0] = 2;
    bytes[1] = status;
    bytes[17] = 7;
    bytes
}

#[test]
fn key_value_backup_image_selects_only_published_generation_at_its_own_cut() {
    let control = StorageReadControl::with_limit(1 << 20);
    let store = MemoryVersionStore::new(control.memory());
    for status in 0..=5 {
        let generation = DiskANNGeneration::new([9; 16], 10, 11, u64::from(status) + 1).unwrap();
        let keys = Keys::new(generation);
        let key = keys.key(Kind::State);
        let revision = store
            .commit(
                &[RecordWrite {
                    key: key.as_ref(),
                    expected: None,
                    value: Some(&state(status)),
                }],
                &control,
            )
            .unwrap();
        let cut = store.snapshot().unwrap();
        // A later retirement/publication must never change earlier image membership.
        store
            .commit(
                &[RecordWrite {
                    key: key.as_ref(),
                    expected: Some(revision),
                    value: Some(&state(if status == 4 { 5 } else { 4 })),
                }],
                &control,
            )
            .unwrap();
        for key in [
            key,
            keys.key(Kind::Graph(0)),
            keys.key(Kind::Graph(u64::MAX)),
        ] {
            assert_eq!(
                include(&cut, key.as_ref(), &control).unwrap(),
                (status == 4, STATE_BYTES)
            );
        }
    }
    assert_eq!(
        include(&store.snapshot().unwrap(), b"ordinary", &control).unwrap(),
        (true, 0)
    );
}

#[test]
fn key_value_backup_image_rejects_corrupt_or_missing_generation_state() {
    let control = StorageReadControl::with_limit(1 << 20);
    let store = MemoryVersionStore::new(control.memory());
    let keys = Keys::new(DiskANNGeneration::new([9; 16], 10, 11, 12).unwrap());
    let state_key = keys.key(Kind::State);
    assert!(include(&store.snapshot().unwrap(), state_key.as_ref(), &control).is_err());
    let cases: &[Option<&[u8]>] = &[
        None,
        Some(b""),
        Some(&[0; 18]),
        Some(&[0; 19]),
        Some(&state(6)),
    ];
    let mut revision = None;
    for bytes in cases {
        revision = Some(
            store
                .commit(
                    &[RecordWrite {
                        key: state_key.as_ref(),
                        expected: revision,
                        value: *bytes,
                    }],
                    &control,
                )
                .unwrap(),
        );
        assert!(include(
            &store.snapshot().unwrap(),
            keys.key(Kind::Graph(7)).as_ref(),
            &control
        )
        .is_err());
    }
    for key in [
        &state_key.as_ref()[..PREFIX_BYTES],
        &[ROOT, &[1]].concat(),
        &[state_key.as_ref(), &[1]].concat(),
    ] {
        assert!(include(&store.snapshot().unwrap(), key, &control).is_err());
    }
    control.cancellation().cancel();
    assert!(matches!(
        include(&store.snapshot().unwrap(), state_key.as_ref(), &control),
        Err(crate::StorageBackendError::Cancelled(_))
    ));
}
