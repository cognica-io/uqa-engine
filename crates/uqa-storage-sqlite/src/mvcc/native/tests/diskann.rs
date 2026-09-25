//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_storage::mvcc::{PreparedRecordCommit, RecordWrite, VersionedPersistence};

use super::{materialization::with, persistence::connection, *};
use crate::SQLiteRecordStore;

pub(in crate::mvcc::native) fn remove_empty_table(
    sqlite: &rusqlite::Connection,
) -> crate::Result<()> {
    remove_empty_origins(sqlite)?;
    assert_eq!(
        sqlite.query_row(
            "SELECT count(*) FROM _uqa_mvcc_native_diskann_records",
            [],
            |row| row.get::<_, i64>(0)
        )?,
        0
    );
    sqlite.execute_batch("DROP TABLE _uqa_mvcc_native_diskann_records")?;
    Ok(())
}

pub(in crate::mvcc::native) fn remove_empty_origins(
    sqlite: &rusqlite::Connection,
) -> crate::Result<()> {
    assert_eq!(
        sqlite.query_row(
            "SELECT count(*) FROM _uqa_mvcc_native_vector_origins",
            [],
            |row| row.get::<_, i64>(0)
        )?,
        0
    );
    sqlite.execute_batch("DROP TABLE _uqa_mvcc_native_vector_origins")?;
    Ok(())
}

#[test]
fn native_diskann_blob_records_preserve_bytes_snapshots_guards_and_cold_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(1 << 22);
    for mode in 0..4 {
        let path = directory.path().join(format!("native-diskann-{mode}.db"));
        let namespace;
        let mut payload = [255; 4096];
        payload[..4].copy_from_slice(&[0, 1, 0, 128]);
        let key = [0, 255, 0, 3];
        {
            let connection = connection(&path, mode);
            let persistence = SQLiteRecordStore::for_native(&connection, &control).unwrap();
            namespace = persistence.native_namespace().unwrap();
            let record = NativeRecord::encode(
                NativeRecordFamily::DiskANNRecords,
                NativeRecordOwner::Database(namespace),
                &[ValueRef::Blob(&key), ValueRef::Blob(&payload)],
                &control,
            )
            .unwrap();
            let before = persistence.snapshot(&control).unwrap();
            let transaction = persistence.allocate_transaction(&control).unwrap();
            let receipt = persistence
                .commit(
                    transaction,
                    &PreparedRecordCommit::new(&[record.write(None)], &control).unwrap(),
                    &control,
                )
                .unwrap();
            assert!(before.get(record.key(), &control).unwrap().is_none());
            let retained = persistence.snapshot(&control).unwrap();
            assert_retained(&*retained, &record, &control);
            assert_physical(&connection, &key, &payload);
            let transaction = persistence.allocate_transaction(&control).unwrap();
            persistence
                .commit(
                    transaction,
                    &PreparedRecordCommit::new(
                        &[RecordWrite {
                            key: record.key(),
                            expected: Some(receipt.sequence),
                            value: None,
                        }],
                        &control,
                    )
                    .unwrap(),
                    &control,
                )
                .unwrap();
            persistence.reclaim_versions(&control).unwrap();
            assert!(persistence
                .snapshot(&control)
                .unwrap()
                .get(record.key(), &control)
                .unwrap()
                .unwrap()
                .value()
                .is_none());
            assert_retained(&*retained, &record, &control);
            let head = persistence
                .snapshot(&control)
                .unwrap()
                .get(record.key(), &control)
                .unwrap()
                .unwrap()
                .sequence();
            let transaction = persistence.allocate_transaction(&control).unwrap();
            persistence
                .commit(
                    transaction,
                    &PreparedRecordCommit::new(&[record.write(Some(head))], &control).unwrap(),
                    &control,
                )
                .unwrap();
        }
        let connection = connection(&path, mode);
        let reopened = SQLiteRecordStore::for_native(&connection, &control).unwrap();
        assert_eq!(reopened.native_namespace(), Some(namespace));
        assert_physical(&connection, &key, &payload);
    }
}

#[test]
fn native_diskann_family_is_distinct_from_standalone_graph_and_keeps_stable_ids() {
    assert_eq!(NativeRecordFamily::DiskANNRecords.id(), 57);
    assert_eq!(NativeRecordFamily::StandaloneGraphLookups.id(), 56);
    assert!(!NativeRecordFamily::DiskANNRecords.is_standalone_graph());
    assert_eq!(
        NativeRecordFamily::DiskANNRecords.layout().columns,
        ["key", "value"]
    );
    assert!(!NativeRecordFamily::DiskANNRecords.layout().object_owned);
    for family in NativeRecordFamily::all() {
        assert_eq!(NativeRecordFamily::from_id(family.id()), Some(family));
        assert_eq!(family.layout().family, family);
    }
}

fn assert_retained(
    snapshot: &dyn uqa_storage::mvcc::CommittedRecordSnapshot,
    record: &NativeRecord,
    control: &StorageReadControl,
) {
    assert_eq!(
        &***snapshot
            .get(record.key(), control)
            .unwrap()
            .unwrap()
            .value()
            .unwrap(),
        record.row()
    );
}

fn assert_physical(connection: &ManagedConnection, key: &[u8], payload: &[u8]) {
    with(connection, |sqlite| {
        let (kind, bytes): (String, Vec<u8>) = sqlite.query_row(
            "SELECT typeof(value),value FROM _uqa_mvcc_native_diskann_records WHERE key=?1",
            [key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(kind, "blob");
        assert_eq!(bytes, payload);
        assert!(sqlite
            .execute("DELETE FROM _uqa_mvcc_native_diskann_records", [])
            .is_err());
        Ok(())
    });
}
