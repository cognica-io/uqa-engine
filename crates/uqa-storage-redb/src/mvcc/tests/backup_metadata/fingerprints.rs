//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::backup::{ImageDigest, UnitFingerprint};

mod bodies;

fn fingerprint(
    snapshot: &dyn CommittedBackupSnapshot,
    control: &StorageReadControl,
) -> Option<UnitFingerprint> {
    let mut found = None;
    snapshot
        .visit_record_image_positions(None, 4, control, &mut |key, position| {
            assert_eq!(key, b"row");
            assert!(position.visible.unwrap().live);
            found = position.fingerprint;
            Ok(true)
        })
        .unwrap();
    found
}

#[test]
fn cold_fingerprint_reads_stay_bounded_and_select_the_pinned_revision() {
    let backend = FaultBackend::default();
    let open = || {
        RedbRecordStore::new(Arc::new(
            Database::builder()
                .set_cache_size(0)
                .create_with_backend(backend.clone())
                .unwrap(),
        ))
        .unwrap()
    };
    let setup = StorageReadControl::with_limit(32 << 20);
    let value = vec![42; 2 << 20];
    let store = open();
    let first = write(&store, None, &value, &setup);
    drop(store);
    let store = open();
    let reader = StorageReadControl::with_limit(8 << 10);
    let cut = capture(&store, &reader);
    write(&store, Some(first), b"later", &setup);
    backend.read_bytes.store(0, Ordering::Relaxed);
    let found = fingerprint(cut.as_ref(), &reader).unwrap();
    let mut visited = 0;
    cut.records()
        .visit_keys(b"ro", Some(b"r"), 1, &reader, &mut |key, record| {
            assert_eq!(key, b"row");
            assert_eq!(record.revision, Some(first));
            assert!(record.live);
            visited += 1;
            Ok(true)
        })
        .unwrap();
    assert_eq!(visited, 1);
    let physical = backend.read_bytes.load(Ordering::Relaxed);
    assert!(
        physical < 256 << 10,
        "metadata scan fetched {physical} bytes"
    );
    let mut image = first.as_u64().to_be_bytes().to_vec();
    image.extend_from_slice(&value);
    assert_eq!(
        found,
        UnitFingerprint {
            digest: ImageDigest::of_bytes(&image),
            decoded_bytes: image.len() as u64
        }
    );
}

#[test]
fn upgrading_revision_images_leaves_old_data_to_content_comparison() {
    let store = memory_store();
    let control = StorageReadControl::with_limit(1 << 20);
    let first = write(&store, None, b"old", &control);
    let db = store.database.clone();
    drop(store);
    let transaction = physical_writer(&db).unwrap();
    transaction
        .open_table(METADATA)
        .unwrap()
        .insert("format", 64_u64.to_be_bytes().as_slice())
        .unwrap();
    remove_backup_metadata_for_predecessor(&transaction);
    transaction.commit().unwrap();
    let store = RedbRecordStore::new(db).unwrap();
    let old = capture(&store, &control);
    let external = vec![5; crate::mvcc::heads::INLINE_LIMIT + 1];
    let next = write(&store, Some(first), &external, &control);
    assert!(fingerprint(old.as_ref(), &control).is_none());
    let current = capture(&store, &control);
    assert_eq!(
        fingerprint(current.as_ref(), &control),
        Some(UnitFingerprint {
            digest: ImageDigest::of_bytes(
                &[next.as_u64().to_be_bytes().as_slice(), external.as_slice()].concat()
            ),
            decoded_bytes: external.len() as u64 + 8
        })
    );
    let transaction = physical_writer(&store.database).unwrap();
    {
        let mut images = transaction
            .open_table(crate::mvcc::image_fingerprints::TABLE)
            .unwrap();
        let identity = (b"row".as_slice(), next.as_u64());
        let mut incompatible = *images.get(identity).unwrap().unwrap().value();
        incompatible[..8].copy_from_slice(&1_u64.to_be_bytes());
        images.insert(identity, &incompatible).unwrap();
    }
    transaction.commit().unwrap();
    assert!(fingerprint(current.as_ref(), &control).is_none());
}

