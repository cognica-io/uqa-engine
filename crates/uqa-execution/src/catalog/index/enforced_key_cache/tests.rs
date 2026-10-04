//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::cell::Cell;

use uqa_sql::ast::TableKeyConstraintKind;

use super::*;

fn constraint(column: &str) -> TableKeyConstraint {
    TableKeyConstraint {
        catalog_identity: None,
        index_identity: None,
        name: Some(format!("{column}_key")),
        kind: TableKeyConstraintKind::Unique,
        columns: vec![column.to_owned()],
        included_columns: Vec::new(),
        nulls_not_distinct: false,
        without_overlaps: false,
    }
}

fn key(constraint: TableKeyConstraint, ancestors: Vec<[u8; 16]>) -> EnforcedKey {
    EnforcedKey {
        constraint,
        keys: Vec::new(),
        index: None,
        index_catalog: None,
        index_ancestors: ancestors,
        predicate: None,
        constraint_owned: true,
    }
}

/// The names of the keys found, and how many times they were looked for.
fn names(
    cache: &EnforcedKeyCache,
    table: &str,
    constraints: &[TableKeyConstraint],
    indexes: &Arc<IndexRows>,
    lookups: &Cell<usize>,
) -> Vec<String> {
    cache
        .keys(table, constraints.to_vec(), indexes, |constraints| {
            lookups.set(lookups.get() + 1);
            Ok(constraints
                .into_iter()
                .map(|constraint| key(constraint, Vec::new()))
                .collect())
        })
        .unwrap()
        .into_iter()
        .map(|key| key.constraint.name.unwrap())
        .collect()
}

#[test]
fn keys_are_found_once_for_the_same_constraints_and_index_rows() {
    let cache = EnforcedKeyCache::default();
    let indexes = Arc::new(IndexRows::new());
    let lookups = Cell::new(0);
    let first = [constraint("a")];
    assert_eq!(
        names(&cache, "public.t", &first, &indexes, &lookups),
        ["a_key"]
    );
    assert_eq!(
        names(&cache, "public.t", &first, &indexes, &lookups),
        ["a_key"]
    );
    assert_eq!(lookups.get(), 1);
    // Another table with the same constraints has its own keys.
    assert_eq!(
        names(&cache, "public.u", &first, &indexes, &lookups),
        ["a_key"]
    );
    assert_eq!(lookups.get(), 2);
    // A changed constraint, and index rows that are another object, are looked up again.
    let second = [constraint("a"), constraint("b")];
    assert_eq!(
        names(&cache, "public.t", &second, &indexes, &lookups),
        ["a_key", "b_key"]
    );
    assert_eq!(lookups.get(), 3);
    let republished = Arc::new(IndexRows::new());
    assert_eq!(
        names(&cache, "public.t", &second, &republished, &lookups),
        ["a_key", "b_key"]
    );
    assert_eq!(lookups.get(), 4);
    assert_eq!(
        names(&cache, "public.t", &second, &republished, &lookups),
        ["a_key", "b_key"]
    );
    assert_eq!(lookups.get(), 4);
    // The earlier inputs of the same table were replaced, not kept beside the new ones.
    assert_eq!(
        names(&cache, "public.t", &first, &indexes, &lookups),
        ["a_key"]
    );
    assert_eq!(lookups.get(), 5);
    assert_eq!(cache.entries.lock().len(), 2);
}

#[test]
fn the_keys_of_a_partition_index_and_failed_lookups_are_not_kept() {
    let cache = EnforcedKeyCache::default();
    let indexes = Arc::new(IndexRows::new());
    let lookups = Cell::new(0);
    for _ in 0..2 {
        let keys = cache
            .keys(
                "public.part",
                vec![constraint("a")],
                &indexes,
                |constraints| {
                    lookups.set(lookups.get() + 1);
                    Ok(constraints
                        .into_iter()
                        .map(|constraint| key(constraint, vec![[7; 16]]))
                        .collect())
                },
            )
            .unwrap();
        assert_eq!(keys[0].index_ancestors, [[7; 16]]);
    }
    assert_eq!(lookups.get(), 2);
    for _ in 0..2 {
        assert!(cache
            .keys("public.broken", Vec::new(), &indexes, |_| {
                lookups.set(lookups.get() + 1);
                Err(uqa_storage::StorageBackendError::Other("no catalog".into()))
            })
            .is_err());
    }
    assert_eq!(lookups.get(), 4);
    assert!(cache.entries.lock().is_empty());
}

#[test]
fn the_oldest_table_makes_room() {
    let cache = EnforcedKeyCache::default();
    let indexes = Arc::new(IndexRows::new());
    let lookups = Cell::new(0);
    for table in 0..=TABLES {
        names(&cache, &format!("public.t{table}"), &[], &indexes, &lookups);
    }
    assert_eq!(cache.entries.lock().len(), TABLES);
    names(&cache, "public.t0", &[], &indexes, &lookups);
    assert_eq!(lookups.get(), TABLES + 2);
    names(
        &cache,
        &format!("public.t{TABLES}"),
        &[],
        &indexes,
        &lookups,
    );
    assert_eq!(lookups.get(), TABLES + 2);
}
