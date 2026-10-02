//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rows at an identity their table never used are staged without reading what they would replace.

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_storage::{
    document_store::identifiers::DocumentIdNamespace, mvcc::VersionedSessionOptions, DocumentStore,
    StoredDocument, ValueIndexKey,
};

use super::*;
use crate::{Catalog, SQLiteBTreeIndexStore, SQLiteDocumentStore};

/// A document with a scalar field and a BLOB, which is stored in a row of its own.
fn fields(n: i64) -> BTreeMap<String, Value> {
    BTreeMap::from([
        ("n".into(), Value::Int(n)),
        ("bytes".into(), Value::Bytes(vec![n as u8; 4])),
    ])
}

fn stored(n: i64) -> StoredDocument {
    StoredDocument::new(fields(n))
}

fn field() -> ValueIndexKey {
    ValueIndexKey::Column("n".into())
}

fn values(n: i64) -> BTreeMap<ValueIndexKey, Value> {
    BTreeMap::from([(field(), Value::Int(n))])
}

/// The record reads of this thread while `work` ran.
fn record_reads(work: impl FnOnce()) -> usize {
    let before = RECORD_READS.with(std::cell::Cell::get);
    work();
    RECORD_READS.with(std::cell::Cell::get) - before
}

/// A native table `docs` with one document and one built index, and the namespace of its document identities.
fn table(
    connection: &ManagedConnection,
) -> (
    SQLiteDocumentStore,
    SQLiteBTreeIndexStore,
    DocumentIdNamespace,
) {
    Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
    let mut documents = SQLiteDocumentStore::new(connection.clone(), "docs");
    let indexes = SQLiteBTreeIndexStore::new(connection.clone());
    documents.put_stored(1, stored(1)).unwrap();
    indexes
        .replace("docs", &field(), &[(1, Value::Int(1))])
        .unwrap();
    let namespace = connection
        .with_physical(|sqlite| {
            sqlite
                .query_row(
                    "SELECT object_id, generation FROM _uqa_mvcc_native_owners WHERE name = 'docs'",
                    [],
                    |row| {
                        let object: Vec<u8> = row.get(0)?;
                        let generation: Vec<u8> = row.get(1)?;
                        Ok(DocumentIdNamespace {
                            object: object.try_into().unwrap(),
                            generation: generation.try_into().unwrap(),
                        })
                    },
                )
                .map_err(Into::into)
        })
        .unwrap();
    (documents, indexes, namespace)
}

fn entries(indexes: &SQLiteBTreeIndexStore) -> Vec<(u64, Value)> {
    let mut entries = indexes.load("docs", &field()).unwrap().unwrap();
    entries.sort_by_key(|(id, _)| *id);
    entries
}

#[test]
fn rows_at_an_unused_identity_are_staged_without_a_record_read() {
    let directory = tempfile::tempdir().unwrap();
    let connection = ManagedConnection::open(&directory.path().join("unused.db")).unwrap();
    let (mut documents, indexes, namespace) = table(&connection);
    connection.begin_transaction().unwrap();
    // The first write of the transaction reads the table's binding and the marker of its index, which the snapshot then keeps.
    documents.put_stored(2, stored(2)).unwrap();
    indexes.apply_write("docs", 2, Some(&values(2))).unwrap();
    // A replacement reads the body, the BLOB rows and the entry it may replace.
    let replaced = record_reads(|| {
        documents.put_stored(3, stored(3)).unwrap();
        indexes.apply_write("docs", 3, Some(&values(3))).unwrap();
    });
    assert!(replaced >= 3, "{replaced}");
    let unused = record_reads(|| {
        documents
            .put_stored_unused(4, stored(4), namespace)
            .unwrap();
        indexes
            .apply_unused_write("docs", 4, &values(4), namespace)
            .unwrap();
    });
    assert_eq!(unused, 0);
    // What another namespace's watermark shows says nothing about the rows of this table.
    let other = DocumentIdNamespace {
        object: namespace.generation,
        generation: namespace.object,
    };
    let foreign = record_reads(|| {
        documents.put_stored_unused(5, stored(5), other).unwrap();
        indexes
            .apply_unused_write("docs", 5, &values(5), other)
            .unwrap();
    });
    assert_eq!(foreign, replaced);
    // A row the transaction itself changed is read from its own changes, whatever the claim.
    documents.delete(4).unwrap();
    documents
        .put_stored_unused(4, stored(40), namespace)
        .unwrap();
    indexes
        .apply_unused_write("docs", 4, &values(40), namespace)
        .unwrap();
    connection.commit_transaction().unwrap();
    for (id, n) in [(1, 1), (2, 2), (3, 3), (4, 40), (5, 5)] {
        assert_eq!(documents.get(id).unwrap(), Some(fields(n)));
    }
    assert_eq!(
        entries(&indexes),
        [(1, 1), (2, 2), (3, 3), (4, 40), (5, 5)].map(|(id, n)| (id, Value::Int(n)))
    );
}

/// Whether `error` reports a write that met a revision it did not expect.
fn is_write_conflict(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(error) = current {
        if matches!(
            error.downcast_ref::<VersionError>(),
            Some(VersionError::WriteConflict { .. })
        ) {
            return true;
        }
        current = error.source();
    }
    false
}

#[test]
fn a_record_at_an_identity_claimed_unused_fails_the_commit() {
    let directory = tempfile::tempdir().unwrap();
    let connection = ManagedConnection::open(&directory.path().join("claimed.db")).unwrap();
    let (mut documents, indexes, namespace) = table(&connection);
    documents.put_stored(7, stored(7)).unwrap();
    indexes.apply_write("docs", 7, Some(&values(7))).unwrap();
    documents.delete(7).unwrap();
    // The deleted document left a revision at its key. A write that claims the identity unused expects none, so it conflicts instead of replacing what it did not read. A live document is met the same way.
    for (id, kept) in [(7, None), (1, Some(fields(1)))] {
        connection.begin_transaction().unwrap();
        documents
            .put_stored_unused(id, stored(70), namespace)
            .unwrap();
        let error = connection.commit_transaction().unwrap_err();
        assert!(is_write_conflict(&error), "{error}");
        connection.rollback_transaction().unwrap();
        assert_eq!(documents.get(id).unwrap(), kept);
    }
    assert_eq!(entries(&indexes), [(1, Value::Int(1))]);
}
