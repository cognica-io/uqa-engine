//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exercise the native codec with real document/blob rows and the shared record persistence contract. Native SQL routing and current-state materialization are separate integration requirements.

use std::{collections::BTreeMap, path::Path};

use uqa_core::Value;
use uqa_storage::mvcc::{PreparedRecordCommit, VersionedPersistence};
use uqa_storage::{DocumentMetadata, DocumentStore, StoredDocument};

use super::*;
use crate::{SQLiteCompressionOptions, SQLiteDocumentStore, SQLiteRecordStore};

pub(super) fn connection(path: &Path, mode: usize) -> ManagedConnection {
    match mode {
        0 => ManagedConnection::open(path),
        1 => ManagedConnection::open_encrypted(path, "native record codec test"),
        2 => ManagedConnection::open_compressed(path, SQLiteCompressionOptions::default()),
        _ => ManagedConnection::open_compressed_encrypted(
            path,
            "native record codec test",
            SQLiteCompressionOptions::default(),
        ),
    }
    .unwrap()
}

fn source_records(
    connection: &ManagedConnection,
    database: DatabaseId,
    control: &StorageReadControl,
) -> Vec<NativeRecord> {
    connection
        .with(|connection| {
            let mut records = Vec::new();
            for family in [
                NativeRecordFamily::Documents,
                NativeRecordFamily::DocumentBlobs,
                NativeRecordFamily::Metadata,
                NativeRecordFamily::Schemas,
            ] {
                let layout = family.layout();
                let mut statement =
                    connection.prepare(&format!("SELECT * FROM {}", layout.table))?;
                let mut rows = statement.query([])?;
                while let Some(row) = rows.next()? {
                    let values = (0..layout.columns.len())
                        .map(|column| row.get_ref(column))
                        .collect::<Result<Vec<_>, _>>()?;
                    let owner = if layout.object_owned {
                        owner()
                    } else {
                        NativeRecordOwner::Database(database)
                    };
                    let record = NativeRecord::encode(family, owner, &values, control).unwrap();
                    let (_, decoded) = decode_record(record.key(), record.row(), control).unwrap();
                    assert_eq!(&*decoded, &values);
                    drop(decoded);
                    records.push(record);
                }
            }
            Ok(records)
        })
        .unwrap()
}

#[test]
fn encoded_native_rows_keep_binary_fields_and_tuple_metadata_through_durable_records() {
    let directory = tempfile::tempdir().unwrap();
    for mode in 0..4 {
        let path = directory.path().join(format!("native-codec-{mode}.db"));
        let control = StorageReadControl::with_limit(1 << 22);
        let (records, receipt) = {
            let connection = connection(&path, mode);
            Catalog::open(connection.clone()).unwrap();
            let mut documents = SQLiteDocumentStore::new(connection.clone(), "public.native");
            let fields = BTreeMap::from([
                ("body".to_owned(), Value::Str("native\0text 日本어".into())),
                ("bytes".to_owned(), Value::Bytes(vec![255; 8192])),
                ("count".to_owned(), Value::Int(i64::MAX)),
            ]);
            documents
                .put_stored(
                    17,
                    StoredDocument::with_metadata(
                        fields.clone(),
                        DocumentMetadata::with_tuple_xmin(u32::MAX),
                    ),
                )
                .unwrap();
            let persistence = SQLiteRecordStore::new(&connection).unwrap();
            let records = source_records(&connection, persistence.database_id(), &control);
            assert!(records.len() >= 4);
            let before = persistence.snapshot(&control).unwrap();
            let writes: Vec<_> = records.iter().map(|record| record.write(None)).collect();
            let prepared = PreparedRecordCommit::new(&writes, &control).unwrap();
            let id = persistence.allocate_transaction(&control).unwrap();
            let receipt = persistence.commit(id, &prepared, &control).unwrap();
            for record in &records {
                assert!(before.get(record.key(), &control).unwrap().is_none());
            }
            let stored = documents.get_stored(17).unwrap().unwrap();
            assert_eq!(stored.metadata().tuple_xmin(), Some(u32::MAX));
            assert_eq!(stored.fields(), &fields);
            (records, receipt)
        };
        let reopened = SQLiteRecordStore::new(&connection(&path, mode)).unwrap();
        assert_eq!(
            reopened
                .commit_status(receipt.transaction, &control)
                .unwrap(),
            uqa_storage::mvcc::CommitStatus::Committed(receipt)
        );
        let snapshot = reopened.snapshot(&control).unwrap();
        for record in records {
            let retained = snapshot.get(record.key(), &control).unwrap().unwrap();
            let bytes = retained.value().unwrap();
            assert_eq!(&***bytes, record.row());
            let (_, values) = decode_record(record.key(), bytes, &control).unwrap();
            assert!(!values.is_empty());
        }
    }
}
