//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{Catalog, ManagedConnection, SQLiteRecordStore};

mod changes;
mod diskann;

fn retired_search_layout(connection: &ManagedConnection) {
    Catalog::open(connection.clone()).unwrap();
    connection
        .with(|sqlite| {
            sqlite.execute_batch(
                "DROP TABLE _doc_lengths;
                 DROP TABLE _posting_clusters;
                 DROP TABLE _posting_documents;
                 CREATE TABLE _doc_lengths (
                    table_name TEXT NOT NULL, doc_id INTEGER NOT NULL,
                    lengths TEXT NOT NULL, PRIMARY KEY (table_name, doc_id)
                 );
                 CREATE TABLE _postings (
                    table_name TEXT NOT NULL, field TEXT NOT NULL, term TEXT NOT NULL,
                    doc_id INTEGER NOT NULL, positions TEXT NOT NULL,
                    PRIMARY KEY (table_name, field, term, doc_id)
                 );",
            )?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn native_import_normalizes_retired_empty_search_tables_atomically() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    retired_search_layout(&connection);
    connection.with(|sqlite| {
        sqlite.execute_batch("CREATE TRIGGER reject_retired_baseline BEFORE UPDATE ON _metadata WHEN NEW.key = 'schema_version' AND NEW.value = '49' BEGIN SELECT RAISE(ABORT, 'retired baseline failure'); END;")?;
        Ok(())
    }).unwrap();
    let error = connection
        .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
        .unwrap_err();
    assert!(
        error.to_string().contains("retired baseline failure"),
        "{error}"
    );
    connection
        .with(|sqlite| {
            assert_eq!(
                sqlite.query_row(
                    "SELECT group_concat(name, ',') FROM pragma_table_info('_doc_lengths')",
                    [],
                    |row| row.get::<_, String>(0)
                )?,
                "table_name,doc_id,lengths"
            );
            assert!(sqlite.prepare("SELECT * FROM _postings").is_ok());
            assert!(!present(sqlite).unwrap());
            sqlite.execute_batch("DROP TRIGGER reject_retired_baseline")?;
            Ok(())
        })
        .unwrap();
    connection
        .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
        .unwrap();
    connection
        .with_physical(|sqlite| {
            validate_format(sqlite, CURRENT_VERSION).unwrap();
            assert!(sqlite.prepare("SELECT * FROM _postings").is_err());
            Ok(())
        })
        .unwrap();
}

#[test]
fn native_import_preserves_legacy_search_rows_requiring_source_reconstruction() {
    for table in ["_postings", "_doc_lengths", "_posting_documents"] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        retired_search_layout(&connection);
        connection
            .with(|sqlite| {
                sqlite.execute_batch(match table {
                    "_postings" => "INSERT INTO _postings VALUES ('public.t', 'body', 'term', 1, '[0]')",
                    "_doc_lengths" => "INSERT INTO _doc_lengths VALUES ('public.t', 1, '{\"body\":1}')",
                    _ => "CREATE TABLE _posting_documents (table_name TEXT NOT NULL, doc_id INTEGER NOT NULL, field TEXT NOT NULL, terms_blob BLOB NOT NULL, PRIMARY KEY (table_name, doc_id, field)) WITHOUT ROWID; INSERT INTO _posting_documents VALUES ('public.t', 1, 'body', X'00')",
                })?;
                Ok(())
            })
            .unwrap();
        let error = connection
            .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
            .unwrap_err();
        assert!(
            error.to_string().contains("requires source reconstruction"),
            "{error}"
        );
        connection
            .with(|sqlite| {
                assert_eq!(
                    sqlite.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                        .get::<_, i64>(0))?,
                    1
                );
                assert!(!present(sqlite).unwrap());
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn fresh_native_catalog_and_mapping_bootstrap_atomically() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    // Exhaustion during baseline conversion must also undo the catalog migrations preceding it.
    assert!(
        SQLiteRecordStore::for_native(&connection, &StorageReadControl::with_limit(1)).is_err()
    );
    connection
        .with_physical(|sqlite| {
            assert_eq!(
                sqlite.query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*'",
                    [],
                    |row| row.get::<_, i64>(0)
                )?,
                0
            );
            Ok(())
        })
        .unwrap();
    connection
        .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
        .unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    catalog.set_metadata("bootstrap", "visible").unwrap();
    connection
        .with_physical(|sqlite| {
            check_mapping_version(sqlite, CURRENT_VERSION).unwrap();
            assert_eq!(
                sqlite.query_row(
                    "SELECT value FROM _metadata WHERE key = 'bootstrap'",
                    [],
                    |row| row.get::<_, String>(0)
                )?,
                "visible"
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn native_vector_mapping_rejects_prior_writers_and_rollback_restores_the_old_marker() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(connection.clone()).unwrap();
    let control = StorageReadControl::with_limit(1 << 20);
    SQLiteRecordStore::for_native(&connection, &control).unwrap();
    connection
        .with_physical(|sqlite| {
            let _permit = schema::WritePermit::acquire(sqlite).unwrap();
            let transaction = schema::begin(sqlite).unwrap();
            crate::mvcc::native::tests::standalone_graph::remove_empty_tables(&transaction)
                .unwrap();
            crate::mvcc::native::tests::diskann::remove_empty_table(&transaction).unwrap();
            transaction
                .execute_batch("DROP TABLE _uqa_mvcc_native_format")
                .unwrap();
            transaction.execute_batch(FORMAT_SIX).unwrap();
            transaction
                .execute("INSERT INTO _uqa_mvcc_native_format VALUES (1,6,49)", [])
                .unwrap();
            for action in ["INSERT", "UPDATE", "DELETE"] {
                transaction
                    .execute_batch(&schema::trigger(TABLES[0].0, action).1)
                    .unwrap();
            }
            transaction.commit().unwrap();
            validate_format(sqlite, 6).unwrap();
            let transaction = schema::begin(sqlite).unwrap();
            reopen(&transaction, &control).unwrap();
            check_mapping_version(&transaction, CURRENT_VERSION).unwrap();
            assert!(check_mapping_version(&transaction, 6).is_err());
            // An error after migration must roll back its DDL and recreated guards together.
            drop(transaction);
            validate_format(sqlite, 6).unwrap();
            check_mapping_version(sqlite, 6).unwrap();
            assert!(check_mapping_version(sqlite, CURRENT_VERSION).is_err());
            Ok(())
        })
        .unwrap();
    SQLiteRecordStore::for_native(&connection, &control).unwrap();
    connection
        .with_physical(|sqlite| {
            validate_format(sqlite, CURRENT_VERSION).unwrap();
            assert!(check_mapping_version(sqlite, 6).is_err());
            Ok(())
        })
        .unwrap();
}
