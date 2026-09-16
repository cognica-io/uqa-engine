//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public native document APIs use the connection's common logical transaction and retained snapshots.

use super::{open, MODES};
use std::{collections::BTreeMap, sync::mpsc, time::Duration};
use uqa_core::Value;
use uqa_storage::{mvcc::VersionedSessionOptions, DocumentMetadata, DocumentStore, StoredDocument};
use uqa_storage_sqlite::{
    Catalog, ManagedConnection, SQLiteDocumentStore, SQLiteError, SQLiteKeyValueStore,
};

fn fields(n: i64) -> BTreeMap<String, Value> {
    BTreeMap::from([
        ("n".into(), Value::Int(n)),
        ("blob".into(), Value::Bytes(n.to_be_bytes().repeat(100))),
    ])
}

fn memory() -> (ManagedConnection, SQLiteDocumentStore) {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let documents = SQLiteDocumentStore::new(connection.clone(), "docs");
    (connection, documents)
}

#[test]
fn independent_native_document_writers_finish_before_the_other_private_transaction() {
    for mode in MODES {
        for ending in ["commit", "rollback", "savepoint"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("documents.db");
            let connection = open(mode, &path);
            Catalog::open(connection.clone()).unwrap();
            // A handle created before binding must join the same logical session too.
            let mut a = SQLiteDocumentStore::new(connection.clone(), "docs");
            a.put_stored(
                1,
                StoredDocument::with_metadata(fields(1), DocumentMetadata::with_tuple_xmin(7)),
            )
            .unwrap();
            a.put(2, fields(2)).unwrap();
            connection
                .bind_native_records(VersionedSessionOptions::default())
                .unwrap();
            connection.begin_transaction().unwrap();
            a.put_stored(
                1,
                StoredDocument::with_metadata(fields(10), DocumentMetadata::with_tuple_xmin(42)),
            )
            .unwrap();
            connection.savepoint("keep").unwrap();
            a.patch_fields(1, &fields(11)).unwrap();
            let private = a.snapshot().unwrap();
            let other_path = path.clone();
            let (sent, received) = mpsc::channel();
            let writer = std::thread::spawn(move || {
                let connection = open(mode, &other_path);
                connection
                    .bind_native_records(VersionedSessionOptions::default())
                    .unwrap();
                let mut b = SQLiteDocumentStore::new(connection.clone(), "docs");
                connection.begin_transaction().unwrap();
                b.put(2, fields(20)).unwrap();
                connection.commit_transaction().unwrap();
                sent.send(()).unwrap();
            });
            let completed = received.recv_timeout(Duration::from_secs(20));
            if completed.is_err() {
                connection.rollback_transaction().unwrap();
                writer.join().unwrap();
                panic!("native document writer did not finish: {mode:?}, {completed:?}");
            }
            writer.join().unwrap();
            assert!(connection.in_transaction());
            assert_eq!(a.get(2).unwrap(), Some(fields(2)));
            let expected = match ending {
                "commit" => {
                    connection.commit_transaction().unwrap();
                    11
                }
                "rollback" => {
                    connection.rollback_transaction().unwrap();
                    1
                }
                _ => {
                    connection.rollback_to_savepoint("keep").unwrap();
                    connection.commit_transaction().unwrap();
                    10
                }
            };
            assert_eq!(a.get(1).unwrap(), Some(fields(expected)));
            assert_eq!(a.get(2).unwrap(), Some(fields(20)));
            assert_eq!(private.get(1).unwrap(), Some(fields(11)));
            assert_eq!(private.get(2).unwrap(), Some(fields(2)));
            assert_eq!(
                private.get_metadata(1).unwrap().unwrap().tuple_xmin(),
                Some(42)
            );
            drop(private);
            drop(a);
            drop(connection);
            let reopened = open(mode, &path);
            reopened
                .bind_native_records(VersionedSessionOptions::default())
                .unwrap();
            let documents = SQLiteDocumentStore::new(reopened, "docs");
            assert_eq!(documents.get(1).unwrap(), Some(fields(expected)));
            assert_eq!(documents.get(2).unwrap(), Some(fields(20)));
            assert_eq!(
                documents.get_metadata(1).unwrap().unwrap().tuple_xmin(),
                Some(if ending == "rollback" { 7 } else { 42 })
            );
        }
    }
}

