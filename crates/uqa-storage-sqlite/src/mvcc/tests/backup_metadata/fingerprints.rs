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
        .visit_record_image_positions(None, 1, control, &mut |key, position| {
            assert_eq!(key, b"row");
            assert!(position.visible.unwrap().live);
            found = position.fingerprint;
            Ok(true)
        })
        .unwrap();
    found
}

#[test]
fn opening_format_69_preserves_existing_fingerprints_and_writes_the_next_revision() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    let first = write(&store, b"old", None, &control);
    let original = fingerprint(capture(&store, &control).as_ref(), &control).unwrap();
    downgrade_record_format(&store, 69);
    drop(store);
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let before_update = capture(&store, &control);
    write(&store, b"new", Some(first), &control);
    assert_eq!(
        fingerprint(before_update.as_ref(), &control),
        Some(original)
    );
    let current = fingerprint(capture(&store, &control).as_ref(), &control).unwrap();
    assert_ne!(current.digest, original.digest);
    assert_eq!(current.decoded_bytes, original.decoded_bytes);
}

#[test]
fn fingerprint_metadata_tracks_visible_revisions_and_upgrades_without_reading_old_payloads() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let control = control();
    let first = write(&store, b"old", None, &control);
    downgrade_record_format(&store, 68);
    drop(store);
    let store = SQLiteRecordStore::new(&connection).unwrap();
    let old = capture(&store, &control);
    let next = write(&store, b"new", Some(first), &control);
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
    store
        .with(|connection| {
            let _permit = schema::WritePermit::acquire(connection)?;
            connection.execute(
                "UPDATE _uqa_mvcc_version_metadata SET image_digest = x'01' WHERE key = ?1 AND sequence = ?2",
                params![b"row".as_slice(), next.as_u64().to_be_bytes().as_slice()],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(fingerprint(current.as_ref(), &control).is_none());
}