#[test]
fn current_certificates_move_to_history_on_replacement_and_follow_snapshot_reclamation() {
    let store = memory_store();
    let control = StorageReadControl::with_limit(1 << 20);
    let inside = vec![17; crate::mvcc::heads::INLINE_LIMIT];
    let outside = vec![23; inside.len() + 1];
    let first = write(&store, None, &inside, &control);
    let indexed = |sequence: CommitSequence| {
        store
            .database
            .begin_read()
            .unwrap()
            .open_table(crate::mvcc::image_fingerprints::TABLE)
            .unwrap()
            .get((b"row".as_slice(), sequence.as_u64()))
            .unwrap()
            .is_some()
    };
    assert!(!indexed(first));
    let original = capture(&store, &control);
    let before = fingerprint(original.as_ref(), &control).unwrap();
    let second = write(&store, Some(first), &outside, &control);
    assert!(indexed(first));
    assert!(indexed(second));
    let newer = capture(&store, &control);
    let after = fingerprint(newer.as_ref(), &control).unwrap();
    assert_eq!(
        before.digest,
        ImageDigest::of_bytes(
            &[first.as_u64().to_be_bytes().as_slice(), inside.as_slice()].concat()
        )
    );
    assert_eq!(
        after.digest,
        ImageDigest::of_bytes(
            &[second.as_u64().to_be_bytes().as_slice(), outside.as_slice()].concat()
        )
    );
    store.reclaim_versions(&control).unwrap();
    assert_eq!(fingerprint(original.as_ref(), &control), Some(before));
    drop(original);
    store.reclaim_versions(&control).unwrap();
    assert!(!indexed(first));
    assert!(indexed(second));
    assert_eq!(fingerprint(newer.as_ref(), &control), Some(after));
}

#[test]
fn compact_live_heads_keep_pinned_bytes_when_the_exact_large_certificate_is_unusable() {
    use crate::mvcc::image_fingerprints::TABLE;
    let large = vec![41; crate::mvcc::heads::INLINE_LIMIT + 1];
    for damaged in [None, Some([0; 40]), Some([1; 40])] {
        let store = memory_store();
        let control = StorageReadControl::with_limit(1 << 20);
        let older = write(&store, None, &large, &control);
        let earlier = capture(&store, &control);
        let unchanged = fingerprint(&*earlier, &control).unwrap();
        let next = write(&store, Some(older), &large, &control);
        let cut = capture(&store, &control);
        let before = fingerprint(&*cut, &control).unwrap();
        assert_ne!(before, unchanged);
        let change = physical_writer(&store.database).unwrap();
        {
            let mut images = change.open_table(TABLE).unwrap();
            let identity = (b"row".as_slice(), next.as_u64());
            assert!(images.remove(identity).unwrap().is_some());
            if let Some(mut damage) = damaged {
                if damage[8] != 0 {
                    damage[..8].copy_from_slice(&8_u64.to_be_bytes());
                }
                images.insert(identity, &damage).unwrap();
            }
            let heads = change.open_table(HEADS).unwrap();
            assert_eq!(
                heads.get(b"row".as_slice()).unwrap().unwrap().value().len(),
                2
            );
        }
        change.commit().unwrap();
        assert_eq!(fingerprint(&*cut, &control), None);
        assert!(
            cut.records()
                .metadata(b"row", &control)
                .unwrap()
                .unwrap()
                .live
        );
        write(&store, Some(next), b"third", &control);
        store.reclaim_versions(&control).unwrap();
        assert_eq!(fingerprint(&*earlier, &control), Some(unchanged));
        assert_eq!(fingerprint(&*cut, &control), None);
        let row = cut.records().get(b"row", &control).unwrap().unwrap();
        assert_eq!(row.value().map(|value| &***value), Some(large.as_slice()));
    }
}

