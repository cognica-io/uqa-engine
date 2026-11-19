//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Logical image membership and writer progress across all `SQLite` KV file modes.

use super::*;
use std::collections::BTreeMap;
use uqa_core::memory::BudgetedVec;
use uqa_storage::{
    key_value::{
        conformance::verify_diskann_restore_source, KeyValueImageStep, KeyValueNotificationRecords,
    },
    mvcc::NotificationRecordLayout,
    KeyValueStore,
};

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
#[test]
fn sqlite_kv_backup_repository_round_trip_survives_source_loss_in_every_file_mode() {
    use uqa_storage::backup::{
        BackupKey, BackupLimits, BackupReadStep, LocalRepository, Protection, ProtectionId,
    };
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("source");
        std::fs::create_dir(&source_path).unwrap();
        let path = source_path.join("image.db");
        let connection = super::reclamation::open(&path, mode);
        let owner =
            crate::key_value::SQLiteKeyValueStorage::from_connection(connection.clone()).unwrap();
        let setup = StorageReadControl::with_limit(32 << 20);
        verify_diskann_restore_source(&owner.backend()).unwrap();
        let store = owner.store();
        let large = vec![63; (512 << 10) + 17];
        store.put(b"large", &large).unwrap();
        let notification = KeyValueNotificationRecords.key(&setup).unwrap();
        store.put(&notification, &large).unwrap();
        let records = SQLiteRecordStore::for_key_value(&connection, &setup).unwrap();
        let snapshot = records.snapshot(&setup).unwrap();
        let mut expected = BTreeMap::new();
        snapshot
            .visit_prefix(b"", None, usize::MAX, &setup, &mut |key, record| {
                if key != &*notification {
                    if let Some(value) = record.value {
                        let mut bytes = record.revision.unwrap().as_u64().to_be_bytes().to_vec();
                        bytes.extend_from_slice(value);
                        expected.insert(key.to_vec(), bytes);
                    }
                }
                Ok(true)
            })
            .unwrap();
        drop(snapshot);
        drop(records);
        let control = StorageReadControl::with_limit(128 * 1024);
        let image = owner.capture_backup_image(&control).unwrap();
        let cut = image.cut();
        super::chunks::replace_on_another_connection(&path, mode, false);
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
        drop(connection);
        std::fs::remove_dir_all(source_path).unwrap();
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
                    .expect("exact committed image records")
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
}

#[test]
fn sqlite_kv_backup_image_preserves_live_artifacts_and_writer_progress_in_every_file_mode() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("image.db");
        let connection = super::reclamation::open(&path, mode);
        let owner =
            crate::key_value::SQLiteKeyValueStorage::from_connection(connection.clone()).unwrap();
        let setup = StorageReadControl::with_limit(32 << 20);
        verify_diskann_restore_source(&owner.backend()).unwrap();
        let store = owner.store();
        let notification = KeyValueNotificationRecords.key(&setup).unwrap();
        let large = vec![63; (512 << 10) + 17];
        store.put(b"large", &large).unwrap();
        store.put(&notification, &large).unwrap();
        store.begin_transaction().unwrap();
        store.put(b"private", b"not committed").unwrap();
        let records = SQLiteRecordStore::for_key_value(&connection, &setup).unwrap();
        let snapshot = records.snapshot(&setup).unwrap();
        let mut expected = BTreeMap::new();
        snapshot
            .visit_prefix(b"", None, usize::MAX, &setup, &mut |key, record| {
                if key != &*notification {
                    if let Some(value) = record.value {
                        let mut bytes = record.revision.unwrap().as_u64().to_be_bytes().to_vec();
                        bytes.extend_from_slice(value);
                        expected.insert(key.to_vec(), bytes);
                    }
                }
                Ok(true)
            })
            .unwrap();
        assert!(!expected.contains_key(b"private".as_slice()));
        let control = StorageReadControl::with_limit(4096);
        let mut image = owner.capture_backup_image(&control).unwrap();
        store.rollback_transaction().unwrap();
        let mut output = BudgetedVec::new(control.memory());
        output.extend_from_slice(&[0; 1024]).unwrap();
        let mut wrote = false;
        let mut units = 0;
        loop {
            let before = image.visited_positions();
            let step = image.advance(4).unwrap();
            assert!(image.visited_positions() - before <= 4);
            match step {
                KeyValueImageStep::Complete => break,
                KeyValueImageStep::Pending => continue,
                KeyValueImageStep::Unit => (),
            }
            let unit = image.current_unit().unwrap();
            let family = unit.address.family;
            let key = unit.address.key.to_vec();
            let bytes = if family == 3 {
                Some(expected.remove(&key).expect("only committed image records"))
            } else {
                None
            };
            let length = unit.decoded_bytes.expect("record image length") as usize;
            let mut offset = 0;
            while offset < length {
                let copied = image.read_current(&mut output).unwrap();
                assert!(copied > 0);
                if let Some(expected) = &bytes {
                    assert_eq!(&output[..copied], &expected[offset..offset + copied]);
                }
                offset += copied;
                if key == b"large" && !wrote {
                    super::chunks::replace_on_another_connection(&path, mode, false);
                    records.reclaim_versions(&setup).unwrap();
                    wrote = true;
                }
            }
            units += 1;
        }
        assert!(units > 20 && wrote);
        assert!(expected.is_empty());
        assert_eq!(image.excluded_units(), 1);
        assert!(image.metadata_read_bytes() > 0);
        assert!(control.memory().peak() <= 4096);
        drop(output);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(owner.store().get(b"large").unwrap(), Some(b"new".to_vec()));
    }
}
