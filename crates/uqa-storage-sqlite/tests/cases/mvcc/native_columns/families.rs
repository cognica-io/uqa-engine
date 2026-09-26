//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column lifecycle preserves physical payloads, expression namespaces and unrelated large BLOBs.

use super::*;
use crate::mvcc::native_tables::families::{
    assert_ivf_guard_history, families, rows, seed_missing_families, seed_native_families,
};
use rusqlite::types::Value as SQLValue;
use uqa_storage::{mvcc::VersionedPersistence, read_control::StorageReadControl};
use uqa_storage_sqlite::{
    mvcc::native::{NativeRecord, NativeRecordFamily as Family, NativeRecordOwner},
    SQLiteRecordStore,
};

#[test]
fn native_column_rename_and_drop_preserve_every_fixed_field_family_and_old_history() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    catalog.save_table(&schema("docs", 1, 1)).unwrap();
    let bytes = Value::Bytes(vec![0, 1, 255]);
    SQLiteDocumentStore::new(connection.clone(), TABLE)
        .put(1, BTreeMap::from([("n".into(), bytes.clone())]))
        .unwrap();
    SQLiteBTreeIndexStore::new(connection.clone())
        .replace(TABLE, &ValueIndexKey::Column("n".into()), &[(1, bytes)])
        .unwrap();
    catalog.save_catalog_index_row(&index("i", TABLE)).unwrap();
    seed_missing_families(&connection);
    let control = StorageReadControl::with_limit(1 << 24);
    let records = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    seed_native_families(&records, &control);
    let original: Vec<_> = families()
        .filter_map(|family| {
            let column = family
                .layout()
                .columns
                .iter()
                .position(|name| matches!(*name, "field" | "field_name" | "column_name"))?;
            Some((family, column, rows(&connection, family, TABLE)))
        })
        .collect();
    assert_eq!(original.len(), 25);
    assert!(original.iter().all(|(_, _, rows)| !rows.is_empty()));
    let old = records.snapshot(&control).unwrap();
    bind(&connection);
    catalog.rename_column_data(TABLE, "n", "renamed").unwrap();
    let renamed = records.snapshot(&control).unwrap();
    assert_ivf_guard_history(&*old, &*renamed, &control);
    for (family, column, before) in &original {
        let mut expected = before.clone();
        if matches!(family, Family::OccurrenceSkips | Family::OccurrenceBlockMax) {
            expected.clear();
        } else {
            for row in &mut expected {
                row[*column] = SQLValue::Text("renamed".into());
            }
        }
        assert_eq!(
            rows(&connection, *family, TABLE),
            expected,
            "{}",
            family.layout().table
        );
        for row in before {
            let encoded = NativeRecord::encode(
                *family,
                NativeRecordOwner::Object {
                    identity: [1; 16],
                    generation: [1; 16],
                },
                &row.iter().map(Into::into).collect::<Vec<_>>(),
                &control,
            )
            .unwrap();
            assert_eq!(
                &***old
                    .get(encoded.key(), &control)
                    .unwrap()
                    .unwrap()
                    .value()
                    .unwrap(),
                encoded.row()
            );
            assert!(renamed
                .get(encoded.key(), &control)
                .unwrap()
                .unwrap()
                .value()
                .is_none());
        }
    }
    assert_eq!(
        catalog.load_catalog_indexes().unwrap()[0].columns_json,
        "[\"renamed\"]"
    );
    catalog.drop_column_data(TABLE, "renamed").unwrap();
    assert_ivf_guard_history(&*old, &*records.snapshot(&control).unwrap(), &control);
    for (family, _, _) in original {
        assert!(
            rows(&connection, family, TABLE).is_empty(),
            "{}",
            family.layout().table
        );
    }
    assert_eq!(rows(&connection, Family::Documents, TABLE).len(), 1);
    assert_eq!(rows(&connection, Family::OccurrenceFormats, TABLE).len(), 1);
    assert!(catalog.load_catalog_indexes().unwrap().is_empty());
    catalog.save_table(&schema("i", 2, 2)).unwrap();
}

const EXPRESSION_KEYS: &str = "[{\"expression\":{\"ColumnRef\":\"n\"}}]";

fn prepare_column_namespaces(
    connection: &ManagedConnection,
    catalog: &Catalog,
    existing: bool,
) -> SQLiteBTreeIndexStore {
    prepare(connection, catalog);
    SQLiteDocumentStore::new(connection.clone(), TABLE)
        .put(2, fields("renamed", 2))
        .unwrap();
    let btree = SQLiteBTreeIndexStore::new(connection.clone());
    btree
        .replace(
            TABLE,
            &ValueIndexKey::Index("n".into()),
            &[(1, Value::Int(9))],
        )
        .unwrap();
    if existing {
        catalog
            .save_column_stats(statistic(TABLE, "renamed", 9))
            .unwrap();
        btree
            .replace(
                TABLE,
                &ValueIndexKey::Column("renamed".into()),
                &[(2, Value::Int(2))],
            )
            .unwrap();
    }
    connection
        .with(|sql| {
            sql.execute(
                "INSERT INTO _btree_index_repairs (table_name, field) VALUES (?1, 'n')",
                [TABLE],
            )?;
            Ok(())
        })
        .unwrap();
    let mut expression = index("expression", TABLE);
    expression.columns_json = EXPRESSION_KEYS.into();
    catalog.save_catalog_index_row(&expression).unwrap();
    btree
}

