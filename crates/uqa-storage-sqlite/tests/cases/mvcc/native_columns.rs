//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column metadata, field-owned records and statistics participate in document transactions.

use super::{
    native_tables::{index, schema},
    open, MODES,
};
use std::{collections::BTreeMap, sync::mpsc, time::Duration};
use uqa_core::Value;
use uqa_storage::{mvcc::VersionedSessionOptions, ColumnStatsInput, DocumentStore, ValueIndexKey};
use uqa_storage_sqlite::{Catalog, ManagedConnection, SQLiteBTreeIndexStore, SQLiteDocumentStore};

#[path = "native_columns/families.rs"]
mod families;
#[path = "native_columns/stats.rs"]
mod stats;

const TABLE: &str = "public.docs";

fn bind(connection: &ManagedConnection) {
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
}

fn fields(field: &str, n: i64) -> BTreeMap<String, Value> {
    BTreeMap::from([
        (field.into(), Value::Int(n)),
        ("bytes".into(), Value::Bytes(vec![0, 1, 255])),
    ])
}

fn statistic<'a>(table: &'a str, field: &'a str, n: i64) -> ColumnStatsInput<'a> {
    ColumnStatsInput {
        table_name: table,
        column_name: field,
        distinct_count: n,
        null_count: 2,
        min_value: None,
        max_value: Some("10"),
        row_count: n * 10,
        histogram_json: "[1,5,10]",
        mcv_values_json: "[2]",
        mcv_frequencies_json: "[0.25]",
    }
}

fn prepare(connection: &ManagedConnection, catalog: &Catalog) {
    catalog.save_table(&schema("docs", 1, 1)).unwrap();
    SQLiteDocumentStore::new(connection.clone(), TABLE)
        .put(1, fields("n", 1))
        .unwrap();
    SQLiteBTreeIndexStore::new(connection.clone())
        .replace(
            TABLE,
            &ValueIndexKey::Column("n".into()),
            &[(1, Value::Int(1))],
        )
        .unwrap();
    catalog.save_catalog_index_row(&index("i", TABLE)).unwrap();
    catalog
        .replace_table_field_analyzer_binding(TABLE, "n", "both", "standard", "binding")
        .unwrap();
    catalog.save_column_stats(statistic(TABLE, "n", 1)).unwrap();
}

fn rename(connection: &ManagedConnection, catalog: &Catalog) {
    // Match the caller's order: rewrite the document before catalog-owned field storage.
    SQLiteDocumentStore::new(connection.clone(), TABLE)
        .put(1, fields("renamed", 1))
        .unwrap();
    catalog.rename_column_data(TABLE, "n", "renamed").unwrap();
    let mut row = catalog.load_tables().unwrap().remove(0);
    row.fts_fields = vec!["renamed".into()];
    catalog.save_table(&row).unwrap();
}

fn assert_column(connection: &ManagedConnection, catalog: &Catalog, field: &str, count: i64) {
    assert_eq!(
        SQLiteDocumentStore::new(connection.clone(), TABLE)
            .get(1)
            .unwrap(),
        Some(fields(field, 1))
    );
    assert_eq!(
        SQLiteBTreeIndexStore::new(connection.clone())
            .load(TABLE, &ValueIndexKey::Column(field.into()))
            .unwrap(),
        Some(vec![(1, Value::Int(1))])
    );
    let row = catalog
        .load_column_stats(TABLE)
        .unwrap()
        .into_iter()
        .find(|row| row.column_name == field)
        .unwrap();
    assert_eq!(row.distinct_count, count);
    assert_eq!(row.histogram_json, "[1,5,10]");
    assert_eq!(
        catalog.load_catalog_indexes().unwrap()[0].columns_json,
        format!("[\"{field}\"]")
    );
    assert_eq!(catalog.load_table_field_analyzers().unwrap()[0].1, field);
}

