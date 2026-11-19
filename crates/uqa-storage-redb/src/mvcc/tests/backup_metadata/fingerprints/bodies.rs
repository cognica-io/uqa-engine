//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::large_values::{BODIES, OWNERS};

fn digest(revision: CommitSequence, bytes: &[u8]) -> [u8; 32] {
    ImageDigest::of_bytes(&[revision.as_u64().to_be_bytes().as_slice(), bytes].concat()).as_bytes()
}

fn commit(
    storage: &RedbRecordStore,
    rows: &[RecordWrite<'_>],
    control: &StorageReadControl,
) -> CommitSequence {
    let intent = PreparedRecordCommit::new(rows, control).unwrap();
    storage
        .commit(
            storage.allocate_transaction(control).unwrap(),
            &intent,
            control,
        )
        .unwrap()
        .sequence
}

#[test]
fn equal_large_values_in_one_revision_share_bytes_until_the_last_retained_version_is_reclaimed() {
    let storage = memory_store();
    let control = StorageReadControl::with_limit(4 << 20);
    let value = [37; 2048];
    let other = [83; 2048];
    let rows = [(b"a", &value), (b"b", &value), (b"c", &other)].map(|(key, bytes)| RecordWrite {
        key,
        expected: None,
        value: Some(bytes),
    });
    let first = commit(&storage, &rows, &control);
    let hash = digest(first, &value);
    let counts = || {
        let read = storage.database.begin_read().unwrap();
        let bodies = read.open_table(BODIES).unwrap();
        let owners = read.open_table(OWNERS).unwrap();
        assert_eq!(bodies.len().unwrap(), owners.len().unwrap());
        (
            bodies.len().unwrap(),
            owners.get(&hash).unwrap().map(|count| count.value()),
        )
    };
    assert_eq!(counts(), (2, Some(2)));
    let pinned = storage.snapshot(&control).unwrap();
    commit(
        &storage,
        &[RecordWrite {
            key: b"a",
            expected: Some(first),
            value: Some(b"small"),
        }],
        &control,
    );
    storage.reclaim_versions(&control).unwrap();
    assert_eq!(counts(), (2, Some(2)));
    assert_eq!(
        pinned
            .get(b"a", &control)
            .unwrap()
            .unwrap()
            .value()
            .map(|v| &***v),
        Some(value.as_slice())
    );
    drop(pinned);
    storage.reclaim_versions(&control).unwrap();
    assert_eq!(counts(), (2, Some(1)));
    let current = storage.snapshot(&control).unwrap();
    assert_eq!(
        current
            .get(b"b", &control)
            .unwrap()
            .unwrap()
            .value()
            .map(|v| &***v),
        Some(value.as_slice())
    );
    assert_eq!(
        current
            .get(b"c", &control)
            .unwrap()
            .unwrap()
            .value()
            .map(|v| &***v),
        Some(other.as_slice())
    );
    drop(current);
    let remove = [b"b", b"c"].map(|key| RecordWrite {
        key,
        expected: Some(first),
        value: None,
    });
    commit(&storage, &remove, &control);
    storage.reclaim_versions(&control).unwrap();
    assert_eq!(counts(), (0, None));
}

#[test]
fn large_body_collision_and_saturated_owners_store_independent_bytes() {
    for saturated in [false, true] {
        let store = memory_store();
        let control = StorageReadControl::with_limit(4 << 20);
        let actual = [17; 1024];
        let mut other = [19; 1025];
        other[0] = 1;
        if saturated {
            other[1..].copy_from_slice(&actual);
        }
        let owners = if saturated { u64::MAX } else { 1 };
        let digest = digest(CommitSequence::from_u64(1), &actual);
        let change = physical_writer(&store.database).unwrap();
        change
            .open_table(BODIES)
            .unwrap()
            .insert(&digest, other.as_slice())
            .unwrap();
        change
            .open_table(OWNERS)
            .unwrap()
            .insert(&digest, owners)
            .unwrap();
        change.commit().unwrap();
        let first = write(&store, None, &actual, &control);
        let cut = capture(&store, &control);
        assert_eq!(
            fingerprint(&*cut, &control).unwrap().digest.as_bytes(),
            digest
        );
        let row = cut.records().get(b"row", &control).unwrap().unwrap();
        assert_eq!(row.value().map(|value| &***value), Some(actual.as_slice()));
        let read = store.database.begin_read().unwrap();
        let raw = read
            .open_table(VERSIONS)
            .unwrap()
            .get((b"row".as_slice(), first.as_u64()))
            .unwrap()
            .unwrap();
        assert_eq!(raw.value()[0], 1);
        assert_eq!(
            read.open_table(BODIES)
                .unwrap()
                .get(&digest)
                .unwrap()
                .unwrap()
                .value(),
            other.as_slice()
        );
        assert_eq!(
            read.open_table(OWNERS)
                .unwrap()
                .get(&digest)
                .unwrap()
                .unwrap()
                .value(),
            owners
        );
    }
}

#[test]
fn cancelled_duplicate_comparison_aborts_new_bodies_and_missing_payload_cannot_be_returned() {
    let store = memory_store();
    let control = StorageReadControl::with_limit(4 << 20);
    let payload = vec![11; 192 << 10];
    let first = write(&store, None, &payload, &control);
    let hash = digest(first, &payload);
    let next_hash = [53; 32];
    let mut encoded = vec![1];
    encoded.extend_from_slice(&payload);
    let failed = physical_writer(&store.database).unwrap();
    {
        let mut writer = crate::mvcc::large_values::Writer::open(&failed).unwrap();
        writer
            .store(ImageDigest::from_bytes(next_hash), &encoded, None)
            .unwrap();
        let cancelled = StorageReadControl::with_limit(2 << 20);
        cancelled.cancellation().cancel();
        assert!(matches!(
            writer.store(ImageDigest::from_bytes(hash), &encoded, Some(&cancelled)),
            Err(VersionError::Cancelled(_))
        ));
    }
    failed.abort().unwrap();
    {
        let read = store.database.begin_read().unwrap();
        assert_eq!(read.open_table(BODIES).unwrap().len().unwrap(), 1);
        let owners = read.open_table(OWNERS).unwrap();
        assert_eq!(owners.get(&hash).unwrap().unwrap().value(), 1);
        assert!(owners.get(&next_hash).unwrap().is_none());
    }
    let damage = physical_writer(&store.database).unwrap();
    damage.open_table(BODIES).unwrap().remove(&hash).unwrap();
    damage.commit().unwrap();
    let snapshot = store.snapshot(&control).unwrap();
    assert!(matches!(
        snapshot.get(b"row", &control),
        Err(VersionError::InvalidEncoding("missing owned large record"))
    ));
}
