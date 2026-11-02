//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;
use uqa_storage::{
    backup::{BackupLimits, BackupReadStep, LocalRepository, Protection},
    key_value::conformance::verify_diskann_restore_source,
    mvcc::{IdentifierRequest, MemoryVersionStore, VersionedPersistence, VersionedSessionOptions},
    PersistentStorageProvider,
};

use super::*;
use crate::{SQLiteRecordStore, SQLiteStorageProvider};

fn open(path: &std::path::Path, mode: usize) -> ManagedConnection {
    match mode {
        0 => ManagedConnection::open(path),
        1 => ManagedConnection::open_encrypted(path, "native-backup"),
        2 => ManagedConnection::open_compressed(path, crate::SQLiteCompressionOptions::default()),
        _ => ManagedConnection::open_compressed_encrypted(
            path,
            "native-backup",
            crate::SQLiteCompressionOptions::default(),
        ),
    }
    .unwrap()
}

#[test]
fn native_backup_capture_preserves_private_transactions_and_the_original_committed_cut() {
    for mode in 0..4 {
        let temporary = tempfile::tempdir().unwrap();
        let connection = open(&temporary.path().join("database"), mode);
        let control = StorageReadControl::with_limit(4096);
        let provider = SQLiteStorageProvider::new(connection.clone());
        assert!(matches!(
            provider.capture_backup_image(&control),
            Err(BackupError::Unsupported(_))
        ));
        assert!(!connection.is_native_record_session());
        connection
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        let catalog = crate::Catalog::open(connection.clone()).unwrap();
        catalog.set_metadata("transaction", "committed").unwrap();
        let setup = StorageReadControl::with_limit(1 << 20);
        let (namespace, snapshot) = connection.capture_native_backup_snapshot(&setup).unwrap();
        let sequence = snapshot.records().sequence();
        drop(snapshot);
        let expected = super::super::NativeRecord::encode(
            NativeRecordFamily::Metadata,
            NativeRecordOwner::Database(namespace),
            &[ValueRef::Text(b"transaction"), ValueRef::Text(b"committed")],
            &setup,
        )
        .unwrap();
        let private = NativeRecordIdentity::new(
            NativeRecordFamily::Metadata,
            NativeRecordOwner::Database(namespace),
        )
        .unwrap()
        .encode_key(&[ValueRef::Text(b"private")], &setup)
        .unwrap();
        connection.begin_transaction().unwrap();
        catalog.set_metadata("transaction", "uncommitted").unwrap();
        catalog.set_metadata("private", "uncommitted").unwrap();
        let mut image = provider.capture_backup_image(&control).unwrap();
        assert_eq!(image.cut().sequence, Some(sequence));
        assert!(connection.in_transaction());
        assert_eq!(
            catalog.get_metadata("transaction").unwrap().as_deref(),
            Some("uncommitted")
        );
        connection.commit_transaction().unwrap();
        let mut found = false;
        loop {
            match image.advance(1).unwrap() {
                BackupSourceStep::Complete => break,
                BackupSourceStep::Pending => continue,
                BackupSourceStep::Unit => (),
            }
            let unit = image.current_unit().unwrap();
            assert_ne!(unit.address.key, &*private);
            let selected = unit.address.family == 3 && unit.address.key == expected.key();
            let mut bytes = Vec::new();
            let mut output = [0; 128];
            loop {
                let count = image.read_current(&mut output).unwrap();
                if count == 0 {
                    break;
                }
                if selected {
                    bytes.extend_from_slice(&output[..count]);
                }
            }
            if selected {
                assert!(!found);
                found = true;
                assert_eq!(&bytes[8..], expected.row());
            }
        }
        assert!(found);
        assert_eq!(control.memory().used(), 0);
        let later = provider.capture_backup_image(&control).unwrap();
        assert!(later.cut().sequence > Some(sequence));
        drop(later);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn native_backup_image_preserves_evaluated_rows_and_watermarks_after_source_loss() {
    for mode in 0..4 {
        let temporary = tempfile::tempdir().unwrap();
        let source_path = temporary.path().join("source");
        std::fs::create_dir(&source_path).unwrap();
        let connection = open(&source_path.join("database"), mode);
        let provider = SQLiteStorageProvider::new(connection.clone());
        let session = provider.open_session().unwrap();
        verify_diskann_restore_source(&*session.backend).unwrap();
        session
            .catalog
            .set_metadata("large", &"x".repeat((512 << 10) + 17))
            .unwrap();
        session
            .catalog
            .set_metadata(
                uqa_storage::mvcc::NOTIFICATION_PUBLICATION_KEY,
                &"n".repeat(512 << 10),
            )
            .unwrap();
        let setup = StorageReadControl::with_limit(32 << 20);
        let records = SQLiteRecordStore::for_native(&connection, &setup).unwrap();
        records
            .allocate_identifiers(b"native.image.ids", IdentifierRequest::Observe(71), &setup)
            .unwrap();
        let namespace = records.native_namespace().unwrap();
        let notification = NativeRecordNamespace(namespace).key(&setup).unwrap();
        let cut = records.capture_backup_snapshot(&setup).unwrap().unwrap();
        let mut expected = BTreeMap::new();
        cut.records()
            .visit_prefix(b"", None, usize::MAX, &setup, &mut |key, record| {
                if key != &*notification {
                    if let Some(value) = record.value {
                        let mut bytes = record.revision.unwrap().as_u64().to_be_bytes().to_vec();
                        bytes.extend_from_slice(value);
                        expected.insert((3u16, key.to_vec()), bytes);
                    }
                }
                Ok(true)
            })
            .unwrap();
        cut.visit_identifier_watermarks(None, usize::MAX, &setup, &mut |key, watermark| {
            if let Some(watermark) = watermark {
                expected.insert((2, key.to_vec()), watermark.to_be_bytes().to_vec());
            }
            Ok(true)
        })
        .unwrap();
        drop(cut);
        let control = StorageReadControl::with_limit(128 << 10);
        let image = provider.capture_backup_image(&control).unwrap();
        let captured = image.cut();
        assert_eq!(captured.format.provider(), ImageProvider::SQLiteNative);
        let writer_path = source_path.join("database");
        let (send, receive) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let writer = open(&writer_path, mode);
            let provider = SQLiteStorageProvider::new(writer);
            let session = provider.open_session().unwrap();
            session
                .catalog
                .set_metadata("large", "replacement")
                .unwrap();
            session
                .catalog
                .set_metadata("after-capture", "must be excluded")
                .unwrap();
            send.send(()).unwrap();
        });
        receive
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("retained image must release every physical read");
        writer.join().unwrap();
        records
            .allocate_identifiers(b"native.image.ids", IdentifierRequest::Observe(99), &setup)
            .unwrap();
        let repository = LocalRepository::create(
            temporary.path().join("repository"),
            Protection::Plaintext,
            BackupLimits::new(4096, 4096).unwrap(),
            &control,
        )
        .unwrap();
        let mut backup = repository
            .begin_backup(captured, None, 16 << 20, &control)
            .unwrap();
        let report = backup.prepare_image(image, 2 << 20, &control).unwrap();
        assert_eq!(report.source.excluded_units, 1);
        assert!(report.source.metadata_read_bytes > 0);
        let root = backup.publish(&control).unwrap();
        drop((backup, session, provider, connection, records));
        assert_eq!(control.memory().used(), 0);
        std::fs::remove_dir_all(source_path).unwrap();
        let lease = repository.open_backup(root.backup(), &control).unwrap();
        verify_image(&lease, captured, namespace, expected, &control);
        drop(lease);
        assert_eq!(control.memory().used(), 0);
        assert!(control.memory().peak() <= 128 << 10);
    }
}

