//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Actual logical mapping and artifact images, with a separate full-read oracle outside the job allowance.

use std::{collections::BTreeMap, sync::Arc};

use uqa_core::memory::BudgetedVec;
use uqa_storage::{
    backup::{BackupError, ImageProvider},
    diskann_index::{format::DiskANNGeneration, pages::DiskANNRecordKey},
    key_value::{
        conformance::{verify_diskann_restore_source, verify_diskann_restored_writes},
        KeyValueBackupImage, KeyValueDiskANNStore, KeyValueImageStep, KeyValueNotificationRecords,
    },
    mvcc::{IdentifierRequest, NotificationRecordLayout, VersionedPersistence},
    read_control::StorageReadControl,
    KeyValueStore, PersistentStorageProvider,
};

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
#[test]
fn redb_backup_repository_round_trip_retains_the_cut_after_source_loss() {
    use uqa_storage::backup::{
        BackupKey, BackupLimits, BackupReadStep, LocalRepository, Protection, ProtectionId,
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.redb");
    let owner = crate::RedbStorage::open(&path).unwrap();
    let setup = StorageReadControl::with_limit(32 << 20);
    let session = owner.open_session().unwrap();
    verify_diskann_restore_source(&*session.backend).unwrap();
    drop(session);
    let store = owner.store();
    let large = vec![73; (1 << 20) + 17];
    store.put(b"large", &large).unwrap();
    for number in 0..128_u64 {
        store
            .put(&number.to_be_bytes(), b"literal retained bytes")
            .unwrap();
    }
    let notification = KeyValueNotificationRecords.key(&setup).unwrap();
    store.put(&notification, b"transient delivery").unwrap();
    let mut expected = expected_records(&owner, b"\xffunused", &notification);
    let control = StorageReadControl::with_limit(128 * 1024);
    let image = owner.capture_backup_image(&control).unwrap();
    let cut = image.cut();
    store.put(b"large", b"later replacement").unwrap();
    let protection = || Protection::Encrypted {
        domain: ProtectionId::from_bytes([3; 16]).unwrap(),
        key: BackupKey::new([61; 32]),
    };
    let limits = BackupLimits::new(4096, 4096).unwrap();
    let repository_path = directory.path().join("backup");
    let repository =
        LocalRepository::create(&repository_path, protection(), limits, &control).unwrap();
    let mut operation = repository
        .begin_backup(cut, None, 16 << 20, &control)
        .unwrap();
    let report = operation.prepare_image(image, 2 << 20, &control).unwrap();
    assert_eq!(report.source.excluded_units, 1);
    let root = operation.publish(&control).unwrap();
    drop(operation);
    drop(repository);
    drop(store);
    drop(owner);
    std::fs::remove_file(path).unwrap();
    let repository =
        LocalRepository::open(&repository_path, protection(), limits, &control).unwrap();
    let lease = repository.open_backup(root.backup(), &control).unwrap();
    let mut reader = lease.read_image(2 << 20, &control).unwrap();
    loop {
        match reader.advance(4096).unwrap() {
            BackupReadStep::Complete => break,
            BackupReadStep::Pending => continue,
            BackupReadStep::Unit => (),
        }
        let unit = reader.current_unit().unwrap();
        let wanted = (unit.address().family == 3).then(|| {
            expected
                .remove(unit.address().key)
                .expect("exact original record set")
        });
        let mut offset = 0;
        let mut buffer = [0; 4096];
        loop {
            let n = reader.read_current(&mut buffer).unwrap();
            if n == 0 {
                break;
            }
            if let Some(wanted) = &wanted {
                assert_eq!(&buffer[..n], &wanted[offset..offset + n]);
            }
            offset += n;
        }
        if let Some(wanted) = wanted {
            assert_eq!(offset, wanted.len());
        }
    }
    assert!(expected.is_empty());
    assert_eq!(reader.verified().unwrap().root(), root);
    assert_eq!(control.memory().used(), 0);
    assert!(control.memory().peak() <= 128 * 1024);
}

fn generation_prefix(generation: DiskANNGeneration) -> Vec<u8> {
    let mut prefix = b"\0uqa-diskann-v1\0\x01".to_vec();
    prefix.extend_from_slice(&generation.database());
    for number in [
        generation.table(),
        generation.index(),
        generation.generation(),
    ] {
        prefix.extend_from_slice(&number.to_be_bytes());
    }
    prefix
}

pub(super) fn expected_records(
    owner: &crate::RedbStorage,
    excluded: &[u8],
    notification: &[u8],
) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let control = StorageReadControl::with_limit(32 << 20);
    let snapshot = owner.record_store().unwrap().snapshot(&control).unwrap();
    let mut expected = BTreeMap::new();
    let mut after = None;
    loop {
        let mut next = None;
        snapshot
            .visit_prefix(b"", after.as_deref(), 64, &control, &mut |key, record| {
                next = Some(key.to_vec());
                if !key.starts_with(excluded) && key != notification {
                    if let Some(value) = record.value {
                        let mut encoded = record.revision.unwrap().as_u64().to_be_bytes().to_vec();
                        encoded.extend_from_slice(value);
                        expected.insert(key.to_vec(), encoded);
                    }
                }
                Ok(true)
            })
            .unwrap();
        if next.is_none() {
            break;
        }
        after = next;
    }
    expected
}

