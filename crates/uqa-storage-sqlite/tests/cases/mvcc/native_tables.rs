//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table/catalog-index lifecycle shares document transactions and retains prior generations.

use super::{open, MODES};
use std::{collections::BTreeMap, sync::mpsc, time::Duration};
use uqa_core::Value;
use uqa_storage::{
    mvcc::VersionedSessionOptions, CatalogIndexRow, DocumentStore, RelationIdentity, TableSchema,
    ValueIndexKey, VectorFieldSchema,
};
use uqa_storage_sqlite::{Catalog, ManagedConnection, SQLiteBTreeIndexStore, SQLiteDocumentStore};

#[path = "native_tables/families.rs"]
pub(super) mod families;
#[path = "native_tables/rename.rs"]
mod rename;
#[path = "native_tables/security.rs"]
mod security;

fn bind(connection: &ManagedConnection) {
    connection
        .bind_native_records(VersionedSessionOptions::default())
        .unwrap();
}

pub(super) fn schema(name: &str, identity: u8, generation: u8) -> TableSchema {
    TableSchema {
        relation: RelationIdentity::new("public", name),
        role_owner: "owner".into(),
        acl: None,
        column_acls: BTreeMap::new(),
        object_id: [identity; 16],
        storage_generation: [generation; 16],
        analyzer_json: "{}".into(),
        fts_fields: vec!["n".into()],
        vector_fields: vec![VectorFieldSchema {
            field: "embedding".into(),
            dimensions: 3,
        }],
        columns_json: "[]".into(),
        constraints_json: "{}".into(),
    }
}

fn fields(n: i64) -> BTreeMap<String, Value> {
    BTreeMap::from([
        ("n".into(), Value::Int(n)),
        ("bytes".into(), Value::Bytes(vec![0, 1, 255])),
    ])
}

pub(super) fn index(name: &str, table: &str) -> CatalogIndexRow {
    CatalogIndexRow {
        relation: RelationIdentity::new("public", name),
        index_type: "btree".into(),
        table_name: table.into(),
        columns_json: "[\"n\"]".into(),
        parameters_json: "{}".into(),
        definition_json: Some("{\"expression\":\"n + 1\"}".into()),
    }
}

fn write(connection: &ManagedConnection, catalog: &Catalog, name: &str, id: u8, n: i64) {
    let mut row = schema(name, id, id);
    row.role_owner = format!("owner_{n}");
    let table = row.relation.qualified_name();
    catalog.save_table(&row).unwrap();
    catalog
        .save_catalog_index_row(&index(&format!("i_{name}"), &table))
        .unwrap();
    SQLiteDocumentStore::new(connection.clone(), &table)
        .put(1, fields(n))
        .unwrap();
    SQLiteBTreeIndexStore::new(connection.clone())
        .replace(
            &table,
            &ValueIndexKey::Column("n".into()),
            &[(1, Value::Int(n))],
        )
        .unwrap();
    catalog
        .replace_table_field_analyzer_binding(&table, "n", "both", "standard", "binding")
        .unwrap();
}

fn assert_value(
    connection: &ManagedConnection,
    catalog: &Catalog,
    name: &str,
    expected: Option<i64>,
) {
    let table = RelationIdentity::new("public", name).qualified_name();
    let rows = catalog.load_tables().unwrap();
    let row = rows.iter().find(|row| row.relation.name == name);
    assert_eq!(
        row.map(|row| row.role_owner.clone()),
        expected.map(|n| format!("owner_{n}"))
    );
    assert_eq!(
        SQLiteDocumentStore::new(connection.clone(), &table)
            .get(1)
            .unwrap(),
        expected.map(fields)
    );
    assert_eq!(
        SQLiteBTreeIndexStore::new(connection.clone())
            .load(&table, &ValueIndexKey::Column("n".into()))
            .unwrap(),
        expected.map(|n| vec![(1, Value::Int(n))])
    );
    let indexes = catalog.load_catalog_indexes().unwrap();
    let actual = indexes
        .iter()
        .find(|row| row.relation.name == format!("i_{name}"));
    assert_eq!(
        actual.map(|row| row.table_name.as_str()),
        expected.map(|_| table.as_str())
    );
    if let Some(row) = actual {
        assert_eq!(row.definition_json, index("unused", &table).definition_json);
    }
}