fn verify_image(
    lease: &uqa_storage::backup::BackupLease<'_>,
    captured: BackupCut,
    namespace: DatabaseId,
    mut expected: BTreeMap<(u16, Vec<u8>), Vec<u8>>,
    control: &StorageReadControl,
) {
    let mut reader = lease.read_image(2 << 20, control).unwrap();
    let mut header_seen = false;
    loop {
        match reader.advance(4096).unwrap() {
            BackupReadStep::Pending => continue,
            BackupReadStep::Complete => break,
            BackupReadStep::Unit => (),
        }
        let unit = reader.current_unit().unwrap();
        assert_eq!(unit.address().namespace.as_bytes(), NAMESPACE);
        assert_eq!(unit.address().generation, 0);
        let expected = if unit.address().family == 1 {
            assert!(!header_seen);
            header_seen = true;
            let mut header = Vec::from(b"UQANVI01".as_slice());
            header.extend_from_slice(&captured.source.as_bytes());
            header.extend_from_slice(&captured.sequence.unwrap().as_u64().to_be_bytes());
            header.extend_from_slice(&namespace.as_bytes());
            header.extend_from_slice(&17u32.to_be_bytes());
            header.extend_from_slice(&63u16.to_be_bytes());
            header.extend_from_slice(&49u16.to_be_bytes());
            header
        } else {
            expected
                .remove(&(unit.address().family, unit.address().key.to_vec()))
                .expect("exact retained unit")
        };
        assert_eq!(unit.decoded_bytes(), expected.len() as u64);
        let mut offset = 0;
        let mut buffer = [0; 1024];
        loop {
            let copied = reader.read_current(&mut buffer).unwrap();
            assert_eq!(&buffer[..copied], &expected[offset..offset + copied]);
            offset += copied;
            if copied == 0 {
                break;
            }
        }
        assert_eq!(offset, expected.len());
    }
    assert!(header_seen && expected.is_empty());
    drop(reader);
}