#[test]
fn redb_backup_image_preserves_published_artifacts_and_excludes_delivery_and_builds() {
    let directory = tempfile::tempdir().unwrap();
    let owner = crate::RedbStorage::open(directory.path().join("image.redb")).unwrap();
    let setup = StorageReadControl::with_limit(32 << 20);
    let session = owner.open_session().unwrap();
    let published = verify_diskann_restore_source(&*session.backend).unwrap();
    let store: Arc<dyn KeyValueStore> = Arc::new(owner.store());
    let diskann = KeyValueDiskANNStore::connect(&store, &setup).unwrap();
    let mut abandoned = diskann
        .allocate_stage(published.table(), published.index(), &setup)
        .unwrap();
    abandoned.start(&setup).unwrap();
    let abandoned_prefix = generation_prefix(abandoned.generation());
    let large: Vec<_> = (0..(1 << 20) + 17).map(|i| (i % 251) as u8).collect();
    abandoned
        .write_record(DiskANNRecordKey::Codes(0), &large, 2 << 20, &setup)
        .unwrap();
    let notification = KeyValueNotificationRecords.key(&setup).unwrap();
    store.put(&notification, &large).unwrap();
    store.put(b"opaque-large", &large).unwrap();
    store.put(b"opaque-empty", b"").unwrap();
    let records = owner.record_store().unwrap();
    records
        .allocate_identifiers(b"user-sequence", IdentifierRequest::Observe(42), &setup)
        .unwrap();
    let expected = expected_records(&owner, &abandoned_prefix, &notification);
    assert!(expected
        .keys()
        .any(|key| key.starts_with(&generation_prefix(published))));
    let control = StorageReadControl::with_limit(4096);
    let mut image = owner.capture_backup_image(&control).unwrap();
    // Retire the captured head and publish a different generation before any image record is read.
    let replacement = verify_diskann_restored_writes(&*session.backend, published).unwrap();
    assert_ne!(replacement, published);
    verify_export(&mut image, &owner, expected, &control);
}

fn verify_export(
    image: &mut KeyValueBackupImage,
    owner: &crate::RedbStorage,
    mut expected: BTreeMap<Vec<u8>, Vec<u8>>,
    control: &StorageReadControl,
) {
    let cut = image.cut();
    let mut output = BudgetedVec::new(control.memory());
    output.extend_from_slice(&[0; 1024]).unwrap();
    let mut previous = None;
    let mut wrote = false;
    let mut found_sequence = false;
    let mut copied = 0;
    let mut count = 0;
    loop {
        let before = image.visited_positions();
        let step = image.advance(3).unwrap();
        assert!(image.visited_positions() - before <= 3);
        match step {
            KeyValueImageStep::Complete => break,
            KeyValueImageStep::Pending => continue,
            KeyValueImageStep::Unit => (),
        }
        count += 1;
        let unit = image.current_unit().unwrap();
        let key = (unit.address.family, unit.address.key.to_vec());
        assert!(previous.as_ref().is_none_or(|last| last < &key));
        previous = Some(key.clone());
        let length = unit.decoded_bytes as usize;
        let wanted = match key.0 {
            1 => {
                let mut bytes = b"UQAKVI01".to_vec();
                bytes.extend_from_slice(&cut.source.as_bytes());
                bytes.extend_from_slice(&cut.sequence.unwrap().as_u64().to_be_bytes());
                Some(bytes)
            }
            2 if key.1 == b"user-sequence" => {
                found_sequence = true;
                Some(42_u64.to_be_bytes().to_vec())
            }
            2 => None,
            3 => Some(
                expected
                    .remove(&key.1)
                    .expect("only expected live records are exported"),
            ),
            _ => panic!("unknown image family"),
        };
        let mut offset = 0;
        while offset < length {
            let n = image.read_current(&mut output).unwrap();
            assert!(n > 0);
            if let Some(wanted) = &wanted {
                assert_eq!(&output[..n], &wanted[offset..offset + n]);
                assert_eq!(wanted.len(), length);
            }
            offset += n;
            if !wrote && key.1 == b"opaque-large" {
                replace_during_export(owner.clone());
                wrote = true;
            }
        }
        assert_eq!(image.read_current(&mut output).unwrap(), 0);
        copied += match key.0 {
            1 => 0,
            2 => length,
            _ => length - 8,
        } as u64;
    }
    assert!(expected.is_empty());
    assert!(wrote && found_sequence && count > 20);
    assert_eq!(image.excluded_units(), 3);
    assert_eq!(image.copied_source_bytes(), copied);
    assert!(image.metadata_read_bytes() >= 3 * 18);
    assert!(control.memory().peak() <= 4096);
    drop(output);
    assert_eq!(control.memory().used(), 0);
    control.cancellation().cancel();
    assert_eq!(image.advance(1).unwrap(), KeyValueImageStep::Complete);
    assert!(matches!(
        image.read_current(&mut []),
        Err(BackupError::Finished)
    ));
}

