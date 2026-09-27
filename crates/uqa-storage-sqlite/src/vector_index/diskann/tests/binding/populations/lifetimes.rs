//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::Catalog;

#[test]
fn native_diskann_population_raw_writers_preserve_selected_counts_and_retirement() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let connection = open(&directory.path().join("population-raw-writers.db"), mode);
        let control = StorageReadControl::with_limit(1 << 22);
        seed(&connection, &control);
        let index = live(&connection, &control);
        for kind in 0..3 {
            let mut raw: Box<dyn VectorIndex> = match kind {
                0 => Box::new(crate::SQLiteVectorIndex::new(
                    connection.clone(),
                    TABLE,
                    FIELD,
                    2,
                )),
                1 => Box::new(crate::SQLiteIVFIndex::new(
                    connection.clone(),
                    TABLE,
                    FIELD,
                    2,
                )),
                _ => Box::new(crate::SQLiteHNSWIndex::new(
                    connection.clone(),
                    TABLE,
                    FIELD,
                    2,
                )),
            };
            let before = super::rename::image(&connection);
            for operation in 0..5 {
                let result = match operation {
                    0 => raw.add(1, vec![0.0, 1.0]),
                    1 => raw.add_many(1, vec![vec![0.0, 1.0], vec![1.0, 0.0]]),
                    2 => raw.delete(2),
                    3 => raw.delete(3),
                    _ => raw.add(4, vec![1.0, 0.0]),
                };
                assert!(
                    result.is_err(),
                    "raw owner {kind} accepted operation {operation}"
                );
                assert_eq!(super::rename::image(&connection), before);
                counts(&index, 3, 0);
                assert_eq!(raw.count().unwrap(), 3);
            }
        }
        connection.begin_transaction().unwrap();
        canonical(&connection, TABLE, FIELD, 2)
            .retire_index(&row().relation, &Resolver, &control)
            .unwrap();
        let mut raw = crate::SQLiteVectorIndex::new(connection.clone(), TABLE, FIELD, 2);
        raw.add(1, vec![0.0, 1.0]).unwrap();
        raw.delete(2).unwrap();
        connection.rollback_transaction().unwrap();
        counts(&index, 3, 0);
    }
}

#[test]
fn native_diskann_population_structural_movement_preserves_mutations_undo_and_old_readers() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let connection = open(&directory.path().join("population-lifetimes.db"), mode);
        let control = StorageReadControl::with_limit(1 << 22);
        let generation = seed(&connection, &control);
        let original = capture(&connection, &control);
        let catalog = Catalog::open(connection.clone()).unwrap();
        connection.begin_transaction().unwrap();
        catalog.rename_column_data(TABLE, FIELD, "after").unwrap();
        catalog
            .rename_table_data(TABLE, "public.population_after")
            .unwrap();
        let renamed = canonical(&connection, "public.population_after", "after", 2);
        let require = |expected| {
            assert_eq!(
                renamed
                    .retain(&control)
                    .unwrap()
                    .population_counts(generation, &control)
                    .unwrap(),
                expected,
            );
        };
        require(Some(DiskANNCanonicalCounts::new(3, 0).unwrap()));
        renamed
            .replace(1, &[vec![1.0, 0.0], vec![0.0, 1.0]], &control)
            .unwrap();
        require(Some(DiskANNCanonicalCounts::new(4, 2).unwrap()));
        connection.savepoint("population_field").unwrap();
        catalog
            .drop_column_data("public.population_after", "after")
            .unwrap();
        require(None);
        connection
            .rollback_to_savepoint("population_field")
            .unwrap();
        require(Some(DiskANNCanonicalCounts::new(4, 2).unwrap()));
        connection.commit_transaction().unwrap();
        require(Some(DiskANNCanonicalCounts::new(4, 2).unwrap()));
        assert_eq!(
            original.population_counts(generation, &control).unwrap(),
            Some(DiskANNCanonicalCounts::new(3, 0).unwrap())
        );
        connection.begin_transaction().unwrap();
        catalog.purge_table_data("public.population_after").unwrap();
        require(None);
        connection.rollback_transaction().unwrap();
        require(Some(DiskANNCanonicalCounts::new(4, 2).unwrap()));
        catalog
            .drop_column_data("public.population_after", "after")
            .unwrap();
        require(None);
        connection.reclaim_obsolete().unwrap();
        assert_eq!(
            original.population_counts(generation, &control).unwrap(),
            Some(DiskANNCanonicalCounts::new(3, 0).unwrap())
        );
        drop(original);
        connection.reclaim_obsolete().unwrap();
        assert_no_population_history(&connection, &control);
    }
}