#[test]
fn independent_native_table_creators_commit_documents_and_indexes_before_the_other_finishes() {
    for mode in MODES {
        for ending in ["commit", "rollback", "savepoint"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("tables.db");
            let connection = open(mode, &path);
            let catalog = Catalog::open(connection.clone()).unwrap();
            bind(&connection);
            connection.begin_transaction().unwrap();
            write(&connection, &catalog, "a", 1, 10);
            connection.savepoint("keep").unwrap();
            write(&connection, &catalog, "a", 1, 11);
            let other_path = path.clone();
            let (sent, received) = mpsc::channel();
            let writer = std::thread::spawn(move || {
                let connection = open(mode, &other_path);
                bind(&connection);
                let catalog = Catalog::open(connection.clone()).unwrap();
                connection.begin_transaction().unwrap();
                write(&connection, &catalog, "b", 2, 20);
                connection.commit_transaction().unwrap();
                sent.send(()).unwrap();
            });
            let completed = received.recv_timeout(Duration::from_secs(20));
            if completed.is_err() {
                connection.rollback_transaction().unwrap();
                writer.join().unwrap();
                panic!("native table creator did not finish: {mode:?} {completed:?}");
            }
            writer.join().unwrap();
            assert!(connection.in_transaction());
            assert_value(&connection, &catalog, "a", Some(11));
            assert_value(&connection, &catalog, "b", None);
            let expected = match ending {
                "commit" => {
                    connection.commit_transaction().unwrap();
                    Some(11)
                }
                "rollback" => {
                    connection.rollback_transaction().unwrap();
                    None
                }
                _ => {
                    connection.rollback_to_savepoint("keep").unwrap();
                    connection.commit_transaction().unwrap();
                    Some(10)
                }
            };
            drop(catalog);
            drop(connection);
            let reopened = open(mode, &path);
            bind(&reopened);
            let catalog = Catalog::open(reopened.clone()).unwrap();
            assert_value(&reopened, &catalog, "a", expected);
            assert_value(&reopened, &catalog, "b", Some(20));
        }
    }
}

#[test]
fn native_table_rename_generation_rotation_and_recreation_preserve_retained_views() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("lifecycle.db");
        let connection = open(mode, &path);
        let catalog = Catalog::open(connection.clone()).unwrap();
        bind(&connection);
        connection.begin_transaction().unwrap();
        write(&connection, &catalog, "docs", 7, 1);
        connection.commit_transaction().unwrap();
        let other = connection.new_session();
        let old_catalog = Catalog::open(other.clone()).unwrap();
        other.begin_transaction().unwrap();
        let old = SQLiteDocumentStore::new(other.clone(), "public.docs")
            .snapshot()
            .unwrap();
        connection.begin_transaction().unwrap();
        catalog
            .rename_table_data("public.docs", "public.renamed")
            .unwrap();
        assert_eq!(catalog.load_tables().unwrap()[0].object_id, [7; 16]);
        assert_eq!(
            catalog.load_catalog_indexes().unwrap()[0].table_name,
            "public.renamed"
        );
        assert_eq!(
            catalog.load_table_field_analyzers().unwrap()[0].0,
            "public.renamed"
        );
        let mut changed = catalog.load_tables().unwrap().remove(0);
        changed.storage_generation = [8; 16];
        catalog.save_table(&changed).unwrap();
        connection.commit_transaction().unwrap();
        assert_eq!(
            SQLiteDocumentStore::new(connection.clone(), "public.renamed")
                .get(1)
                .unwrap(),
            Some(fields(1))
        );
        assert_eq!(
            SQLiteBTreeIndexStore::new(connection.clone())
                .load("public.renamed", &ValueIndexKey::Column("n".into()))
                .unwrap(),
            Some(vec![(1, Value::Int(1))])
        );
        assert_eq!(
            catalog.load_catalog_indexes().unwrap()[0].definition_json,
            index("i_docs", "public.renamed").definition_json
        );
        assert_value(&other, &old_catalog, "docs", Some(1));
        catalog.drop_table_and_data("public.renamed").unwrap();
        assert!(catalog.load_tables().unwrap().is_empty());
        assert!(catalog.load_catalog_indexes().unwrap().is_empty());
        assert!(catalog.load_table_field_analyzers().unwrap().is_empty());
        let replacement = schema("renamed", 9, 9);
        catalog.save_table(&replacement).unwrap();
        assert!(
            SQLiteDocumentStore::new(connection.clone(), "public.renamed")
                .get(1)
                .unwrap()
                .is_none()
        );
        assert_eq!(old.get(1).unwrap(), Some(fields(1)));
        other.rollback_transaction().unwrap();
        drop(old_catalog);
        drop(other);
        drop(catalog);
        drop(connection);
        let reopened = open(mode, &path);
        bind(&reopened);
        let catalog = Catalog::open(reopened.clone()).unwrap();
        assert_eq!(
            catalog.load_tables().unwrap()[0].object_id,
            replacement.object_id
        );
        assert_eq!(old.get(1).unwrap(), Some(fields(1)));
    }
}

#[test]
fn native_definition_only_drop_keeps_data_and_purge_preserves_analyzer_configuration() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    bind(&connection);
    write(&connection, &catalog, "docs", 1, 3);
    catalog.drop_table("docs").unwrap();
    assert!(catalog.load_tables().unwrap().is_empty());
    assert!(catalog.load_catalog_indexes().unwrap().is_empty());
    assert_eq!(
        SQLiteDocumentStore::new(connection.clone(), "public.docs")
            .get(1)
            .unwrap(),
        Some(fields(3))
    );
    let zero = schema("docs", 0, 0);
    catalog.save_table(&zero).unwrap();
    let row = catalog.load_tables().unwrap().remove(0);
    assert_eq!(row.object_id, [1; 16]);
    assert_eq!(row.storage_generation, [1; 16]);
    catalog.purge_table_data("docs").unwrap();
    assert_eq!(catalog.load_tables().unwrap().len(), 1);
    assert_eq!(catalog.load_table_field_analyzers().unwrap().len(), 1);
    assert!(SQLiteDocumentStore::new(connection.clone(), "public.docs")
        .get(1)
        .unwrap()
        .is_none());
    assert!(SQLiteBTreeIndexStore::new(connection.clone())
        .fields("public.docs")
        .unwrap()
        .is_empty());
    catalog.save_table(&schema("new_zero", 0, 0)).unwrap();
    let rows = catalog.load_tables().unwrap();
    assert_eq!(rows[0].relation.name, "docs");
    assert_eq!(rows[1].relation.name, "new_zero");
    assert_ne!(rows[1].object_id, [0; 16]);
    assert_ne!(rows[1].storage_generation, [0; 16]);
    assert_ne!(rows[1].object_id, rows[0].object_id);
}