#[test]
fn native_reads_projections_cursors_and_retained_views_share_typed_document_semantics() {
    let (connection, mut documents) = memory();
    let mut typed = fields(3);
    typed.insert(
        "list".into(),
        Value::List((0..40).map(|n| Value::Float(f64::from(n))).collect()),
    );
    typed.insert("json".into(), Value::Json("{\"name\":\"typed\"}".into()));
    typed.insert("x.y\"z".into(), Value::Str("exact field".into()));
    typed.insert("absent".into(), Value::Null);
    documents
        .put_stored(
            0,
            StoredDocument::with_metadata(
                typed.clone(),
                DocumentMetadata::with_tuple_xmin(u32::MAX),
            ),
        )
        .unwrap();
    typed.remove("absent");
    documents.put(i64::MAX as u64, fields(8)).unwrap();
    assert_eq!(documents.get(0).unwrap(), Some(typed.clone()));
    assert!(documents.put(u64::MAX, fields(4)).is_err());
    assert!(!connection.in_transaction());
    assert_eq!(documents.len().unwrap(), 2);
    assert_eq!(documents.doc_ids().unwrap(), vec![0, i64::MAX as u64]);
    assert_eq!(documents.next_doc_ids(None, 1).unwrap(), vec![0]);
    assert_eq!(
        documents.next_doc_id(Some(0)).unwrap(),
        Some(i64::MAX as u64)
    );
    assert!(documents.next_doc_ids(None, 0).unwrap().is_empty());
    assert_eq!(documents.max_doc_id().unwrap(), i64::MAX as u64);
    assert!(documents.contains_doc_id(0).unwrap());
    assert!(!documents.contains_doc_id(1).unwrap());
    assert_eq!(
        documents.get_field(0, "x.y\"z").unwrap(),
        typed.get("x.y\"z").cloned()
    );
    assert_eq!(
        documents.get_fields_bulk(&[0, 1, 0], "n").unwrap(),
        BTreeMap::from([(0, Value::Int(3)), (1, Value::Null)])
    );
    assert_eq!(
        documents
            .get_fields_multi(&[0, 1], &["n", "blob", "missing", "n"])
            .unwrap(),
        BTreeMap::from([(
            0,
            vec![
                Value::Int(3),
                typed["blob"].clone(),
                Value::Null,
                Value::Int(3)
            ]
        )])
    );
    assert_eq!(documents.get_stored_many(&[0, 1]).unwrap().len(), 1);
    assert_eq!(
        documents.get_many(&[0, 1]).unwrap(),
        BTreeMap::from([(0, typed.clone())])
    );
    assert_eq!(
        documents.find_doc_id_by_field("n", &Value::Int(3)).unwrap(),
        Some(0)
    );
    assert_eq!(
        documents
            .find_doc_id_by_fields(
                &["n".into(), "missing".into()],
                &[Value::Int(3), Value::Null]
            )
            .unwrap(),
        Some(0)
    );
    assert!(documents.has_value("blob", &typed["blob"]).unwrap());
    let mut snapshot = documents.snapshot().unwrap();
    documents.put(0, fields(4)).unwrap();
    assert_eq!(
        documents.get_metadata(0).unwrap().unwrap().tuple_xmin(),
        Some(u32::MAX)
    );
    documents.delete(i64::MAX as u64).unwrap();
    assert_eq!(snapshot.get(0).unwrap(), Some(typed));
    assert_eq!(snapshot.doc_ids().unwrap(), vec![0, i64::MAX as u64]);
    assert_eq!(snapshot.iter_all().unwrap().count(), 2);
    assert!(std::sync::Arc::get_mut(&mut snapshot)
        .unwrap()
        .clear()
        .is_err());
    documents.clear().unwrap();
    assert!(documents.is_empty().unwrap());
    assert_eq!(documents.max_doc_id().unwrap(), 0);
    drop(connection);
    drop(documents);
    assert_eq!(snapshot.len().unwrap(), 2);
}