#[test]
fn native_backup_membership_uses_the_captured_generation_state_without_payload_reads() {
    let control = StorageReadControl::with_limit(4096);
    let setup = StorageReadControl::with_limit(1 << 20);
    let namespace = DatabaseId::from_bytes([12; 16]);
    let mapping = Mapping {
        header: [0; HEADER_BYTES],
        namespace,
        notification: NativeRecordNamespace(namespace).key(&control).unwrap(),
    };
    let store = MemoryVersionStore::new(setup.memory());
    for status in 0..6 {
        let mut logical = b"\0uqa-diskann-v1\0".to_vec();
        logical.push(1);
        logical.extend_from_slice(&[5; 16]);
        for value in [7u64, 8, u64::from(status) + 1] {
            logical.extend_from_slice(&value.to_be_bytes());
        }
        logical.push(0);
        let mut state = [0; 18];
        state[0] = 2;
        state[1] = status;
        state[17] = 7;
        let record = super::super::NativeRecord::encode(
            NativeRecordFamily::DiskANNRecords,
            NativeRecordOwner::Database(namespace),
            &[ValueRef::Blob(&logical), ValueRef::Blob(&state)],
            &setup,
        )
        .unwrap();
        let revision = store.commit(&[record.write(None)], &setup).unwrap();
        let cut = store.snapshot().unwrap();
        state[1] = if status == 4 { 5 } else { 4 };
        let changed = super::super::NativeRecord::encode(
            NativeRecordFamily::DiskANNRecords,
            NativeRecordOwner::Database(namespace),
            &[ValueRef::Blob(&logical), ValueRef::Blob(&state)],
            &setup,
        )
        .unwrap();
        store
            .commit(&[changed.write(Some(revision))], &setup)
            .unwrap();
        let metadata_bytes = record.row().len();
        assert_eq!(
            mapping.include(&cut, record.key(), &control).unwrap(),
            (status == 4, metadata_bytes)
        );
        let last = logical.len() - 1;
        logical[last] = 5;
        logical.extend_from_slice(&0u64.to_be_bytes());
        let identity = NativeRecordIdentity::new(
            NativeRecordFamily::DiskANNRecords,
            NativeRecordOwner::Database(namespace),
        )
        .unwrap();
        let key = identity
            .encode_key(&[ValueRef::Blob(&logical)], &control)
            .unwrap();
        assert_eq!(
            mapping.include(&cut, &key, &control).unwrap(),
            (status == 4, metadata_bytes)
        );
    }
}

