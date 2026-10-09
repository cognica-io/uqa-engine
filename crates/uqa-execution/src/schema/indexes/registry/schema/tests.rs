//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::test_support::{empty_catalog, table_snapshot};
use std::{collections::BTreeMap, sync::Arc};
use uqa_sql::ast::{PartitionBound, TableHierarchy};

#[test]
fn descendant_names_borrow_the_candidate_without_copying_unrelated_tables() {
    for unrelated in [1, 256] {
        let uqa_sql::Statement::CreateTable(parsed) =
            uqa_sql::compile("CREATE TABLE root(v int, PRIMARY KEY(v)) PARTITION BY LIST(v)")
                .unwrap()
                .remove(0)
        else {
            panic!("table");
        };
        let root = RelationIdentity::new("public", "root");
        let mut columns = parsed.columns;
        let mut constraints = TableConstraintSet {
            key_constraints: parsed.key_constraints,
            hierarchy: parsed.hierarchy,
            ..Default::default()
        };
        let mut next = 100_u128;
        let mut allocate = |_: &str| {
            next += 1;
            Ok(next.to_le_bytes())
        };
        uqa_sql::schema::constraint_metadata::materialize_constraint_metadata(
            &root,
            &mut columns,
            &mut constraints,
            &mut allocate,
        )
        .unwrap();
        let mut snapshot = empty_catalog().snapshot().clone();
        snapshot.tables.insert(
            root.clone(),
            table_snapshot([1; 16], columns.clone(), TableConstraintSet::default()),
        );
        let mut declarations = BTreeMap::new();
        for (index, suffix) in ["a", "b"].into_iter().enumerate() {
            // The generated index names collide after PostgreSQL's 63-byte identifier limit.
            let child = RelationIdentity::new("public", format!("{}{suffix}", "p".repeat(60)));
            let definition = TableConstraintSet {
                hierarchy: TableHierarchy {
                    parents: vec![root.qualified_name()],
                    partition_bound: Some(PartitionBound::Default),
                    ..Default::default()
                },
                ..Default::default()
            };
            let object_id = (index as u128 + 10).to_le_bytes();
            snapshot.tables.insert(
                child.clone(),
                table_snapshot(object_id, columns.clone(), definition.clone()),
            );
            declarations.insert(child, (columns.clone(), definition, object_id));
        }
        for index in 0..unrelated {
            snapshot.tables.insert(
                RelationIdentity::new("public", format!("unrelated_{index}")),
                table_snapshot(
                    (index as u128 + 1000).to_le_bytes(),
                    Vec::new(),
                    TableConstraintSet::default(),
                ),
            );
        }
        let original = CatalogReadView::new(snapshot);
        let mut candidate = original.snapshot().clone();
        replace(&mut candidate, &root, &columns, &constraints).unwrap();
        let observed =
            &original.snapshot().tables[&RelationIdentity::new("public", "unrelated_0")].columns;
        assert_eq!(Arc::strong_count(observed), 2);
        let mut allocations = 0;
        let mut allocate = |_: &str| {
            assert_eq!(
                Arc::strong_count(observed),
                2,
                "descendant naming copied the full table map"
            );
            allocations += 1;
            Ok((allocations + 200_u128).to_le_bytes())
        };
        let changes =
            prepare_descendants(&original, &mut candidate, &root, &mut allocate, |name| {
                Ok(declarations[name].clone())
            })
            .unwrap();
        assert_eq!(changes.len(), 2);
        assert!(allocations > 0);
        let mut names = BTreeSet::new();
        let mut identities = BTreeSet::new();
        for change in changes {
            assert_eq!(
                change.object_id,
                original.snapshot().tables[&change.relation].object_id
            );
            let keys = &candidate.tables[&change.relation].keys;
            assert_eq!(keys.len(), 1);
            assert!(names.insert(keys[0].name.clone().unwrap()));
            assert!(identities.insert(keys[0].catalog_identity.unwrap().object_id));
            assert_eq!(keys[0].columns, ["v"]);
            assert_eq!(
                change
                    .constraints
                    .hierarchy
                    .partition_inherited_key_constraints[0],
                keys[0]
            );
            assert!(original.snapshot().tables[&change.relation].keys.is_empty());
        }
        assert_eq!(
            names,
            BTreeSet::from([
                format!("{}_pkey", "p".repeat(58)),
                format!("{}_pkey1", "p".repeat(57)),
            ])
        );
        for (name, before) in &original.snapshot().tables {
            if name.name.starts_with("unrelated_") {
                let after = &candidate.tables[name];
                assert!(Arc::ptr_eq(&before.columns, &after.columns));
                assert!(Arc::ptr_eq(&before.keys, &after.keys));
                assert!(Arc::ptr_eq(&before.hierarchy, &after.hierarchy));
            }
        }
    }
}
