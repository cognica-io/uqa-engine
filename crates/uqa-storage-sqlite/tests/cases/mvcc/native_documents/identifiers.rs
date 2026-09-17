//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supplied document IDs use the same durable generation namespace as automatic allocation.

use super::super::native_tables::schema;
use super::*;
use uqa_storage::document_store::identifiers::DocumentIdAllocator;
use uqa_storage::{PersistentStorageBackend, TableSchema};
use uqa_storage_sqlite::SQLiteStorageBackend;

fn next(connection: &ManagedConnection, row: &TableSchema) -> u64 {
    let backend = SQLiteStorageBackend::new(connection.clone());
    DocumentIdAllocator::new(
        backend.identifier_allocator(),
        row.object_id,
        row.storage_generation,
    )
    .unwrap()
    .allocate(&mut 1)
    .unwrap()
}

fn bind(connection: &ManagedConnection) {
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
}

#[test]
fn supplied_document_identifiers_are_reserved_before_private_rows_and_survive_reopen() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("supplied-document-identifiers.db");
        let row = schema("docs", 21, 22);
        {
            let connection = open(mode, &path);
            let catalog = Catalog::open(connection.clone()).unwrap();
            bind(&connection);
            catalog.save_table(&row).unwrap();
            let mut documents = SQLiteDocumentStore::new(connection.clone(), "public.docs");
            documents
                .put_stored(
                    50,
                    StoredDocument::with_metadata(fields(50), DocumentMetadata::with_tuple_xmin(7)),
                )
                .unwrap();
            connection.begin_transaction().unwrap();
            documents.put(100, fields(100)).unwrap();
            connection.savepoint("before-larger-id").unwrap();
            documents
                .put_stored(200, StoredDocument::new(fields(200)))
                .unwrap();
            let retained = documents.snapshot().unwrap();
            let other = open(mode, &path);
            bind(&other);
            let mut independent = SQLiteDocumentStore::new(other.clone(), "public.docs");
            assert_eq!(independent.doc_ids().unwrap(), vec![50]);
            assert_eq!(next(&other, &row), 201);
            independent.put(201, fields(201)).unwrap();
            assert!(connection.in_transaction());
            connection
                .rollback_to_savepoint("before-larger-id")
                .unwrap();
            assert!(!documents.contains_doc_id(200).unwrap());
            assert!(documents.contains_doc_id(100).unwrap());
            connection.rollback_transaction().unwrap();
            assert_eq!(documents.doc_ids().unwrap(), vec![50, 201]);
            assert_eq!(retained.doc_ids().unwrap(), vec![50, 100, 200]);
            documents.put(50, fields(51)).unwrap();
            assert_eq!(
                documents.get_metadata(50).unwrap(),
                Some(DocumentMetadata::with_tuple_xmin(7))
            );
            documents.clear().unwrap();
        }
        let reopened = open(mode, &path);
        bind(&reopened);
        assert_eq!(next(&reopened, &row), 202);
        assert!(SQLiteDocumentStore::new(reopened, "public.docs")
            .is_empty()
            .unwrap());
    }
}

#[test]
fn supplied_document_identifiers_follow_renames_and_separate_recreated_generations() {
    let (connection, _) = memory();
    let catalog = Catalog::open(connection.clone()).unwrap();
    let mut row = schema("docs", 31, 32);
    catalog.save_table(&row).unwrap();
    SQLiteDocumentStore::new(connection.clone(), "public.docs")
        .put(500, fields(500))
        .unwrap();
    catalog
        .rename_table_data("public.docs", "public.renamed")
        .unwrap();
    row.relation = uqa_storage::RelationIdentity::new("public", "renamed");
    assert_eq!(next(&connection, &row), 501);
    let mut documents = SQLiteDocumentStore::new(connection.clone(), "public.renamed");
    connection.begin_transaction().unwrap();
    documents.clear().unwrap();
    let mut replacement = row.clone();
    replacement.storage_generation = [33; 16];
    catalog.save_table(&replacement).unwrap();
    documents.put(900, fields(900)).unwrap();
    connection.rollback_transaction().unwrap();
    assert_eq!(next(&connection, &row), 502);
    assert_eq!(next(&connection, &replacement), 901);
    catalog.drop_table_and_data("public.renamed").unwrap();
    let fresh = schema("renamed", 34, 35);
    catalog.save_table(&fresh).unwrap();
    assert_eq!(next(&connection, &fresh), 1);
    documents.put(2, fields(2)).unwrap();
    assert_eq!(next(&connection, &fresh), 3);
}

