//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared native vector guards retain document and catalog conflicts in either publication order.

use super::*;

#[test]
fn native_vector_document_and_catalog_conflicts_preserve_the_winner_in_both_orders() {
    for kind in [IndexKind::Ivf, IndexKind::Hnsw] {
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
                    let name = format!("vectors_{case}_{reverse}");
                    let identity = case * 2 + u8::from(reverse) + 1;
                    let schema = schema(&name, identity, identity);
                    let table = schema.relation.qualified_name();
                    let destination = format!("{table}_renamed");
                    catalog.save_table(&schema).unwrap();
                    let mut left = index(&a, kind, &table);
                    left.add(1, X.to_vec()).unwrap();
                    left.add(2, Y.to_vec()).unwrap();
                    left.initialize().unwrap();
                    let mut right = index(&b, kind, &table);
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
                        4 | 5 => recreate_index(&b, kind, &table, case == 5),
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
                    let before = generation(winner, kind, target_table, target_field);
                    assert!(
                        loser.commit_transaction().is_err(),
                        "{kind:?}, case {case}, reverse {reverse}"
                    );
                    loser.rollback_transaction().unwrap();
                    assert_eq!(generation(winner, kind, target_table, target_field), before);
                }
            }
        }
    }
}

fn recreate_index(connection: &ManagedConnection, kind: IndexKind, table: &str, changed: bool) {
    let mut replacement: Box<dyn VectorIndex> = match kind {
        IndexKind::Ivf => {
            SQLiteIVFIndex::drop_metadata(connection, table, "embedding").unwrap();
            let nlist = if changed { 3 } else { 2 };
            Box::new(SQLiteIVFIndex::with_params(
                connection.clone(),
                table,
                "embedding",
                3,
                nlist,
                nlist,
                2,
            ))
        }
        IndexKind::Hnsw => {
            SQLiteHNSWIndex::drop_metadata(connection, table, "embedding").unwrap();
            let mut params = uqa_storage::vector_index::HNSWIndexParams::default();
            if changed {
                params.m += 1;
            }
            Box::new(SQLiteHNSWIndex::with_params(
                connection.clone(),
                table,
                "embedding",
                3,
                params,
            ))
        }
        IndexKind::Exact => unreachable!(),
    };
    replacement.initialize().unwrap();
}
