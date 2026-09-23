//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_storage::mvcc::{
    CommitStatus, DatabaseId, GraphRecordKey, IdentifierRequest, PreparedRecordCommit,
    VersionedPersistence, VersionedSessionOptions,
};
use uqa_storage::read_control::StorageReadControl;
use uqa_storage::DocumentStore;

use super::{materialization, persistence::connection};
use crate::{mvcc::schema, Catalog, SQLiteDocumentStore, SQLiteRecordStore};

fn replace_identity(connection: &crate::ManagedConnection, column: &str, identity: DatabaseId) {
    materialization::with(connection, |sqlite| {
        let _permit = schema::WritePermit::acquire(sqlite)?;
        let transaction = schema::begin(sqlite)?;
        let table = if column == "database_id" {
            "_uqa_mvcc_metadata"
        } else {
            "_uqa_mvcc_native_format"
        };
        transaction.execute(
            &format!("UPDATE {table} SET {column} = ?1 WHERE singleton = 1"),
            [identity.as_bytes().as_slice()],
        )?;
        transaction.commit()?;
        Ok(())
    });
}

#[test]
fn native_data_addressing_survives_a_new_history_identity() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(1 << 24);
    for mode in 0..4 {
        let path = directory.path().join(format!("namespace-{mode}.db"));
        let replacement = DatabaseId::from_bytes([173; 16]);
        {
            let connection = connection(&path, mode);
            materialization::initialize(&connection);
            Catalog::open(connection.clone())
                .unwrap()
                .set_metadata("namespace_probe", "before")
                .unwrap();
            let records = SQLiteRecordStore::for_native(&connection, &control).unwrap();
            assert_ne!(records.database_id(), replacement);
            drop(records);
            // Isolate data addressing from the separate restore admission and receipt protocol.
            materialization::with(&connection, |sqlite| {
                let _permit = schema::WritePermit::acquire(sqlite)?;
                let transaction = schema::begin(sqlite)?;
                transaction.execute(
                    "UPDATE _uqa_mvcc_metadata SET database_id = ?1 WHERE singleton = 1",
                    [replacement.as_bytes().as_slice()],
                )?;
                transaction.commit()?;
                Ok(())
            });
        }
        let connection = connection(&path, mode);
        connection
            .bind_native_records(VersionedSessionOptions::default())
            .unwrap();
        let catalog = Catalog::open(connection.clone()).unwrap();
        assert_eq!(
            catalog.get_metadata("namespace_probe").unwrap().as_deref(),
            Some("before"),
            "mode {mode}"
        );
        let documents = SQLiteDocumentStore::new(connection.clone(), "public.docs");
        assert!(documents.get(1).unwrap().is_some());
        assert!(documents.get(2).unwrap().is_some());
        catalog.set_metadata("namespace_probe", "after").unwrap();
        assert_eq!(
            catalog.get_metadata("namespace_probe").unwrap().as_deref(),
            Some("after")
        );
        let records = SQLiteRecordStore::for_native(&connection, &control).unwrap();
        assert_eq!(records.database_id(), replacement);
    }
}