fn standalone_schema(connection: &ManagedConnection) -> TableSchema {
    let mut row = schema("docs", 0, 0);
    (row.object_id, row.storage_generation) = connection
        .with_physical(|sqlite| {
            sqlite
                .query_row(
                    "SELECT object_id, generation FROM _uqa_mvcc_native_owners WHERE name = 'docs'",
                    [],
                    |row| {
                        let object: Vec<u8> = row.get(0)?;
                        let generation: Vec<u8> = row.get(1)?;
                        Ok((object.try_into().unwrap(), generation.try_into().unwrap()))
                    },
                )
                .map_err(Into::into)
        })
        .unwrap();
    row
}

#[test]
fn standalone_patches_observe_legacy_ids_without_replacing_their_document_payloads() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    let mut documents = SQLiteDocumentStore::new(connection.clone(), "docs");
    documents
        .put_stored(
            700,
            StoredDocument::with_metadata(fields(700), DocumentMetadata::with_tuple_xmin(9)),
        )
        .unwrap();
    bind(&connection);
    let row = standalone_schema(&connection);
    connection.begin_transaction().unwrap();
    documents
        .patch_fields(700, &BTreeMap::from([("n".into(), Value::Int(701))]))
        .unwrap();
    let other = connection.new_session();
    assert_eq!(next(&other, &row), 701);
    connection.rollback_transaction().unwrap();
    assert_eq!(documents.get(700).unwrap(), Some(fields(700)));
    assert_eq!(
        documents.get_metadata(700).unwrap(),
        Some(DocumentMetadata::with_tuple_xmin(9))
    );
    documents.put(702, fields(702)).unwrap();
    documents.delete(702).unwrap();
    assert_eq!(next(&connection, &row), 703);
}

#[test]
fn failed_identifier_persistence_discards_only_the_evaluated_native_document_write() {
    let (connection, mut documents) = memory();
    documents.put(10, fields(10)).unwrap();
    let row = standalone_schema(&connection);
    connection.begin_transaction().unwrap();
    documents.put(20, fields(20)).unwrap();
    let other = connection.new_session();
    other.with_physical(|sqlite| {
        sqlite.execute_batch("CREATE TRIGGER reject_document_identifier AFTER UPDATE ON _uqa_mvcc_identifiers BEGIN SELECT RAISE(ABORT, 'injected document identifier failure'); END")?;
        Ok(())
    }).unwrap();
    let error = documents
        .put_stored(100, StoredDocument::new(fields(100)))
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("injected document identifier failure"),
        "{error}"
    );
    assert_eq!(documents.doc_ids().unwrap(), vec![10, 20]);
    assert_eq!(
        SQLiteDocumentStore::new(other.clone(), "docs")
            .doc_ids()
            .unwrap(),
        vec![10]
    );
    other
        .with_physical(|sqlite| {
            sqlite.execute_batch("DROP TRIGGER reject_document_identifier")?;
            Ok(())
        })
        .unwrap();
    assert_eq!(next(&other, &row), 21);
    connection.commit_transaction().unwrap();
    assert_eq!(documents.doc_ids().unwrap(), vec![10, 20]);
}

#[test]
fn failed_native_record_publication_retains_identifiers_and_retries_without_observing_again() {
    let (connection, mut documents) = memory();
    documents.put(10, fields(10)).unwrap();
    let row = standalone_schema(&connection);
    let other = connection.new_session();
    other.with_physical(|sqlite| {
        sqlite.execute_batch("CREATE TRIGGER reject_document_record BEFORE INSERT ON _uqa_mvcc_versions BEGIN SELECT RAISE(ABORT, 'injected document history failure'); END")?;
        Ok(())
    }).unwrap();
    assert!(documents.put(100, fields(100)).is_err());
    assert_eq!(
        SQLiteDocumentStore::new(other.clone(), "docs")
            .doc_ids()
            .unwrap(),
        vec![10]
    );
    assert_eq!(next(&other, &row), 101);
    assert!(documents.put(999, fields(999)).is_err());
    other.with_physical(|sqlite| {
        sqlite.execute_batch("DROP TRIGGER reject_document_record; CREATE TRIGGER reject_document_identifier AFTER UPDATE ON _uqa_mvcc_identifiers BEGIN SELECT RAISE(ABORT, 'identifier observation was replayed'); END")?;
        Ok(())
    }).unwrap();
    connection.commit_transaction().unwrap();
    assert_eq!(documents.doc_ids().unwrap(), vec![10, 100]);
    other
        .with_physical(|sqlite| {
            sqlite.execute_batch("DROP TRIGGER reject_document_identifier")?;
            Ok(())
        })
        .unwrap();
    assert_eq!(next(&other, &row), 102);
}