#[test]
fn independent_sessions_reject_lost_updates_and_preserve_batch_atomicity() {
    let (connection, mut a) = memory();
    a.put(1, fields(1)).unwrap();
    let other = connection.new_session();
    let mut b = SQLiteDocumentStore::new(other.clone(), "docs");
    assert_ne!(
        connection.transaction_affinity(),
        other.transaction_affinity()
    );
    connection.begin_transaction().unwrap();
    other.begin_transaction().unwrap();
    a.put(1, fields(10)).unwrap();
    b.patch_fields(1, &fields(20)).unwrap();
    other.commit_transaction().unwrap();
    assert!(connection.commit_transaction().is_err());
    connection.rollback_transaction().unwrap();
    assert_eq!(a.get(1).unwrap(), Some(fields(20)));

    let bounded = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(bounded.clone()).unwrap();
    bounded
        .bind_native_records(VersionedSessionOptions {
            retained_bytes: 256 * 1024,
        })
        .unwrap();
    let mut documents = SQLiteDocumentStore::new(bounded.clone(), "bounded");
    documents.put(1, fields(1)).unwrap();
    bounded.begin_transaction().unwrap();
    documents.put(2, fields(2)).unwrap();
    let oversized = BTreeMap::from([("blob".into(), Value::Bytes(vec![1; 1024 * 1024]))]);
    assert!(documents.put(1, oversized.clone()).is_err());
    assert_eq!(documents.get(1).unwrap(), Some(fields(1)));
    assert_eq!(documents.get(2).unwrap(), Some(fields(2)));
    bounded.commit_transaction().unwrap();
    assert!(documents.put(1, oversized).is_err());
    assert!(!bounded.in_transaction());
    documents
        .patch_fields(1, &BTreeMap::from([("blob".into(), Value::Null)]))
        .unwrap();
    assert_eq!(documents.get_field(1, "blob").unwrap(), None);
}

