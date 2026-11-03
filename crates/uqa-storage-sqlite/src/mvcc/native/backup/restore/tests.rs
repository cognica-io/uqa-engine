//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ManagedConnection, SQLiteCompressionOptions, SQLiteRecordStore, SQLiteStorageProvider,
};
use std::{collections::BTreeMap, fs::OpenOptions};
use uqa_storage::{
    backup::{BackupLimits, LocalRepository, Protection},
    key_value::conformance::{
        verify_diskann_restore_source, verify_diskann_restored, verify_diskann_restored_writes,
    },
    mvcc::{IdentifierRequest, VersionedPersistence},
    PersistentStorageProvider, StorageEncryptionKey,
};

mod incremental;

fn open(path: &Path, mode: usize) -> crate::Result<ManagedConnection> {
    match mode {
        0 => ManagedConnection::open(path),
        1 => ManagedConnection::open_encrypted(path, "native-image"),
        2 => ManagedConnection::open_compressed(path, SQLiteCompressionOptions::default()),
        _ => ManagedConnection::open_compressed_encrypted(
            path,
            "native-image",
            SQLiteCompressionOptions::default(),
        ),
    }
}
fn options(mode: usize) -> SQLiteBackupImportOptions {
    SQLiteBackupImportOptions {
        compression: (mode >= 2).then(SQLiteCompressionOptions::default),
        encryption_key: (mode % 2 == 1).then(|| StorageEncryptionKey::new("native-image")),
        maximum_value_bytes: 1 << 20,
        maximum_workspace_bytes: 2 << 20,
    }
}
fn empty(path: &Path) {
    drop(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap(),
    );
}
fn rows(
    records: &SQLiteRecordStore,
    control: &StorageReadControl,
) -> BTreeMap<Vec<u8>, (u64, Vec<u8>)> {
    let mut rows = BTreeMap::new();
    records
        .snapshot(control)
        .unwrap()
        .visit_prefix(b"", None, usize::MAX, control, &mut |key, row| {
            if let Some(value) = row.value {
                rows.insert(
                    key.to_vec(),
                    (row.revision.unwrap().as_u64(), value.to_vec()),
                );
            }
            Ok(true)
        })
        .unwrap();
    rows
}
fn capture(
    provider: &SQLiteStorageProvider,
    path: &Path,
    control: &StorageReadControl,
) -> (LocalRepository, BackupRoot) {
    let image = provider.capture_backup_image(control).unwrap();
    let repository = LocalRepository::create(
        path,
        Protection::Plaintext,
        BackupLimits::new(4096, 4096).unwrap(),
        control,
    )
    .unwrap();
    let root = {
        let mut backup = repository
            .begin_backup(image.cut(), None, 16 << 20, control)
            .unwrap();
        backup.prepare_image(image, 2 << 20, control).unwrap();
        backup.publish(control).unwrap()
    };
    (repository, root)
}
fn complete(import: &mut SQLiteNativeBackupImport<'_>) {
    for _ in 0..100_000 {
        let before = import.imported_records() + import.imported_identifiers();
        let step = import.advance().unwrap();
        assert!(import.imported_records() + import.imported_identifiers() - before <= 1);
        if step == SQLiteBackupImportStep::Complete {
            return;
        }
    }
    panic!("native image import did not finish");
}

