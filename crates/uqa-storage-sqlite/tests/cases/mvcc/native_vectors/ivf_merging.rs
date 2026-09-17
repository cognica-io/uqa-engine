//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native IVF publications preserve the common owner's serial training transitions.

use super::*;
use uqa_storage::ivf_index::{IVFIndex, IVFState};

fn train_stale(index: &mut IVFIndex) {
    if index.state() == IVFState::Stale {
        index.train().unwrap();
    }
}

#[test]
fn independent_native_ivf_writers_merge_shared_training_generations() {
    for mode in MODES {
        for seed in [0, 2, 8] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("ivf-merge.db");
            let connection = open(mode, &path);
            Catalog::open(connection.clone()).unwrap();
            bind(&connection);
            let mut left = index(&connection, IndexKind::Ivf, "docs");
            let mut serial = IVFIndex::with_params(3, 2, 2, 2);
            for document in 1..=seed {
                let vector = vec![1.0, document as f32, 0.5];
                left.add(document, vector.clone()).unwrap();
                serial.add(document, vector).unwrap();
                train_stale(&mut serial);
            }
            left.initialize().unwrap();
            serial.initialize().unwrap();
            let other = open(mode, &path);
            bind(&other);
            let mut right = index(&other, IndexKind::Ivf, "docs");
            let baseline = left.snapshot().unwrap();
            connection.begin_transaction().unwrap();
            other.begin_transaction().unwrap();
            left.add_many(11, vec![X.to_vec(), Y.to_vec()]).unwrap();
            let private = left.snapshot().unwrap();
            right.add(12, Z.to_vec()).unwrap();
            other.commit_transaction().unwrap();
            assert!(connection.in_transaction());
            connection.commit_transaction().unwrap();
            serial.add(12, Z.to_vec()).unwrap();
            train_stale(&mut serial);
            serial.add_many(11, vec![X.to_vec(), Y.to_vec()]).unwrap();
            train_stale(&mut serial);
            ivf::matches_metadata(&connection, &serial.metadata_snapshot());
            assert_eq!(left.count().unwrap(), seed as usize + 3);
            assert_eq!(right.count().unwrap(), seed as usize + 3);
            assert_eq!(baseline.count().unwrap(), seed as usize);
            assert_eq!(private.count().unwrap(), seed as usize + 2);
            drop((left, right, private, baseline, other, connection));
            let reopened = open(mode, &path);
            bind(&reopened);
            ivf::matches_metadata(&reopened, &serial.metadata_snapshot());
            assert_eq!(
                index(&reopened, IndexKind::Ivf, "docs")
                    .search_knn(&X, 100)
                    .unwrap()
                    .len(),
                seed as usize + 2
            );
        }
    }
}

type Generation = Vec<Vec<Vec<rusqlite::types::Value>>>;
fn generation(connection: &ManagedConnection, table: &str, field: &str) -> Generation {
    connection
        .with_physical(|sqlite| {
            let mut generation = Vec::new();
            for (family, order) in [
                ("_vectors", "doc_id,vector_ordinal"),
                ("_ivf_indexes", "field"),
                ("_ivf_centroids", "centroid_id"),
                ("_ivf_assignments", "doc_id,vector_ordinal"),
            ] {
                let mut query = sqlite.prepare(&format!(
                    "SELECT * FROM {family} WHERE table_name = ?1 AND field = ?2 ORDER BY {order}"
                ))?;
                let columns = query.column_count();
                generation.push(
                    query
                        .query_map([table, field], |row| {
                            (0..columns)
                                .map(|column| row.get(column))
                                .collect::<rusqlite::Result<Vec<_>>>()
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?,
                );
            }
            Ok(generation)
        })
        .unwrap()
}

#[test]
fn native_ivf_document_and_catalog_conflicts_preserve_the_winner_in_both_orders() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("conflicts.db");
        let a = open(mode, &path);
        Catalog::open(a.clone()).unwrap();
        bind(&a);
        let b = open(mode, &path);
        bind(&b);
        let catalog = Catalog::open(b.clone()).unwrap();
        for case in 0_u8..11 {
            for reverse in [false, true] {
                let name = format!("ivf_{case}_{reverse}");
                let identity = case * 2 + u8::from(reverse) + 1;
                let schema = schema(&name, identity, identity);
                let table = schema.relation.qualified_name();
                let destination = format!("{table}_renamed");
                catalog.save_table(&schema).unwrap();
                let mut left = index(&a, IndexKind::Ivf, &table);
                left.add(1, X.to_vec()).unwrap();
                left.add(2, Y.to_vec()).unwrap();
                left.initialize().unwrap();
                let mut right = index(&b, IndexKind::Ivf, &table);
                a.begin_transaction().unwrap();
                b.begin_transaction().unwrap();
                match case {
                    0 => left.add_many(11, vec![]).unwrap(),
                    1 => left.delete(11).unwrap(),
                    _ => left.add(11, Z.to_vec()).unwrap(),
                }
                match case {
                    0 | 1 => right.add(11, X.to_vec()).unwrap(),
                    2 => right.clear().unwrap(),
                    3 => right.initialize().unwrap(),
                    4 | 5 => {
                        SQLiteIVFIndex::drop_metadata(&b, &table, "embedding").unwrap();
                        let nlist = if case == 5 { 3 } else { 2 };
                        let mut replacement = SQLiteIVFIndex::with_params(
                            b.clone(),
                            &table,
                            "embedding",
                            3,
                            nlist,
                            nlist,
                            2,
                        );
                        replacement.initialize().unwrap();
                    }
                    6 | 7 => {
                        if case == 6 {
                            catalog.drop_table_and_data(&table).unwrap();
                        } else {
                            catalog.purge_table_data(&table).unwrap();
                        }
                        catalog.save_table(&schema).unwrap();
                        right.add(1, X.to_vec()).unwrap();
                        right.add(2, Y.to_vec()).unwrap();
                        right.initialize().unwrap();
                    }
                    8 => catalog.rename_table_data(&table, &destination).unwrap(),
                    9 => catalog.drop_column_data(&table, "embedding").unwrap(),
                    _ => catalog
                        .rename_column_data(&table, "embedding", "renamed")
                        .unwrap(),
                }
                let (winner, loser) = if reverse { (&a, &b) } else { (&b, &a) };
                winner.commit_transaction().unwrap();
                let target_table = if !reverse && case == 8 {
                    &destination
                } else {
                    &table
                };
                let target_field = if !reverse && case == 10 {
                    "renamed"
                } else {
                    "embedding"
                };
                let before = generation(winner, target_table, target_field);
                assert!(
                    loser.commit_transaction().is_err(),
                    "case {case}, reverse {reverse}"
                );
                loser.rollback_transaction().unwrap();
                assert_eq!(generation(winner, target_table, target_field), before);
            }
        }
    }
}