#[test]
fn independent_native_column_and_statistics_writers_finish_before_the_other_transaction_ends() {
    for mode in MODES {
        for ending in ["commit", "rollback", "savepoint"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("columns.db");
            let connection = open(mode, &path);
            let catalog = Catalog::open(connection.clone()).unwrap();
            bind(&connection);
            prepare(&connection, &catalog);
            connection.begin_transaction().unwrap();
            rename(&connection, &catalog);
            connection.savepoint("renamed").unwrap();
            catalog
                .save_column_stats(statistic(TABLE, "renamed", 9))
                .unwrap();
            let other_path = path.clone();
            let (sent, received) = mpsc::channel();
            let writer = std::thread::spawn(move || {
                let connection = open(mode, &other_path);
                bind(&connection);
                let catalog = Catalog::open(connection.clone()).unwrap();
                connection.begin_transaction().unwrap();
                SQLiteDocumentStore::new(connection.clone(), TABLE)
                    .put(2, fields("other", 2))
                    .unwrap();
                SQLiteBTreeIndexStore::new(connection.clone())
                    .replace(
                        TABLE,
                        &ValueIndexKey::Column("other".into()),
                        &[(2, Value::Int(2))],
                    )
                    .unwrap();
                catalog
                    .save_column_stats(statistic(TABLE, "other", 2))
                    .unwrap();
                connection.commit_transaction().unwrap();
                sent.send(()).unwrap();
            });
            let completed = received.recv_timeout(Duration::from_secs(20));
            if completed.is_err() {
                connection.rollback_transaction().unwrap();
                writer.join().unwrap();
                panic!("native column writer did not finish: {mode:?} {completed:?}");
            }
            writer.join().unwrap();
            assert!(connection.in_transaction());
            assert_column(&connection, &catalog, "renamed", 9);
            assert!(catalog
                .load_column_stats(TABLE)
                .unwrap()
                .iter()
                .all(|row| row.column_name != "other"));
            let (field, count) = match ending {
                "commit" => {
                    connection.commit_transaction().unwrap();
                    ("renamed", 9)
                }
                "rollback" => {
                    connection.rollback_transaction().unwrap();
                    ("n", 1)
                }
                _ => {
                    connection.rollback_to_savepoint("renamed").unwrap();
                    connection.commit_transaction().unwrap();
                    ("renamed", 1)
                }
            };
            drop(catalog);
            drop(connection);
            let reopened = open(mode, &path);
            bind(&reopened);
            let catalog = Catalog::open(reopened.clone()).unwrap();
            assert_column(&reopened, &catalog, field, count);
            assert_eq!(
                SQLiteDocumentStore::new(reopened.clone(), TABLE)
                    .get(2)
                    .unwrap(),
                Some(fields("other", 2))
            );
            assert_eq!(
                SQLiteBTreeIndexStore::new(reopened)
                    .load(TABLE, &ValueIndexKey::Column("other".into()))
                    .unwrap(),
                Some(vec![(2, Value::Int(2))])
            );
            assert_eq!(
                catalog
                    .load_column_stats(TABLE)
                    .unwrap()
                    .iter()
                    .find(|row| row.column_name == "other")
                    .unwrap()
                    .distinct_count,
                2
            );
        }
    }
}

#[test]
fn native_column_publication_failure_keeps_all_old_state_until_exact_retry() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let connection = open(mode, &directory.path().join("column_failure.db"));
        let catalog = Catalog::open(connection.clone()).unwrap();
        bind(&connection);
        prepare(&connection, &catalog);
        let other = connection.new_session();
        let observer = Catalog::open(other.clone()).unwrap();
        connection.begin_transaction().unwrap();
        rename(&connection, &catalog);
        other.with_physical(|sql| {
            sql.execute_batch("CREATE TRIGGER injected_column_failure BEFORE INSERT ON _column_stats BEGIN SELECT RAISE(ABORT, 'column failure'); END")?;
            Ok(())
        }).unwrap();
        assert!(connection.commit_transaction().is_err());
        assert_column(&other, &observer, "n", 1);
        other
            .with_physical(|sql| {
                sql.execute_batch("DROP TRIGGER injected_column_failure")?;
                Ok(())
            })
            .unwrap();
        connection.commit_transaction().unwrap();
        assert_column(&other, &observer, "renamed", 1);
    }
}

#[test]
fn malformed_catalog_index_columns_cannot_partially_rename_or_delete_native_fields() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    bind(&connection);
    prepare(&connection, &catalog);
    let mut row = index("broken", TABLE);
    row.columns_json = "not-json".into();
    catalog.save_catalog_index_row(&row).unwrap();
    connection.begin_transaction().unwrap();
    assert!(catalog.rename_column_data(TABLE, "n", "renamed").is_err());
    assert!(catalog.drop_column_data(TABLE, "n").is_err());
    assert_eq!(
        catalog.load_column_stats(TABLE).unwrap()[0].column_name,
        "n"
    );
    assert_eq!(catalog.load_table_field_analyzers().unwrap()[0].1, "n");
    assert_eq!(
        SQLiteBTreeIndexStore::new(connection.clone())
            .load(TABLE, &ValueIndexKey::Column("n".into()))
            .unwrap(),
        Some(vec![(1, Value::Int(1))])
    );
    catalog
        .save_column_stats(statistic(TABLE, "another", 2))
        .unwrap();
    connection.commit_transaction().unwrap();
    assert_eq!(catalog.load_column_stats(TABLE).unwrap().len(), 2);
}