#[test]
fn column_rename_keeps_existing_btree_targets_and_expression_namespaces_and_moves_repairs() {
    for (native, foreign_keys) in [(false, false), (false, true), (true, true)] {
        for existing in [false, true] {
            let connection = ManagedConnection::open_in_memory().unwrap();
            let catalog = Catalog::open(connection.clone()).unwrap();
            let btree = prepare_column_namespaces(&connection, &catalog, existing);
            if native {
                bind(&connection);
            } else {
                connection
                    .with(|sql| {
                        sql.pragma_update(None, "foreign_keys", foreign_keys)?;
                        Ok(())
                    })
                    .unwrap();
            }
            catalog.rename_column_data(TABLE, "n", "n").unwrap();
            assert_eq!(
                btree
                    .load(TABLE, &ValueIndexKey::Column("n".into()))
                    .unwrap(),
                Some(vec![(1, Value::Int(1))])
            );
            assert_eq!(
                catalog.load_column_stats(TABLE).unwrap()[0].column_name,
                "n"
            );
            catalog.rename_column_data(TABLE, "n", "renamed").unwrap();
            let stats = catalog.load_column_stats(TABLE).unwrap();
            assert_eq!(stats.len(), 1);
            assert_eq!(stats[0].distinct_count, if existing { 9 } else { 1 });
            assert_eq!(
                btree
                    .load(TABLE, &ValueIndexKey::Column("renamed".into()))
                    .unwrap(),
                Some(if existing {
                    vec![(2, Value::Int(2))]
                } else {
                    vec![(1, Value::Int(1))]
                })
            );
            assert_eq!(
                btree.repairs().unwrap(),
                if existing {
                    vec![]
                } else {
                    vec![(TABLE.into(), ValueIndexKey::Column("renamed".into()))]
                }
            );
            assert_eq!(
                catalog
                    .load_catalog_indexes()
                    .unwrap()
                    .iter()
                    .find(|row| row.relation.name == "expression")
                    .unwrap()
                    .columns_json,
                EXPRESSION_KEYS
            );
            catalog.drop_column_data(TABLE, "renamed").unwrap();
            assert!(btree.repairs().unwrap().is_empty());
            assert_eq!(
                btree
                    .load(TABLE, &ValueIndexKey::Index("n".into()))
                    .unwrap(),
                Some(vec![(1, Value::Int(9))])
            );
            assert_eq!(catalog.load_catalog_indexes().unwrap().len(), 1);
        }
    }
}

#[test]
fn native_column_selection_does_not_hydrate_an_unrelated_oversized_blob() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bounded_columns.db");
    let connection = ManagedConnection::open(&path).unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    bind(&connection);
    SQLiteDocumentStore::new(connection.clone(), TABLE)
        .put(
            1,
            BTreeMap::from([
                ("small".into(), Value::Bytes(vec![1, 2, 3])),
                ("unrelated".into(), Value::Bytes(vec![7; 1024 * 1024])),
            ]),
        )
        .unwrap();
    drop(catalog);
    drop(connection);
    let limited = ManagedConnection::open(&path).unwrap();
    limited
        .bind_native_records(VersionedSessionOptions {
            retained_bytes: 256 * 1024,
        })
        .unwrap();
    let catalog = Catalog::open(limited.clone()).unwrap();
    catalog
        .rename_column_data(TABLE, "small", "renamed")
        .unwrap();
    limited.with_physical(|sql| {
        assert_eq!(sql.query_row("SELECT bytes FROM _document_blobs WHERE table_name = ?1 AND field_name = 'renamed'", [TABLE], |row| row.get::<_, Vec<u8>>(0))?, [1,2,3]);
        assert_eq!(sql.query_row("SELECT length(bytes) FROM _document_blobs WHERE table_name = ?1 AND field_name = 'unrelated'", [TABLE], |row| row.get::<_, i64>(0))?, 1024 * 1024);
        Ok(())
    }).unwrap();
    catalog.drop_column_data(TABLE, "renamed").unwrap();
    limited
        .with_physical(|sql| {
            assert_eq!(
                sql.query_row("SELECT count(*) FROM _document_blobs", [], |row| row
                    .get::<_, i64>(0))?,
                1
            );
            Ok(())
        })
        .unwrap();
}
