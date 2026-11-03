//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Chunk reads retain logical versions without retaining BLOB handles or physical transactions.

mod images;

use super::*;
use crate::mvcc::native::{NativeRecordFamily, NativeRecordIdentity, NativeRecordOwner};
use rusqlite::types::ValueRef;
use uqa_core::memory::BudgetedVec;
use uqa_storage::{KeyValueStore, RelationIdentity, RelationSecurityRow, ViewRow};

fn save_view(connection: &ManagedConnection, definition: &str) {
    crate::Catalog::open(connection.clone())
        .unwrap()
        .save_view(&ViewRow {
            relation: RelationIdentity::new("public", "chunk_view"),
            security: RelationSecurityRow::Legacy(uqa_core::catalog_acl::LegacyRelationSecurity {
                role_owner: "uqa".into(),
                acl: None,
                column_acls: std::collections::BTreeMap::new(),
            }),
            definition_json: definition.into(),
        })
        .unwrap();
}

pub(super) fn replace_on_another_connection(path: &std::path::Path, mode: usize, native: bool) {
    let writer_path = path.to_path_buf();
    let (send, receive) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let connection = super::reclamation::open(&writer_path, mode);
        if native {
            connection
                .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
                .unwrap();
            save_view(&connection, "{}");
        } else {
            crate::key_value::SQLiteKeyValueStore::new(connection)
                .unwrap()
                .put(b"large", b"new")
                .unwrap();
        }
        send.send(()).unwrap();
    });
    receive
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("a returned chunk must release its physical read");
    writer.join().unwrap();
}

fn open_source(
    path: &std::path::Path,
    mode: usize,
    native: bool,
    definition: &str,
    setup: &StorageReadControl,
) -> (SQLiteRecordStore, Vec<u8>) {
    let connection = super::reclamation::open(path, mode);
    if native {
        crate::Catalog::open(connection.clone()).unwrap();
        connection
            .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
            .unwrap();
        save_view(&connection, definition);
        let store = SQLiteRecordStore::for_native(&connection, setup).unwrap();
        let key = NativeRecordIdentity::new(
            NativeRecordFamily::Views,
            NativeRecordOwner::Database(store.native_namespace().unwrap()),
        )
        .unwrap()
        .encode_key(
            &[ValueRef::Text(b"public"), ValueRef::Text(b"chunk_view")],
            setup,
        )
        .unwrap()
        .to_vec();
        (store, key)
    } else {
        crate::key_value::SQLiteKeyValueStore::new(connection.clone())
            .unwrap()
            .put(b"large", definition.as_bytes())
            .unwrap();
        (
            SQLiteRecordStore::for_key_value(&connection, setup).unwrap(),
            b"large".to_vec(),
        )
    }
}

#[test]
fn record_chunks_keep_large_values_and_writer_progress_in_every_mapping_and_file_mode() {
    for mode in 0..4 {
        for native in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("chunks.db");
            let setup = StorageReadControl::with_limit(32 << 20);
            let definition = format!("{{\"value\":\"{}\"}}", "x".repeat((512 << 10) + 17));
            let (store, key) = open_source(&path, mode, native, &definition, &setup);
            let control = StorageReadControl::with_limit(4096);
            let cut = store.capture_backup_snapshot(&control).unwrap().unwrap();
            let record = cut.records().get(&key, &setup).unwrap().unwrap();
            let expected = record.value().unwrap();
            assert!(matches!(
                cut.records().get(&key, &control),
                Err(VersionError::Memory(_))
            ));
            let mut output = BudgetedVec::new(control.memory());
            output.extend_from_slice(&[0; 1024]).unwrap();
            let mut offset = 0;
            while offset < expected.len() {
                let chunk = cut
                    .records()
                    .read_value_chunk(&key, offset, &mut output, &control)
                    .unwrap()
                    .unwrap();
                assert_eq!(chunk.revision(), record.sequence());
                assert_eq!(chunk.total_bytes(), Some(expected.len()));
                assert_eq!(chunk.copied(), output.len().min(expected.len() - offset));
                assert_eq!(
                    &output[..chunk.copied()],
                    &expected[offset..offset + chunk.copied()]
                );
                if offset == 0 {
                    replace_on_another_connection(&path, mode, native);
                    store.reclaim_versions(&setup).unwrap();
                }
                offset += chunk.copied();
            }
            assert_eq!(
                cut.records()
                    .read_value_chunk(&key, offset, &mut output, &control)
                    .unwrap()
                    .unwrap()
                    .copied(),
                0
            );
            assert!(cut
                .records()
                .read_value_chunk(&key, usize::MAX, &mut output, &control)
                .is_err());
            assert!(control.memory().peak() <= 4096);
            drop(output);
            drop(cut);
            assert_eq!(control.memory().used(), 0);
        }
    }
}