#[test]
fn native_catalog_index_claims_and_failed_table_batches_leave_existing_data_intact() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    connection
        .bind_native_records(VersionedSessionOptions {
            retained_bytes: 256 * 1024,
        })
        .unwrap();
    write(&connection, &catalog, "docs", 1, 1);
    connection.begin_transaction().unwrap();
    assert!(catalog.save_table(&schema("duplicate_id", 1, 2)).is_err());
    assert!(catalog
        .save_catalog_index_row(&index("missing", "public.absent"))
        .is_err());
    assert!(catalog
        .save_catalog_index_row(&index("docs", "public.docs"))
        .is_err());
    assert!(catalog
        .drop_catalog_index(&RelationIdentity::new("public", "docs"))
        .is_err());
    assert!(catalog.rename_table_data("docs", "i_docs").is_err());
    let mut huge = schema("failed", 8, 8);
    huge.analyzer_json = "x".repeat(1024 * 1024);
    assert!(catalog.save_table(&huge).is_err());
    catalog
        .save_catalog_index_row(&index("failed", "public.docs"))
        .unwrap();
    catalog
        .drop_catalog_index(&RelationIdentity::new("public", "failed"))
        .unwrap();
    connection.commit_transaction().unwrap();
    assert_value(&connection, &catalog, "docs", Some(1));
    catalog.drop_catalog_indexes_for_table("docs").unwrap();
    assert!(catalog.load_catalog_indexes().unwrap().is_empty());
    catalog.save_table(&schema("i_docs", 9, 9)).unwrap();
}

#[test]
fn native_generation_rotation_rejects_a_late_new_row_and_old_generation_writers() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    bind(&connection);
    write(&connection, &catalog, "docs", 1, 1);
    let other = connection.new_session();
    let mut docs_b = SQLiteDocumentStore::new(other.clone(), "public.docs");
    connection.begin_transaction().unwrap();
    let mut row = catalog.load_tables().unwrap().remove(0);
    row.storage_generation = [2; 16];
    catalog.save_table(&row).unwrap();
    docs_b.put(2, fields(2)).unwrap();
    assert!(connection.commit_transaction().is_err());
    connection.rollback_transaction().unwrap();
    assert_eq!(
        catalog.load_tables().unwrap()[0].storage_generation,
        [1; 16]
    );
    other.begin_transaction().unwrap();
    docs_b.put(3, fields(3)).unwrap();
    catalog.save_table(&row).unwrap();
    assert!(other.commit_transaction().is_err());
    other.rollback_transaction().unwrap();
    assert_eq!(docs_b.get(2).unwrap(), Some(fields(2)));
    assert!(docs_b.get(3).unwrap().is_none());
    assert_eq!(
        catalog.load_tables().unwrap()[0].storage_generation,
        [2; 16]
    );
}

#[test]
fn native_table_materialization_failure_preserves_definitions_and_data_until_retry() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let connection = open(mode, &directory.path().join("failure.db"));
        let catalog = Catalog::open(connection.clone()).unwrap();
        write(&connection, &catalog, "docs", 1, 1);
        bind(&connection);
        let other = connection.new_session();
        let observer = Catalog::open(other.clone()).unwrap();
        connection.begin_transaction().unwrap();
        catalog
            .rename_table_data("public.docs", "public.renamed")
            .unwrap();
        other.with_physical(|sqlite| {
            sqlite.execute_batch("CREATE TRIGGER injected_table_failure BEFORE INSERT ON _tables BEGIN SELECT RAISE(ABORT, 'injected table failure'); END")?;
            Ok(())
        }).unwrap();
        assert!(connection.commit_transaction().is_err());
        assert_value(&other, &observer, "docs", Some(1));
        other
            .with_physical(|sqlite| {
                sqlite.execute_batch("DROP TRIGGER injected_table_failure")?;
                Ok(())
            })
            .unwrap();
        connection.commit_transaction().unwrap();
        assert_eq!(observer.load_tables().unwrap()[0].relation.name, "renamed");
        assert_eq!(
            SQLiteDocumentStore::new(other.clone(), "public.renamed")
                .get(1)
                .unwrap(),
            Some(fields(1))
        );
        assert_eq!(
            observer.load_catalog_indexes().unwrap()[0].table_name,
            "public.renamed"
        );
    }
}