#[test]
fn native_binding_rejects_other_formats_active_transactions_and_changed_options() {
    let (connection, _) = memory();
    assert!(matches!(
        SQLiteKeyValueStore::new(connection.clone()),
        Err(SQLiteError::SessionMappingMismatch)
    ));
    assert!(matches!(
        connection.with(|_| Ok(())),
        Err(SQLiteError::LogicalSessionRequired)
    ));
    assert!(matches!(
        connection.bind_native_records(VersionedSessionOptions { retained_bytes: 1 }),
        Err(SQLiteError::SessionOptionsMismatch)
    ));
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let key_value = ManagedConnection::open_in_memory().unwrap();
    SQLiteKeyValueStore::new(key_value.clone()).unwrap();
    assert!(matches!(
        key_value.bind_native_records(VersionedSessionOptions::default()),
        Err(SQLiteError::SessionMappingMismatch)
    ));
    let unbound = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(unbound.clone()).unwrap();
    unbound.begin_transaction().unwrap();
    assert!(matches!(
        unbound.bind_native_records(VersionedSessionOptions::default()),
        Err(SQLiteError::TransactionAlreadyActive)
    ));
    unbound.rollback_transaction().unwrap();
    unbound
        .with(|sqlite| {
            assert_eq!(
                sqlite.query_row(
                    "SELECT value FROM _metadata WHERE key='schema_version'",
                    [],
                    |row| row.get::<_, String>(0)
                )?,
                "48"
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn native_projection_does_not_read_or_rewrite_unrequested_corrupt_blobs() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    let mut documents = SQLiteDocumentStore::new(connection.clone(), "docs");
    documents.put(1, fields(1)).unwrap();
    connection
        .with(|sqlite| {
            sqlite.execute("DELETE FROM _document_blobs", [])?;
            Ok(())
        })
        .unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    assert!(documents.get(1).is_err());
    assert!(documents.get_field(1, "blob").is_err());
    assert_eq!(documents.get_field(1, "n").unwrap(), Some(Value::Int(1)));
    assert_eq!(
        documents.get_fields_multi(&[1], &["n"]).unwrap()[&1],
        vec![Value::Int(1)]
    );
    documents
        .patch_fields(1, &BTreeMap::from([("n".into(), Value::Int(2))]))
        .unwrap();
    assert_eq!(documents.get_field(1, "n").unwrap(), Some(Value::Int(2)));
    assert!(documents.get_field(1, "blob").is_err());
    documents.put(1, fields(3)).unwrap();
    assert_eq!(documents.get(1).unwrap(), Some(fields(3)));
}

#[test]
fn native_document_deletion_versions_both_btree_namespaces_and_clear_retains_old_views() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    let mut documents = SQLiteDocumentStore::new(connection.clone(), "docs");
    documents.put(1, fields(1)).unwrap();
    documents.put(2, fields(2)).unwrap();
    connection.with(|sqlite| {
        for name in [rusqlite::types::Value::Text("n".into()), rusqlite::types::Value::Blob(b"n".to_vec())] {
            sqlite.execute("INSERT INTO _btree_indexes VALUES ('docs', ?1)", [&name])?;
            sqlite.execute("INSERT INTO _btree_index_entries VALUES ('docs', ?1, 1, '1'), ('docs', ?1, 2, '2')", [&name])?;
        }
        Ok(())
    }).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let old = documents.snapshot().unwrap();
    connection.begin_transaction().unwrap();
    documents.delete(1).unwrap();
    connection.savepoint("deleted").unwrap();
    documents.clear().unwrap();
    assert!(documents.is_empty().unwrap());
    connection.rollback_to_savepoint("deleted").unwrap();
    assert_eq!(documents.doc_ids().unwrap(), vec![2]);
    connection.commit_transaction().unwrap();
    connection
        .with_physical(|sqlite| {
            assert_eq!(
                sqlite.query_row(
                    "SELECT count(*) FROM _btree_index_entries WHERE doc_id=1",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                0
            );
            assert_eq!(
                sqlite.query_row(
                    "SELECT count(*) FROM _btree_index_entries WHERE doc_id=2",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                2
            );
            Ok(())
        })
        .unwrap();
    documents.clear().unwrap();
    connection
        .with_physical(|sqlite| {
            assert_eq!(
                sqlite.query_row("SELECT count(*) FROM _btree_index_entries", [], |row| row
                    .get::<_, i64>(
                    0
                ))?,
                0
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(old.get(1).unwrap(), Some(fields(1)));
    assert_eq!(old.get(2).unwrap(), Some(fields(2)));
}

#[test]
fn native_presence_callbacks_and_empty_snapshots_do_not_advance_during_mutations() {
    let (connection, mut documents) = memory();
    let empty = documents.snapshot().unwrap();
    documents.put(1, fields(1)).unwrap();
    documents.put(2, fields(2)).unwrap();
    assert!(empty.is_empty().unwrap());
    let mut other = SQLiteDocumentStore::new(connection.new_session(), "docs");
    let mut observed = Vec::new();
    documents
        .for_each_fields_multi_ref_with_presence(&[1, 2], &[], &mut |id, present, _| {
            observed.push((id, present));
            if id == 1 {
                other.delete(2).unwrap();
            }
            true
        })
        .unwrap();
    assert_eq!(observed, vec![(1, true), (2, true)]);
    assert!(!documents.contains_doc_id(2).unwrap());
}

#[test]
fn failed_native_autocommit_preserves_the_evaluated_batch_for_retry() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let connection = open(mode, &directory.path().join("failure.db"));
        Catalog::open(connection.clone()).unwrap();
        connection
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        let mut documents = SQLiteDocumentStore::new(connection.clone(), "docs");
        documents.put(1, fields(1)).unwrap();
        let other = connection.new_session();
        let observer = SQLiteDocumentStore::new(other.clone(), "docs");
        other.with_physical(|sqlite| {
            sqlite.execute_batch("CREATE TRIGGER injected_native_commit_failure BEFORE INSERT ON _uqa_mvcc_versions BEGIN SELECT RAISE(ABORT, 'injected history failure'); END")?;
            Ok(())
        }).unwrap();
        assert!(documents.put(1, fields(2)).is_err());
        assert!(connection.in_transaction());
        assert_eq!(observer.get(1).unwrap(), Some(fields(1)));
        assert!(documents.put(1, fields(3)).is_err());
        other.with_physical(|sqlite| {
            assert_eq!(sqlite.query_row("SELECT (SELECT count(*) FROM _uqa_mvcc_native_changes) + (SELECT count(*) FROM _uqa_mvcc_native_expected)", [], |row| row.get::<_, i64>(0))?, 0);
            sqlite.execute_batch("DROP TRIGGER injected_native_commit_failure")?;
            Ok(())
        }).unwrap();
        connection.commit_transaction().unwrap();
        assert_eq!(observer.get(1).unwrap(), Some(fields(2)));
        assert!(!connection.in_transaction());
    }
}
