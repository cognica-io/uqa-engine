//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact standalone storage names remain independent while catalog names and stable identities move.

use super::{
    bind, fields, index, open, schema, Catalog, DocumentStore, ManagedConnection,
    SQLiteDocumentStore, MODES,
};

#[test]
fn native_table_rename_matches_legacy_exact_names_and_existing_target_rows() {
    let names = ["zdocs", "public.zdocs", "adocs", "public.adocs"];
    for native in [false, true] {
        for source in 0..2 {
            for target in 2..4 {
                for missing in [false, true] {
                    let connection = ManagedConnection::open_in_memory().unwrap();
                    let catalog = Catalog::open(connection.clone()).unwrap();
                    catalog.save_table(&schema("zdocs", 1, 1)).unwrap();
                    catalog
                        .save_catalog_index_row(&index("i", "public.zdocs"))
                        .unwrap();
                    let mut expected = vec![vec![None; 4]; 4];
                    for (i, name) in names.iter().enumerate() {
                        if missing && i == source {
                            continue;
                        }
                        SQLiteDocumentStore::new(connection.clone(), *name)
                            .put((i + 1) as u64, fields(i as i64))
                            .unwrap();
                        expected[i][i] = Some(fields(i as i64));
                    }
                    if native {
                        bind(&connection);
                    }
                    connection.begin_transaction().unwrap();
                    catalog
                        .rename_table_data(names[source], names[target])
                        .unwrap();
                    connection.commit_transaction().unwrap();
                    expected[target][source] = expected[source][source].take();
                    for (i, name) in names.iter().enumerate() {
                        let store = SQLiteDocumentStore::new(connection.clone(), *name);
                        for id in 1..=4 {
                            assert_eq!(
                                store.get(id).unwrap(),
                                expected[i][id as usize - 1],
                                "native={native} {} -> {} missing={missing} name={name} id={id}",
                                names[source],
                                names[target]
                            );
                        }
                    }
                    let rows = catalog.load_tables().unwrap();
                    assert_eq!(rows.len(), 1);
                    assert_eq!(rows[0].relation.name, "adocs");
                    assert_eq!(rows[0].object_id, [1; 16]);
                    assert_eq!(rows[0].storage_generation, [1; 16]);
                    assert_eq!(
                        catalog.load_catalog_indexes().unwrap()[0].table_name,
                        "public.adocs"
                    );
                }
            }
        }
    }
}

#[test]
fn native_table_rename_rejects_colliding_data_without_changing_names_or_rows() {
    for native in [false, true] {
        for (from, to) in [
            ("zdocs", "adocs"),
            ("public.zdocs", "public.adocs"),
            ("zdocs", "public.adocs"),
            ("public.zdocs", "adocs"),
        ] {
            let connection = ManagedConnection::open_in_memory().unwrap();
            let catalog = Catalog::open(connection.clone()).unwrap();
            catalog.save_table(&schema("zdocs", 1, 1)).unwrap();
            SQLiteDocumentStore::new(connection.clone(), from)
                .put(1, fields(1))
                .unwrap();
            SQLiteDocumentStore::new(connection.clone(), to)
                .put(1, fields(2))
                .unwrap();
            if native {
                bind(&connection);
            }
            assert!(catalog.rename_table_data(from, to).is_err());
            assert!(!connection.in_transaction());
            assert_eq!(catalog.load_tables().unwrap()[0].relation.name, "zdocs");
            assert_eq!(
                SQLiteDocumentStore::new(connection.clone(), from)
                    .get(1)
                    .unwrap(),
                Some(fields(1))
            );
            assert_eq!(
                SQLiteDocumentStore::new(connection.clone(), to)
                    .get(1)
                    .unwrap(),
                Some(fields(2))
            );
        }
    }
}

#[test]
fn native_rename_and_name_reuse_are_atomic_in_both_lexical_orders() {
    for mode in MODES {
        for (from, to) in [("a", "z"), ("z", "a")] {
            for commit in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join("rename.db");
                let connection = open(mode, &path);
                let catalog = Catalog::open(connection.clone()).unwrap();
                bind(&connection);
                catalog.save_table(&schema(from, 1, 1)).unwrap();
                let old_name = format!("public.{from}");
                let new_name = format!("public.{to}");
                let mut documents = SQLiteDocumentStore::new(connection.clone(), &old_name);
                documents.put(1, fields(1)).unwrap();
                let old = documents.snapshot().unwrap();
                connection.begin_transaction().unwrap();
                catalog.rename_table_data(&old_name, &new_name).unwrap();
                catalog.save_table(&schema(from, 2, 2)).unwrap();
                documents.put(1, fields(2)).unwrap();
                if commit {
                    connection.commit_transaction().unwrap();
                } else {
                    connection.rollback_transaction().unwrap();
                }
                assert_eq!(old.get(1).unwrap(), Some(fields(1)));
                drop(old);
                drop(documents);
                drop(catalog);
                drop(connection);
                let reopened = open(mode, &path);
                bind(&reopened);
                assert_eq!(
                    SQLiteDocumentStore::new(reopened.clone(), &old_name)
                        .get(1)
                        .unwrap(),
                    Some(fields(if commit { 2 } else { 1 }))
                );
                assert_eq!(
                    SQLiteDocumentStore::new(reopened.clone(), &new_name)
                        .get(1)
                        .unwrap(),
                    commit.then(|| fields(1))
                );
                let catalog = Catalog::open(reopened).unwrap();
                let rows = catalog.load_tables().unwrap();
                assert_eq!(rows.len(), if commit { 2 } else { 1 });
                assert_eq!(
                    rows.iter()
                        .find(|row| row.relation.name == from)
                        .unwrap()
                        .object_id,
                    [if commit { 2 } else { 1 }; 16]
                );
            }
        }
    }
}