#[test]
fn upgrading_separate_certificates_preserves_current_and_pinned_record_images() {
    let store = memory_store();
    let control = StorageReadControl::with_limit(1 << 20);
    let first = write(&store, None, b"first", &control);
    let pinned = capture(&store, &control);
    let before = fingerprint(pinned.as_ref(), &control).unwrap();
    let second = write(&store, Some(first), b"second", &control);
    let current = capture(&store, &control);
    let after = fingerprint(current.as_ref(), &control).unwrap();
    drop(current);
    let database = Arc::clone(&store.database);
    drop(store);
    let change = physical_writer(&database).unwrap();
    change
        .open_table(METADATA)
        .unwrap()
        .insert("format", 65_u64.to_be_bytes().as_slice())
        .unwrap();
    remove_backup_metadata_for_predecessor(&change);
    let allocated = {
        let mut metadata = change.open_table(METADATA).unwrap();
        let allocated = read_u64(&metadata, "allocated").unwrap();
        metadata.insert("allocated", b"bad".as_slice()).unwrap();
        allocated
    };
    change.commit().unwrap();
    assert!(pinned.records().get(b"row", &control).is_err());
    assert!(RedbRecordStore::new(Arc::clone(&database)).is_err());
    {
        let unchanged = database.begin_read().unwrap();
        assert!(unchanged.open_table(HEADS).is_err());
        assert_eq!(
            unchanged
                .open_table(crate::mvcc::heads::LEGACY_HEADS)
                .unwrap()
                .get(b"row".as_slice())
                .unwrap()
                .unwrap()
                .value(),
            second.as_u64()
        );
        assert_eq!(
            unchanged
                .open_table(crate::mvcc::image_fingerprints::TABLE)
                .unwrap()
                .len()
                .unwrap(),
            2
        );
    }
    let repair = physical_writer(&database).unwrap();
    repair
        .open_table(METADATA)
        .unwrap()
        .insert("allocated", allocated.to_be_bytes().as_slice())
        .unwrap();
    repair.commit().unwrap();
    let reopened = RedbRecordStore::new(database).unwrap();
    assert_eq!(fingerprint(pinned.as_ref(), &control), Some(before));
    assert_eq!(
        fingerprint(capture(&reopened, &control).as_ref(), &control),
        Some(after)
    );
    assert_eq!(
        &***pinned
            .records()
            .get(b"row", &control)
            .unwrap()
            .unwrap()
            .value()
            .unwrap(),
        b"first"
    );
}

#[test]
fn uncertified_predecessor_shadows_an_older_valid_certificate_at_its_own_cut() {
    let store = memory_store();
    let control = StorageReadControl::with_limit(1 << 20);
    let older = write(&store, None, b"older", &control);
    let earlier = capture(&store, &control);
    let first_image = fingerprint(earlier.as_ref(), &control).unwrap();
    let missing = write(&store, Some(older), b"missing certificate", &control);
    let cut = capture(&store, &control);
    let database = Arc::clone(&store.database);
    drop(store);
    let change = physical_writer(&database).unwrap();
    change
        .open_table(METADATA)
        .unwrap()
        .insert("format", 65_u64.to_be_bytes().as_slice())
        .unwrap();
    remove_backup_metadata_for_predecessor(&change);
    assert!(change
        .open_table(crate::mvcc::image_fingerprints::TABLE)
        .unwrap()
        .remove((b"row".as_slice(), missing.as_u64()))
        .unwrap()
        .is_some());
    change.commit().unwrap();
    let reopened = RedbRecordStore::new(database).unwrap();
    assert!(fingerprint(cut.as_ref(), &control).is_none());
    write(&reopened, Some(missing), b"new", &control);
    reopened.reclaim_versions(&control).unwrap();
    assert!(fingerprint(cut.as_ref(), &control).is_none());
    assert_eq!(fingerprint(earlier.as_ref(), &control), Some(first_image));
    assert_eq!(
        &***cut
            .records()
            .get(b"row", &control)
            .unwrap()
            .unwrap()
            .value()
            .unwrap(),
        b"missing certificate"
    );
}

#[test]
fn live_empty_image_hashes_only_an_observed_revision_for_capture_and_displacement() {
    use crate::mvcc::heads::EMPTY_IMAGE_HASHES;
    let store = memory_store();
    let control = StorageReadControl::with_limit(1 << 20);
    EMPTY_IMAGE_HASHES.with(|count| count.set(0));
    let first = write(&store, None, b"", &control);
    let cut = capture(&store, &control);
    let row = cut.records().get(b"row", &control).unwrap().unwrap();
    assert_eq!(row.value().map(|value| &***value), Some(b"".as_slice()));
    cut.records()
        .visit_keys(b"", None, 1, &control, &mut |_, record| {
            assert!(record.live);
            Ok(true)
        })
        .unwrap();
    assert_eq!(EMPTY_IMAGE_HASHES.with(std::cell::Cell::get), 0);
    let expected = UnitFingerprint {
        decoded_bytes: 8,
        digest: ImageDigest::of_bytes(&first.as_u64().to_be_bytes()),
    };
    cut.visit_record_image_positions(None, 1, &control, &mut |_, position| {
        assert_eq!(position.fingerprint_bytes, 0);
        assert_eq!(position.fingerprint, Some(expected));
        Ok(true)
    })
    .unwrap();
    assert_eq!(EMPTY_IMAGE_HASHES.with(std::cell::Cell::get), 1);
    write(&store, Some(first), b"next", &control);
    assert_eq!(EMPTY_IMAGE_HASHES.with(std::cell::Cell::get), 2);
    assert_eq!(fingerprint(cut.as_ref(), &control), Some(expected));
    assert_eq!(EMPTY_IMAGE_HASHES.with(std::cell::Cell::get), 2);
}