#[test]
fn native_backup_import_preserves_projection_history_artifacts_and_allocators() {
    for combination in 0..16 {
        let mode = combination % 4;
        let source_mode = combination / 4;
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        std::fs::create_dir(&source).unwrap();
        let control = StorageReadControl::with_limit(8 << 20);
        let connection = open(&source.join("database"), source_mode).unwrap();
        let mut graph = crate::SQLiteGraphStore::open(connection.clone(), Some("image")).unwrap();
        graph.create_graph("g").unwrap();
        graph
            .add_vertex(uqa_core::Vertex::new(1, "restored"), "g")
            .unwrap();
        drop(graph);
        let provider = SQLiteStorageProvider::new(connection.clone());
        let session = provider.open_session().unwrap();
        let generation = verify_diskann_restore_source(&*session.backend).unwrap();
        sequence(&connection);
        session
            .catalog
            .set_metadata("large", &"n".repeat((512 << 10) + 17))
            .unwrap();
        let records = SQLiteRecordStore::for_native(&connection, &control).unwrap();
        records
            .allocate_identifiers(
                b"native-image-ids",
                IdentifierRequest::Observe(77),
                &control,
            )
            .unwrap();
        let namespace = records.native_namespace().unwrap();
        let expected = rows(&records, &control);
        let (repository, root) = capture(&provider, &directory.path().join("backup"), &control);
        session.catalog.set_metadata("large", "later").unwrap();
        drop((records, session, provider, connection));
        std::fs::remove_dir_all(source).unwrap();
        let lease = repository.open_backup(root.backup(), &control).unwrap();
        let request = DatabaseRestore::new(root.cut().source).unwrap();
        let destination = directory.path().join("destination");
        empty(&destination);
        let mut import =
            SQLiteNativeBackupImport::new(&destination, &lease, request, options(mode), &control)
                .unwrap();
        complete(&mut import);
        assert_eq!(import.imported_records(), expected.len() as u64);
        assert_eq!(control.memory().used(), 0);
        let restored = open(&destination, mode).unwrap();
        let records = SQLiteRecordStore::for_native(&restored, &control).unwrap();
        assert_eq!(records.database_id(), request.target());
        assert_eq!(records.native_namespace(), Some(namespace));
        assert_eq!(rows(&records, &control), expected);
        assert_eq!(
            records
                .identifier_watermark(b"native-image-ids", &control)
                .unwrap(),
            Some(77)
        );
        let provider = SQLiteStorageProvider::new(restored.clone());
        let session = provider.open_session().unwrap();
        let graph = crate::SQLiteGraphStore::open(restored.clone(), Some("image")).unwrap();
        assert_eq!(graph.get_vertex(1).unwrap().unwrap().label, "restored");
        drop(graph);
        verify_diskann_restored(&*session.backend, generation).unwrap();
        verify_diskann_restored_writes(&*session.backend, generation).unwrap();
        session
            .catalog
            .set_metadata("after-restore", "keep")
            .unwrap();
        let later = rows(&records, &control);
        drop((session, provider, records, restored));
        SQLiteNativeBackupImport::recover(&destination, root, request, options(mode), &control)
            .unwrap();
        let restored = open(&destination, mode).unwrap();
        let records = SQLiteRecordStore::for_native(&restored, &control).unwrap();
        assert_eq!(rows(&records, &control), later);
        control.cancellation().cancel();
        assert_eq!(import.advance().unwrap(), SQLiteBackupImportStep::Complete);
    }
}

#[test]
fn native_backup_import_interrupted_and_underfunded_projection_remains_unavailable() {
    let directory = tempfile::tempdir().unwrap();
    let setup = StorageReadControl::with_limit(8 << 20);
    let connection = ManagedConnection::open_in_memory().unwrap();
    let provider = SQLiteStorageProvider::new(connection);
    let session = provider.open_session().unwrap();
    session
        .catalog
        .set_metadata("large", &"x".repeat(256 << 10))
        .unwrap();
    let (repository, root) = capture(&provider, &directory.path().join("backup"), &setup);
    let lease = repository.open_backup(root.backup(), &setup).unwrap();
    for failure in 0..7 {
        let control =
            StorageReadControl::with_limit(if failure == 2 { 128 << 10 } else { 8 << 20 });
        let path = directory.path().join(format!("failed-{failure}"));
        empty(&path);
        let request = DatabaseRestore::new(root.cut().source).unwrap();
        let mut options = options(0);
        if failure == 3 {
            options.maximum_value_bytes = 1024;
        }
        let mut import =
            SQLiteNativeBackupImport::new(&path, &lease, request, options.clone(), &control)
                .unwrap();
        if failure >= 4 {
            while !matches!(import.state, State::Projecting { .. }) {
                assert_eq!(import.advance().unwrap(), SQLiteBackupImportStep::Pending);
            }
            if failure == 5 {
                while !matches!(import.state, State::Publishing { .. }) {
                    assert_eq!(import.advance().unwrap(), SQLiteBackupImportStep::Pending);
                }
            }
            if failure == 6 {
                control.cancellation().cancel();
            }
        }
        if failure == 1 {
            control.cancellation().cancel();
        }
        if matches!(failure, 1..=3 | 6) {
            loop {
                match import.advance() {
                    Err(_) => break,
                    Ok(SQLiteBackupImportStep::Complete) => panic!("invalid import completed"),
                    Ok(SQLiteBackupImportStep::Pending) => (),
                }
            }
            assert_eq!(control.memory().used(), 0);
        }
        drop(import);
        assert_eq!(control.memory().used(), 0);
        assert!(open(&path, 0).is_err());
        assert!(SQLiteNativeBackupImport::recover(&path, root, request, options, &setup).is_err());
    }
}

fn sequence(connection: &ManagedConnection) {
    crate::Catalog::open(connection.clone())
        .unwrap()
        .create_sequence_row(&uqa_storage::catalog::SequenceRow {
            relation: uqa_storage::RelationIdentity::new("public", "image_sequence"),
            security: uqa_storage::SequenceSecurityRow::legacy("uqa"),
            object_id: [51; 16],
            definition_generation: [52; 16],
            start: 1,
            increment: 2,
            current: 17,
            called: true,
            log_count: 0,
            persistence: "p".into(),
            options: uqa_storage::catalog::SequenceOptions::default(),
            owner: None,
        })
        .unwrap();
}