#[test]
fn native_graph_layout_retains_its_namespace_across_history_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(1 << 24);
    let addresses = [
        GraphRecordKey::Entity(uqa_storage::GraphEntityKind::Vertex, 1),
        GraphRecordKey::Entity(uqa_storage::GraphEntityKind::Edge, 1),
        GraphRecordKey::EntityMemberships(uqa_storage::GraphEntityKind::Vertex, 1),
        GraphRecordKey::GraphMemberships("g"),
        GraphRecordKey::GraphPaths("g"),
        GraphRecordKey::GraphName("g"),
        GraphRecordKey::LabelRegistry("g"),
        GraphRecordKey::PathDefinition("p"),
        GraphRecordKey::PathValidity("p"),
    ];
    for mode in 0..4 {
        let path = directory.path().join(format!("graph-namespace-{mode}.db"));
        let replacement = DatabaseId::from_bytes([174; 16]);
        let (namespace, keys) = {
            let connection = connection(&path, mode);
            let catalog = Catalog::open(connection.clone()).unwrap();
            catalog.save_vertex(1, "original", "{}").unwrap();
            let records = SQLiteRecordStore::for_native(&connection, &control).unwrap();
            let namespace = records.native_namespace().unwrap();
            let layout = records.graph_record_layout().unwrap();
            let keys: Vec<_> = addresses
                .iter()
                .map(|&address| {
                    layout
                        .key(records.database_id(), address, &control)
                        .unwrap()
                })
                .collect();
            drop(records);
            replace_identity(&connection, "database_id", replacement);
            (namespace, keys)
        };
        let connection = connection(&path, mode);
        // Initial provider restoration must use the same namespace as direct binding.
        connection.begin_native_initial_restore().unwrap();
        let catalog = Catalog::open(connection.clone()).unwrap();
        assert_eq!(catalog.graph_vertex(1).unwrap().unwrap().label, "original");
        catalog.save_vertex(1, "updated", "{}").unwrap();
        connection.commit_transaction().unwrap();
        assert_eq!(catalog.graph_vertex(1).unwrap().unwrap().label, "updated");
        let records = SQLiteRecordStore::for_native(&connection, &control).unwrap();
        assert_eq!(records.native_namespace(), Some(namespace));
        assert_eq!(records.database_id(), replacement);
        for (address, expected) in addresses.iter().zip(&keys) {
            let actual = records
                .graph_record_layout()
                .unwrap()
                .key(replacement, *address, &control)
                .unwrap();
            assert_eq!(&*actual, &**expected);
        }
    }
}

#[test]
fn retained_native_handles_reject_a_changed_data_namespace() {
    let connection = crate::ManagedConnection::open_in_memory().unwrap();
    let control = StorageReadControl::with_limit(1 << 24);
    let records = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let original = records.native_namespace().unwrap();
    let snapshot = records.snapshot(&control).unwrap();
    let pending = records.allocate_transaction(&control).unwrap();
    let prepared = PreparedRecordCommit::new(&[], &control).unwrap();
    replace_identity(
        &connection,
        "record_namespace",
        DatabaseId::from_bytes([175; 16]),
    );
    assert!(records.snapshot(&control).is_err());
    assert!(snapshot.get(b"any", &control).is_err());
    assert!(snapshot.scan(b"", None, 1, &control).is_err());
    assert!(records.allocate_transaction(&control).is_err());
    assert!(records.identifier_watermark(b"n", &control).is_err());
    assert!(records
        .allocate_identifiers(b"n", IdentifierRequest::Observe(1), &control)
        .is_err());
    assert!(records.commit(pending, &prepared, &control).is_err());
    assert!(records.commit_status(pending, &control).is_err());
    assert!(records.abort(pending, &control).is_err());
    assert!(records.reclaim_versions(&control).is_err());
    replace_identity(&connection, "record_namespace", original);
    assert_eq!(
        records.commit_status(pending, &control).unwrap(),
        CommitStatus::Pending
    );
    assert_eq!(records.identifier_watermark(b"n", &control).unwrap(), None);
    assert_eq!(
        records.snapshot(&control).unwrap().sequence(),
        snapshot.sequence()
    );
}

#[test]
fn malformed_native_namespaces_reject_reopen_and_retained_reads() {
    for value in ["X'00'", "'1234567890123456'", "0"] {
        let connection = crate::ManagedConnection::open_in_memory().unwrap();
        let control = StorageReadControl::with_limit(1 << 24);
        let records = SQLiteRecordStore::for_native(&connection, &control).unwrap();
        materialization::with(&connection, |sqlite| {
            let _permit = schema::WritePermit::acquire(sqlite)?;
            sqlite.execute_batch(&format!(
                "PRAGMA ignore_check_constraints = ON; UPDATE _uqa_mvcc_native_format SET record_namespace = {value}; PRAGMA ignore_check_constraints = OFF;"
            ))?;
            Ok(())
        });
        assert!(
            SQLiteRecordStore::for_native(&connection, &control).is_err(),
            "{value}"
        );
        assert!(records.snapshot(&control).is_err(), "{value}");
    }
    let connection = crate::ManagedConnection::open_in_memory().unwrap();
    assert_eq!(
        SQLiteRecordStore::new(&connection)
            .unwrap()
            .native_namespace(),
        None
    );
}
