//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::backup::{ImageDigest, UnitFingerprint};

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
    let next = write(&store, Some(first), b"new", &control);
    assert!(fingerprint(old.as_ref(), &control).is_none());
    let current = capture(&store, &control);
    assert_eq!(
        fingerprint(current.as_ref(), &control),
        Some(UnitFingerprint {
            digest: ImageDigest::of_bytes(
                &[next.as_u64().to_be_bytes().as_slice(), b"new"].concat()
            ),
            decoded_bytes: 11
        })
    );
    let transaction = physical_writer(&store.database).unwrap();
    let mut incompatible = [0; 40];
    incompatible[7] = 1;
    transaction
        .open_table(crate::mvcc::image_fingerprints::TABLE)
        .unwrap()
        .insert((b"row".as_slice(), next.as_u64()), &incompatible)
        .unwrap();
    transaction.commit().unwrap();
    assert!(fingerprint(current.as_ref(), &control).is_none());
}