#[test]
fn native_backup_import_rejects_valid_archives_with_invalid_physical_rows() {
    use super::super::test_image::{self, Image};
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(8 << 20);
    let connection = ManagedConnection::open_in_memory().unwrap();
    let provider = SQLiteStorageProvider::new(connection.clone());
    let session = provider.open_session().unwrap();
    verify_diskann_restore_source(&*session.backend).unwrap();
    sequence(&connection);
    let original = Image::capture(provider.capture_backup_image(&control).unwrap());
    for failure in 0..10 {
        let mut image = original.clone();
        corrupt(&mut image, failure, &control);
        image
            .units
            .sort_by(|a, b| (a.family, &a.key).cmp(&(b.family, &b.key)));
        let (repository, root) = test_image::repository(
            &directory.path().join(format!("archive-{failure}")),
            image,
            &control,
        );
        let lease = repository.open_backup(root.backup(), &control).unwrap();
        let path = directory.path().join(format!("invalid-{failure}"));
        empty(&path);
        let request = DatabaseRestore::new(root.cut().source).unwrap();
        let mut import =
            SQLiteNativeBackupImport::new(&path, &lease, request, options(0), &control).unwrap();
        loop {
            match import.advance() {
                Err(_) => break,
                Ok(SQLiteBackupImportStep::Complete) => {
                    panic!("invalid row case {failure} activated")
                }
                Ok(SQLiteBackupImportStep::Pending) => (),
            }
        }
        assert_eq!(control.memory().used(), 0);
        assert!(matches!(import.advance(), Err(BackupError::Finished)));
        assert!(open(&path, 0).is_err());
        assert!(
            SQLiteNativeBackupImport::recover(&path, root, request, options(0), &control).is_err()
        );
    }
}

fn corrupt(
    image: &mut super::super::test_image::Image,
    failure: usize,
    control: &StorageReadControl,
) {
    use super::super::super::{
        decode_record, NativeRecord, NativeRecordFamily as Family, NativeRecordIdentity,
        NativeRecordOwner,
    };
    use super::super::test_image::Unit;
    use rusqlite::types::ValueRef;
    let family = |unit: &Unit| {
        (unit.family == 3).then(|| NativeRecordIdentity::decode(&unit.key).unwrap().family())
    };
    let selected = match failure {
        0 | 1 | 6 => Family::Metadata,
        2 | 3 => Family::TableOwners,
        4 => Family::Documents,
        5 => Family::SequenceValues,
        7 => Family::Sequences,
        _ => Family::DiskANNRecords,
    };
    let index = image
        .units
        .iter()
        .position(|unit| family(unit) == Some(selected))
        .unwrap();
    match failure {
        0 => image.units[index].bytes[8] ^= 1,
        1 => {
            let (id, values) = decode_record(
                &image.units[index].key,
                &image.units[index].bytes[8..],
                control,
            )
            .unwrap();
            let changed = NativeRecord::encode(
                id.family(),
                id.owner(),
                &[ValueRef::Text(b"wrong-key"), values[1]],
                control,
            )
            .unwrap();
            image.units[index].bytes.truncate(8);
            image.units[index].bytes.extend_from_slice(changed.row());
        }
        2 | 5 | 7 => {
            image.units.retain(|unit| family(unit) != Some(selected));
        }
        3 | 4 => {
            let unit = &image.units[index];
            let (id, mut values) = decode_record(&unit.key, &unit.bytes[8..], control).unwrap();
            let owner = if failure == 3 {
                values[2] = ValueRef::Blob(&[99; 16]);
                id.owner()
            } else {
                let NativeRecordOwner::Object { identity, .. } = id.owner() else {
                    panic!("object fixture");
                };
                NativeRecordOwner::Object {
                    identity,
                    generation: [99; 16],
                }
            };
            let changed = NativeRecord::encode(id.family(), owner, &values, control).unwrap();
            let mut unit = unit.clone();
            unit.key = changed.key().to_vec();
            unit.bytes.truncate(8);
            unit.bytes.extend_from_slice(changed.row());
            if failure == 3 {
                image.units[index] = unit;
            } else {
                image.units.push(unit);
            }
        }
        6 => {
            image.units.retain(|unit| {
                if family(unit) != Some(Family::Metadata) {
                    return true;
                }
                let (_, values) = decode_record(&unit.key, &unit.bytes[8..], control).unwrap();
                values[0] != ValueRef::Text(b"schema_version")
            });
        }
        8 | 9 => {
            let state = image
                .units
                .iter()
                .position(|unit| {
                    if family(unit) != Some(Family::DiskANNRecords) {
                        return false;
                    }
                    let (_, values) = decode_record(&unit.key, &unit.bytes[8..], control).unwrap();
                    matches!(values[1], ValueRef::Blob(bytes) if bytes.len() == 18)
                })
                .unwrap();
            if failure == 8 {
                image.units.remove(state);
            } else {
                image.units[state].bytes.pop();
            }
        }
        _ => unreachable!(),
    }
}