#[test]
fn native_ivf_savepoints_and_publication_retry_preserve_intervening_writes() {
    for mode in MODES {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("retry.db");
        let a = open(mode, &path);
        Catalog::open(a.clone()).unwrap();
        bind(&a);
        let b = open(mode, &path);
        bind(&b);
        let mut left = index(&a, IndexKind::Ivf, "docs");
        let mut right = index(&b, IndexKind::Ivf, "docs");
        let mut serial = IVFIndex::with_params(3, 2, 2, 2);
        for document in 1..=8 {
            let vector = vec![1.0, document as f32, 0.5];
            left.add(document, vector.clone()).unwrap();
            serial.add(document, vector).unwrap();
            train_stale(&mut serial);
        }
        left.initialize().unwrap();
        serial.initialize().unwrap();
        a.begin_transaction().unwrap();
        a.savepoint("discard").unwrap();
        left.add(99, X.to_vec()).unwrap();
        let discarded = left.snapshot().unwrap();
        a.rollback_to_savepoint("discard").unwrap();
        a.release_savepoint("discard").unwrap();
        left.delete(1).unwrap();
        left.add_many(11, vec![X.to_vec(), Y.to_vec()]).unwrap();
        left.add_many(11, vec![]).unwrap();
        left.add_many(11, vec![Y.to_vec(), Z.to_vec()]).unwrap();
        let private = left.snapshot().unwrap();
        right.add(12, Z.to_vec()).unwrap();
        let before = generation(&b, "docs", "embedding");
        b.with_physical(|sqlite| {
            sqlite.execute_batch("CREATE TRIGGER fail_ivf_merge BEFORE INSERT ON _ivf_indexes BEGIN SELECT RAISE(ABORT, 'injected IVF merge failure'); END")?;
            Ok(())
        }).unwrap();
        assert!(a.commit_transaction().is_err());
        assert_eq!(generation(&b, "docs", "embedding"), before);
        b.with_physical(|sqlite| {
            sqlite.execute_batch("DROP TRIGGER fail_ivf_merge")?;
            Ok(())
        })
        .unwrap();
        right.add(13, X.to_vec()).unwrap();
        a.commit_transaction().unwrap();
        for (document, vectors) in [(12, vec![Z.to_vec()]), (13, vec![X.to_vec()])] {
            serial.add_many(document, vectors).unwrap();
            train_stale(&mut serial);
        }
        serial.delete(1).unwrap();
        train_stale(&mut serial);
        for vectors in [
            vec![X.to_vec(), Y.to_vec()],
            vec![],
            vec![Y.to_vec(), Z.to_vec()],
        ] {
            serial.add_many(11, vectors).unwrap();
            train_stale(&mut serial);
        }
        ivf::matches_metadata(&a, &serial.metadata_snapshot());
        assert_eq!(discarded.count().unwrap(), 9);
        assert_eq!(private.count().unwrap(), 9);
        assert_eq!(left.count().unwrap(), 11);
        let expected = generation(&a, "docs", "embedding");
        drop((left, right, private, discarded, a, b));
        let reopened = open(mode, &path);
        bind(&reopened);
        assert_eq!(generation(&reopened, "docs", "embedding"), expected);
    }
}