struct MetadataRequests<'a, F> {
    keys: &'a [&'a [u8]],
    receive: F,
}

impl<F> uqa_storage::mvcc::RecordMetadataRequests for MetadataRequests<'_, F>
where
    F: FnMut(Option<uqa_storage::mvcc::RecordMetadata>) -> VersionResult<()>,
{
    fn advance(&mut self) -> VersionResult<bool> {
        Ok(!self.keys.is_empty())
    }
    fn key(&self) -> &[u8] {
        self.keys[0]
    }
    fn accept(&mut self, metadata: Option<uqa_storage::mvcc::RecordMetadata>) -> VersionResult<()> {
        (self.receive)(metadata)?;
        self.keys = &self.keys[1..];
        Ok(())
    }
}

#[test]
fn point_and_batch_metadata_select_historical_and_current_revisions_without_reading_large_values() {
    use uqa_storage::mvcc::RecordMetadata;
    let backend = FaultBackend::default();
    let setup = StorageReadControl::with_limit(32 << 20);
    let limited = StorageReadControl::with_limit(1024);
    let store = RedbRecordStore::new(Arc::new(
        Database::builder()
            .set_cache_size(0)
            .create_with_backend(backend.clone())
            .unwrap(),
    ))
    .unwrap();
    let value = vec![71; 2 << 20];
    let first = write(&store, None, &value, &setup);
    let pinned = store.snapshot(&limited).unwrap();
    let second = write(&store, Some(first), &value, &setup);
    let current = store.snapshot(&limited).unwrap();
    let mut totals = Vec::new();
    for (snapshot, revision) in [(pinned, first), (current, second)] {
        let expected = Some(RecordMetadata {
            revision: Some(revision),
            live: true,
        });
        backend.read_bytes.store(0, Ordering::Relaxed);
        assert_eq!(snapshot.metadata(b"row", &limited).unwrap(), expected);
        let single = backend.read_bytes.swap(0, Ordering::Relaxed);
        crate::mvcc::read::READ_WINDOWS.with(|windows| windows.set(0));
        let mut received = Vec::new();
        let keys: &[&[u8]] = &[b"missing", b"row", b"row"];
        snapshot
            .visit_metadata(
                &mut MetadataRequests {
                    keys,
                    receive: |item| {
                        received.push(item);
                        Ok(())
                    },
                },
                &limited,
            )
            .unwrap();
        assert_eq!(received, [None, expected, expected]);
        totals.push((single, backend.read_bytes.load(Ordering::Relaxed)));
        assert_eq!(
            crate::mvcc::read::READ_WINDOWS.with(std::cell::Cell::get),
            1
        );
    }
    assert!(
        totals
            .iter()
            .all(|(single, batch)| *single < 256 << 10 && *batch < 256 << 10),
        "physical bytes (point, batch) for pinned and current: {totals:?}"
    );
}

#[test]
fn metadata_requests_admit_only_a_real_key_and_stop_before_later_input() {
    let store = memory_store();
    let setup = StorageReadControl::with_limit(1 << 20);
    let snapshot = store.snapshot(&setup).unwrap();
    crate::mvcc::read::READ_WINDOWS.with(|windows| windows.set(0));
    snapshot
        .visit_metadata(
            &mut MetadataRequests {
                keys: &[],
                receive: |_| panic!("no selected key"),
            },
            &setup,
        )
        .unwrap();
    assert_eq!(
        crate::mvcc::read::READ_WINDOWS.with(std::cell::Cell::get),
        0
    );
    for cancel in [false, true] {
        let control = StorageReadControl::with_limit(1024);
        let mut calls = 0;
        let result = snapshot.visit_metadata(
            &mut MetadataRequests {
                keys: &[b"first", b"later"],
                receive: |_| {
                    calls += 1;
                    if cancel {
                        control.cancellation().cancel();
                        Ok(())
                    } else {
                        Err(VersionError::InvalidEncoding("request rejected"))
                    }
                },
            },
            &control,
        );
        if cancel {
            assert!(matches!(
                result.unwrap_err().into_storage_error(),
                uqa_storage::StorageBackendError::Cancelled(_)
            ));
        } else {
            assert!(matches!(
                result,
                Err(VersionError::InvalidEncoding("request rejected"))
            ));
        }
        assert_eq!(calls, 1);
    }
}