fn assert_no_population_history(connection: &ManagedConnection, control: &StorageReadControl) {
    use crate::mvcc::native::{NativeRecordFamily as Family, NativeRecordIdentity};
    connection
        .with_physical(|sqlite| {
            for family in [Family::VectorPopulations, Family::VectorPopulationWitnesses] {
                let prefix = NativeRecordIdentity::family_prefix(family, control)?;
                for table in ["_uqa_mvcc_heads", "_uqa_mvcc_versions"] {
                    let count: i64 = sqlite.query_row(
                        &format!("SELECT count(*) FROM {table} WHERE substr(key, 1, ?1) = ?2"),
                        rusqlite::params![prefix.len() as i64, &*prefix],
                        |row| row.get(0),
                    )?;
                    assert_eq!(count, 0, "population history remains in {table}");
                }
                let count: i64 = sqlite.query_row(
                    &format!("SELECT count(*) FROM {}", family.layout().table),
                    [],
                    |row| row.get(0),
                )?;
                assert_eq!(count, 0, "population rows remain after field removal");
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn native_diskann_population_rejects_corrupt_headers_missing_empty_witnesses_and_exhaustion() {
    use crate::mvcc::native::{populations, NativeRecord, NativeRecordFamily as Family};
    use rusqlite::types::ValueRef;
    let directory = tempfile::tempdir().unwrap();
    let connection = open(&directory.path().join("population-corruption.db"), 0);
    let control = StorageReadControl::with_limit(1 << 22);
    let generation = seed(&connection, &control);
    let index = live(&connection, &control);
    let snapshot = connection.native_snapshot().unwrap().unwrap();
    let owner = snapshot.table_owner(TABLE).unwrap().unwrap();
    let header = populations::header_key(owner, FIELD.as_bytes(), generation, &control).unwrap();
    connection.begin_transaction().unwrap();
    connection
        .with_native_write(|_, batch| {
            batch.put(&header, b"invalid population")?;
            Ok(())
        })
        .unwrap()
        .unwrap();
    assert!(index.query_metadata().is_err());
    assert!(index.replace(1, &[]).is_err());
    connection.rollback_transaction().unwrap();
    counts(&index, 3, 0);
    let mut witness = None;
    snapshot
        .visit_rows(
            Family::VectorPopulationWitnesses,
            Some(owner),
            &[ValueRef::Text(FIELD.as_bytes())],
            |row| {
                if row[3] == ValueRef::Integer(3) {
                    witness = Some(NativeRecord::encode(
                        Family::VectorPopulationWitnesses,
                        owner,
                        row,
                        &control,
                    )?);
                }
                Ok(())
            },
        )
        .unwrap();
    let witness = witness.unwrap();
    connection.begin_transaction().unwrap();
    connection
        .with_native_write(|_, batch| {
            batch.delete(witness.key())?;
            Ok(())
        })
        .unwrap()
        .unwrap();
    assert!(index.replace(3, &[vec![1.0, 0.0]]).is_err());
    assert!(capture(&connection, &control)
        .origin(3, &control)
        .unwrap()
        .is_some());
    counts(&index, 3, 0);
    connection.rollback_transaction().unwrap();
    let held = control
        .memory()
        .reserve(control.memory().limit() - control.memory().used())
        .unwrap();
    assert!(index.replace(1, &[]).is_err());
    drop(held);
    counts(&index, 3, 0);
}