#[test]
fn record_chunks_reject_corrupted_lengths_and_leave_the_read_owner_usable() {
    let store = SQLiteRecordStore::new(&ManagedConnection::open_in_memory().unwrap()).unwrap();
    let setup = control();
    let transaction = store.allocate_transaction(&setup).unwrap();
    store
        .commit(
            transaction,
            &PreparedRecordCommit::new(
                &[RecordWrite {
                    key: b"a",
                    expected: None,
                    value: Some(b"abcd"),
                }],
                &setup,
            )
            .unwrap(),
            &setup,
        )
        .unwrap();
    let snapshot = store.snapshot(&setup).unwrap();
    store
        .with(|connection| {
            let _permit = schema::WritePermit::acquire(connection)?;
            connection.execute(
                "UPDATE _uqa_mvcc_version_metadata SET payload_length = 8",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let mut output = [0xcc; 3];
    assert!(snapshot
        .read_value_chunk(b"a", 0, &mut output, &setup)
        .is_err());
    assert_eq!(output, [0xcc; 3]);
    store
        .with(|connection| {
            let _permit = schema::WritePermit::acquire(connection)?;
            connection.execute(
                "UPDATE _uqa_mvcc_version_metadata SET payload_length = 4",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        snapshot
            .read_value_chunk(b"a", 1, &mut output, &setup)
            .unwrap()
            .unwrap()
            .copied(),
        3
    );
    assert_eq!(output, *b"bcd");
    assert!(snapshot
        .read_value_chunk(b"absent", 0, &mut output, &setup)
        .unwrap()
        .is_none());
    let cancelled = control();
    cancelled.cancellation().cancel();
    assert!(matches!(
        snapshot.read_value_chunk(b"a", 0, &mut output, &cancelled),
        Err(VersionError::Cancelled(_))
    ));
    assert_eq!(output, *b"bcd");
}

#[test]
fn record_chunks_preserve_compacted_runs_empty_values_and_tombstones() {
    let store = SQLiteRecordStore::new(&ManagedConnection::open_in_memory().unwrap()).unwrap();
    let control = control();
    let keys: Vec<_> = (0..128_u64).map(u64::to_be_bytes).collect();
    let writes: Vec<_> = keys
        .iter()
        .map(|key| RecordWrite {
            key,
            expected: None,
            value: Some(b"template"),
        })
        .collect();
    let transaction = store.allocate_transaction(&control).unwrap();
    store
        .commit(
            transaction,
            &PreparedRecordCommit::new(&writes, &control).unwrap(),
            &control,
        )
        .unwrap();
    store.reclaim_versions(&control).unwrap();
    assert!(
        store
            .with(|connection| Ok(connection.query_row(
                "SELECT count(*) FROM _uqa_mvcc_runs",
                [],
                |row| row.get::<_, i64>(0)
            )?))
            .unwrap()
            > 0
    );
    let snapshot = store.snapshot(&control).unwrap();
    let mut output = [0; 3];
    for key in &keys {
        let chunk = snapshot
            .read_value_chunk(key, 2, &mut output, &control)
            .unwrap()
            .unwrap();
        assert_eq!(chunk.total_bytes(), Some(8));
        assert_eq!(output, *b"mpl");
    }
    let transaction = store.allocate_transaction(&control).unwrap();
    store
        .commit(
            transaction,
            &PreparedRecordCommit::new(
                &[
                    RecordWrite {
                        key: b"empty",
                        expected: None,
                        value: Some(b""),
                    },
                    RecordWrite {
                        key: b"deleted",
                        expected: None,
                        value: None,
                    },
                ],
                &control,
            )
            .unwrap(),
            &control,
        )
        .unwrap();
    let latest = store.snapshot(&control).unwrap();
    for (key, length) in [(b"empty".as_slice(), Some(0)), (b"deleted", None)] {
        let chunk = latest
            .read_value_chunk(key, 0, &mut output, &control)
            .unwrap()
            .unwrap();
        assert_eq!(chunk.total_bytes(), length);
        assert_eq!(chunk.copied(), 0);
        assert!(latest
            .read_value_chunk(key, 1, &mut output, &control)
            .is_err());
    }
}