#[test]
fn native_backup_image_retires_its_cut_on_completion_cancellation_and_memory_failure() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let provider = SQLiteStorageProvider::new(connection);
    let session = provider.open_session().unwrap();
    session
        .catalog
        .set_metadata("large", &"x".repeat(64 << 10))
        .unwrap();
    drop(session);
    for failure in 0..4 {
        let control = StorageReadControl::with_limit(4096);
        let mut image = provider.capture_backup_image(&control).unwrap();
        if failure == 0 {
            control.cancellation().cancel();
            assert!(image.advance(1).is_err());
        } else if failure == 1 {
            assert_eq!(image.advance(1).unwrap(), BackupSourceStep::Unit);
            assert!(
                image.advance(1).is_err(),
                "unconsumed header cannot be skipped"
            );
        } else if failure == 2 {
            assert_eq!(image.advance(1).unwrap(), BackupSourceStep::Unit);
            assert_eq!(
                image.read_current(&mut [0; HEADER_BYTES]).unwrap(),
                HEADER_BYTES
            );
            let memory = control
                .memory()
                .reserve(4096 - control.memory().used())
                .unwrap();
            let mut pending = 0;
            let error = loop {
                match image.advance(1) {
                    Ok(BackupSourceStep::Pending) => {
                        pending += 1;
                        assert!(pending < 100);
                    }
                    Ok(_) => panic!("record scan bypassed exhausted allowance"),
                    Err(error) => break error,
                }
            };
            assert!(matches!(error, BackupError::Memory(_)), "{error:?}");
            drop(memory);
        } else {
            let mut buffer = [0; 128];
            loop {
                let before = image.statistics().visited_positions;
                match image.advance(1).unwrap() {
                    BackupSourceStep::Complete => break,
                    BackupSourceStep::Pending => continue,
                    BackupSourceStep::Unit => (),
                }
                assert!(image.statistics().visited_positions - before <= 1);
                while image.read_current(&mut buffer).unwrap() != 0 {}
            }
            control.cancellation().cancel();
            assert_eq!(image.advance(1).unwrap(), BackupSourceStep::Complete);
        }
        assert_eq!(control.memory().used(), 0);
        assert!(image.current_unit().is_none());
        if failure != 3 {
            assert!(matches!(image.advance(1), Err(BackupError::Finished)));
        }
    }
}

#[test]
fn native_backup_membership_rejects_missing_corrupt_and_misaddressed_state() {
    let setup = StorageReadControl::with_limit(1 << 20);
    let control = StorageReadControl::with_limit(4096);
    let namespace = DatabaseId::from_bytes([12; 16]);
    let mapping = Mapping {
        header: [0; HEADER_BYTES],
        namespace,
        notification: NativeRecordNamespace(namespace).key(&control).unwrap(),
    };
    let store = MemoryVersionStore::new(setup.memory());
    let mut logical = b"\0uqa-diskann-v1\0".to_vec();
    logical.push(1);
    logical.extend_from_slice(&[5; 16]);
    for value in [7u64, 8, 9] {
        logical.extend_from_slice(&value.to_be_bytes());
    }
    logical.push(0);
    let identity = NativeRecordIdentity::new(
        NativeRecordFamily::DiskANNRecords,
        NativeRecordOwner::Database(namespace),
    )
    .unwrap();
    let key = identity
        .encode_key(&[ValueRef::Blob(&logical)], &setup)
        .unwrap();
    assert!(mapping
        .include(&store.snapshot().unwrap(), &key, &control)
        .is_err());
    let mut revision = None;
    for bytes in [vec![], vec![0; 18], vec![0; 64 << 10]] {
        let record = super::super::NativeRecord::encode(
            NativeRecordFamily::DiskANNRecords,
            NativeRecordOwner::Database(namespace),
            &[ValueRef::Blob(&logical), ValueRef::Blob(&bytes)],
            &setup,
        )
        .unwrap();
        revision = Some(store.commit(&[record.write(revision)], &setup).unwrap());
        assert!(mapping
            .include(&store.snapshot().unwrap(), &key, &control)
            .is_err());
        assert!(control.memory().peak() <= 4096);
    }
    let foreign = NativeRecordIdentity::new(
        NativeRecordFamily::Schemas,
        NativeRecordOwner::Database(DatabaseId::from_bytes([13; 16])),
    )
    .unwrap()
    .encode_key(&[ValueRef::Text(b"foreign")], &setup)
    .unwrap();
    assert!(mapping
        .include(&store.snapshot().unwrap(), &foreign, &control)
        .is_err());
    let mut malformed = key.to_vec();
    malformed.push(0);
    assert!(mapping
        .include(&store.snapshot().unwrap(), &malformed, &control)
        .is_err());
    assert!(mapping
        .include(&store.snapshot().unwrap(), b"unknown mapping", &control)
        .is_err());
}