fn replace_during_export(owner: crate::RedbStorage) {
    let (send, receive) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let setup = StorageReadControl::with_limit(8 << 20);
        owner.store().put(b"opaque-large", b"later").unwrap();
        owner
            .record_store()
            .unwrap()
            .allocate_identifiers(b"user-sequence", IdentifierRequest::Observe(99), &setup)
            .unwrap();
        owner
            .record_store()
            .unwrap()
            .reclaim_versions(&setup)
            .unwrap();
        send.send(()).unwrap();
    });
    receive
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("writer progresses between export chunks");
    writer.join().unwrap();
}

#[test]
fn redb_backup_image_terminal_failures_release_both_snapshot_owners() {
    for failure in 0..6 {
        let directory = tempfile::tempdir().unwrap();
        let owner = crate::RedbStorage::open(directory.path().join("image.redb")).unwrap();
        owner.store().put(b"row", b"value").unwrap();
        if failure == 5 {
            let mut state = generation_prefix(DiskANNGeneration::new([9; 16], 10, 11, 12).unwrap());
            state.push(0);
            owner.store().put(&state, b"invalid state").unwrap();
        }
        let control = StorageReadControl::with_limit(4096);
        let source = owner
            .record_store()
            .unwrap()
            .capture_backup_snapshot(&control)
            .unwrap()
            .unwrap();
        let weak = Arc::downgrade(&source);
        let mut image = KeyValueBackupImage::new(source, ImageProvider::Redb, &control).unwrap();
        let result = match failure {
            0 => image.read_current(&mut []).map(|_| ()),
            1 => image.advance(0).map(|_| ()),
            2 => {
                image.advance(1).unwrap();
                image.advance(1).map(|_| ())
            }
            3 => {
                control.cancellation().cancel();
                image.advance(1).map(|_| ())
            }
            4 => {
                image.advance(1).unwrap();
                image.read_current(&mut [0; 32]).unwrap();
                while image.advance(1).unwrap() != KeyValueImageStep::Unit {}
                // This fixture has no watermarks: the first raw unit is a record.
                image.read_current(&mut [0; 1]).unwrap();
                image.advance(1).map(|_| ())
            }
            _ => {
                image.advance(1).unwrap();
                image.read_current(&mut [0; 32]).unwrap();
                loop {
                    match image.advance(1) {
                        Ok(KeyValueImageStep::Pending) => (),
                        error => break error.map(|_| ()),
                    }
                }
            }
        };
        assert!(result.is_err());
        assert!(weak.upgrade().is_none());
        assert_eq!(control.memory().used(), 0);
        assert!(matches!(image.advance(1), Err(BackupError::Finished)));
        assert!(matches!(
            image.read_current(&mut []),
            Err(BackupError::Finished)
        ));
    }
}

#[test]
fn redb_backup_image_empty_cut_and_split_prefixes_have_stable_terminal_state() {
    for populated in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let owner = crate::RedbStorage::open(directory.path().join("image.redb")).unwrap();
        if populated {
            owner.store().put(b"", b"").unwrap();
            owner.store().put(b"row", b"payload").unwrap();
        }
        let control = StorageReadControl::with_limit(1024);
        let mut image = owner.capture_backup_image(&control).unwrap();
        let mut result = BTreeMap::new();
        loop {
            match image.advance(1).unwrap() {
                KeyValueImageStep::Complete => break,
                KeyValueImageStep::Pending => continue,
                KeyValueImageStep::Unit => (),
            }
            let unit = image.current_unit().unwrap();
            let key = (unit.address.family, unit.address.key.to_vec());
            let len = unit.decoded_bytes as usize;
            let mut bytes = Vec::new();
            while bytes.len() < len {
                assert_eq!(image.read_current(&mut []).unwrap(), 0);
                let mut byte = [0];
                assert_eq!(image.read_current(&mut byte).unwrap(), 1);
                bytes.push(byte[0]);
            }
            result.insert(key, bytes);
        }
        assert_eq!(result.len(), if populated { 3 } else { 1 });
        if populated {
            assert_eq!(result[&(3, b"".to_vec())], 1_u64.to_be_bytes());
            assert_eq!(&result[&(3, b"row".to_vec())][8..], b"payload");
        }
        assert_eq!(control.memory().used(), 0);
        assert_eq!(image.advance(0).unwrap(), KeyValueImageStep::Complete);
    }
}
