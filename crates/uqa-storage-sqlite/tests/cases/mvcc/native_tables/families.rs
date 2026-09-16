//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lifecycle transport covers every table-owned physical family, including opaque retrieval payloads.

use super::*;
use rusqlite::{params_from_iter, types::Value as SQLValue};
use uqa_storage::{mvcc::VersionedPersistence, read_control::StorageReadControl};
use uqa_storage_sqlite::{
    mvcc::native::{
        NativeColumnType, NativeRecord, NativeRecordFamily as Family, NativeRecordOwner,
    },
    SQLiteRecordStore,
};

pub(in crate::mvcc) fn families() -> impl Iterator<Item = Family> {
    Family::all().filter(|family| family.layout().columns.contains(&"table_name"))
}

pub(in crate::mvcc) fn rows(
    connection: &ManagedConnection,
    family: Family,
    table: &str,
) -> Vec<Vec<SQLValue>> {
    let layout = family.layout();
    connection
        .with_physical(|sqlite| {
            let columns = layout
                .columns
                .iter()
                .map(|column| format!("\"{column}\""))
                .collect::<Vec<_>>()
                .join(",");
            let mut statement = sqlite.prepare(&format!(
                "SELECT {columns} FROM {} WHERE table_name = ?1 ORDER BY {columns}",
                layout.table
            ))?;
            let rows = statement
                .query_map([table], |row| {
                    (0..layout.columns.len())
                        .map(|slot| row.get(slot))
                        .collect::<rusqlite::Result<Vec<SQLValue>>>()
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .unwrap()
}

pub(in crate::mvcc) fn seed_missing_families(connection: &ManagedConnection) {
    // Parent markers precede child rows. Opaque payloads exercise transport, not search/index algorithm acceptance.
    let mut ordered: Vec<_> = families().collect();
    ordered.sort_by_key(|family| match family {
        Family::IVFIndexes | Family::HNSWIndexes | Family::BtreeIndexes => 0,
        Family::HNSWNodes | Family::IVFCentroids => 1,
        _ => 2,
    });
    for family in ordered {
        if !rows(connection, family, "public.docs").is_empty() {
            continue;
        }
        let layout = family.layout();
        let values = layout
            .columns
            .iter()
            .zip(layout.column_types)
            .zip(layout.nullable)
            .map(|((name, kind), nullable)| {
                if *name == "table_name" {
                    return SQLValue::Text("public.docs".into());
                }
                if *nullable {
                    return SQLValue::Null;
                }
                match kind {
                    NativeColumnType::Integer => SQLValue::Integer(1),
                    NativeColumnType::Blob => SQLValue::Blob(vec![0, 1, 2, 255]),
                    NativeColumnType::Text | NativeColumnType::TextOrBlob => {
                        SQLValue::Text(if *name == "seed" { "1" } else { "n" }.into())
                    }
                }
            })
            .collect::<Vec<_>>();
        connection
            .with(|sqlite| {
                let columns = layout
                    .columns
                    .iter()
                    .map(|column| format!("\"{column}\""))
                    .collect::<Vec<_>>()
                    .join(",");
                let parameters = vec!["?"; values.len()].join(",");
                sqlite.execute(
                    &format!(
                        "INSERT INTO {} ({columns}) VALUES ({parameters})",
                        layout.table
                    ),
                    params_from_iter(values),
                )?;
                Ok(())
            })
            .unwrap_or_else(|error| panic!("seed {}: {error}", layout.table));
    }
}

#[test]
fn table_rename_and_generation_transfer_preserve_every_native_owned_payload_and_history() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    write(&connection, &catalog, "docs", 1, 1);
    write(&connection, &catalog, "untouched", 9, 9);
    seed_missing_families(&connection);
    let original: Vec<_> = families()
        .map(|family| (family, rows(&connection, family, "public.docs")))
        .collect();
    assert_eq!(original.len(), 23);
    let control = StorageReadControl::with_limit(1 << 24);
    let store = SQLiteRecordStore::for_native(&connection, &control).unwrap();
    let old = store.snapshot(&control).unwrap();
    bind(&connection);
    catalog
        .rename_table_data("public.docs", "public.renamed")
        .unwrap();
    let mut schema = catalog
        .load_tables()
        .unwrap()
        .into_iter()
        .find(|row| row.relation.name == "renamed")
        .unwrap();
    schema.storage_generation = [2; 16];
    catalog.save_table(&schema).unwrap();
    let latest = store.snapshot(&control).unwrap();
    for (family, before) in original {
        let column = family
            .layout()
            .columns
            .iter()
            .position(|column| *column == "table_name")
            .unwrap();
        let mut expected = before.clone();
        for row in &mut expected {
            row[column] = SQLValue::Text("public.renamed".into());
        }
        assert_eq!(
            rows(&connection, family, "public.renamed"),
            expected,
            "{}",
            family.layout().table
        );
        assert!(rows(&connection, family, "public.docs").is_empty());
        for (before, after) in before.iter().zip(&expected) {
            let record = NativeRecord::encode(
                family,
                NativeRecordOwner::Object {
                    identity: [1; 16],
                    generation: [1; 16],
                },
                &before.iter().map(Into::into).collect::<Vec<_>>(),
                &control,
            )
            .unwrap();
            assert_eq!(
                &***old
                    .get(record.key(), &control)
                    .unwrap()
                    .unwrap()
                    .value()
                    .unwrap(),
                record.row()
            );
            assert!(latest
                .get(record.key(), &control)
                .unwrap()
                .unwrap()
                .value()
                .is_none());
            let record = NativeRecord::encode(
                family,
                NativeRecordOwner::Object {
                    identity: [1; 16],
                    generation: [2; 16],
                },
                &after.iter().map(Into::into).collect::<Vec<_>>(),
                &control,
            )
            .unwrap();
            assert_eq!(
                &***latest
                    .get(record.key(), &control)
                    .unwrap()
                    .unwrap()
                    .value()
                    .unwrap(),
                record.row()
            );
        }
    }
    catalog.drop_table_and_data("public.renamed").unwrap();
    for family in families() {
        assert!(rows(&connection, family, "public.renamed").is_empty());
    }
    assert_value(&connection, &catalog, "untouched", Some(9));
}
