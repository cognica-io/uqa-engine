//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    test_support::{empty_catalog, table_snapshot},
    CatalogReadView,
};
use std::collections::BTreeSet;

fn catalog(unrelated: usize) -> (CatalogReadView, KeyConstraintNames) {
    let (schema, mut constraints, names) = fixture();
    constraints.key_constraints[0].name = Some("current".into());
    constraints.hierarchy.partition_inherited_key_constraints = constraints.key_constraints.clone();
    let table = table_snapshot(schema.object_id, Vec::new(), constraints);
    let mut snapshot = empty_catalog().snapshot().clone();
    snapshot.tables.insert(schema.relation, table);
    for index in 0..unrelated {
        snapshot.tables.insert(
            RelationIdentity::new("public", format!("unrelated_{index}")),
            table_snapshot(
                (index as u128 + 10).to_le_bytes(),
                Vec::new(),
                TableConstraintSet::default(),
            ),
        );
    }
    (CatalogReadView::new(snapshot), names)
}

#[test]
fn unchanged_key_names_retain_the_complete_catalog_allocation() {
    for unrelated in [0, 1, 256] {
        let (catalog, names) = catalog(unrelated);
        for projection in [names, KeyConstraintNames { names: None }] {
            let (projected, changed) = projection.project(&catalog).unwrap();
            assert!(changed.is_empty());
            assert!(std::ptr::eq(catalog.snapshot(), projected.snapshot()));
        }
    }
}

#[test]
fn changed_key_names_copy_only_changed_constraint_collections() {
    for (keys, inherited) in [(true, false), (false, true), (true, true)] {
        let (catalog, names) = catalog(256);
        let relation = RelationIdentity::new("public", "t");
        let mut snapshot = catalog.snapshot().clone();
        let table = snapshot.tables.get_mut(&relation).unwrap();
        if keys {
            Arc::make_mut(&mut table.keys)[0].name = Some("old".into());
        }
        if inherited {
            Arc::make_mut(&mut table.hierarchy).partition_inherited_key_constraints[0].name =
                Some("old".into());
        }
        let catalog = CatalogReadView::new(snapshot);
        let (projected, changed) = names.project(&catalog).unwrap();
        assert_eq!(changed, BTreeSet::from([relation.clone()]));
        for (name, before) in &catalog.snapshot().tables {
            let after = &projected.snapshot().tables[name];
            assert!(Arc::ptr_eq(&before.columns, &after.columns));
            assert!(Arc::ptr_eq(&before.checks, &after.checks));
            assert_eq!(
                Arc::ptr_eq(&before.keys, &after.keys),
                name != &relation || !keys
            );
            assert_eq!(
                Arc::ptr_eq(&before.hierarchy, &after.hierarchy),
                name != &relation || !inherited
            );
        }
        let before = &catalog.snapshot().tables[&relation];
        let after = &projected.snapshot().tables[&relation];
        assert_eq!(
            before.keys[0].name.as_deref(),
            Some(if keys { "old" } else { "current" })
        );
        assert_eq!(
            before.hierarchy.partition_inherited_key_constraints[0]
                .name
                .as_deref(),
            Some(if inherited { "old" } else { "current" })
        );
        assert_eq!(after.keys[0].name.as_deref(), Some("current"));
        assert_eq!(
            after.hierarchy.partition_inherited_key_constraints[0]
                .name
                .as_deref(),
            Some("current")
        );
        assert_eq!(
            after.keys[0].catalog_identity,
            before.keys[0].catalog_identity
        );
        assert_eq!(after.keys[0].columns, before.keys[0].columns);
    }
}

#[test]
fn unchanged_key_names_still_validate_owners_and_skip_temporary_tables() {
    for corruption in 0..4 {
        let (catalog, mut names) = catalog(1);
        let mut snapshot = catalog.snapshot().clone();
        let relation = RelationIdentity::new("public", "t");
        match corruption {
            0 => {
                Arc::make_mut(&mut snapshot.tables.get_mut(&relation).unwrap().keys)[0]
                    .catalog_identity = None
            }
            1 => names.names.as_mut().unwrap().clear(),
            2 => {
                names
                    .names
                    .as_mut()
                    .unwrap()
                    .get_mut(&[3; 16])
                    .unwrap()
                    .table
                    .name = "other".into()
            }
            _ => {
                names
                    .names
                    .as_mut()
                    .unwrap()
                    .get_mut(&[3; 16])
                    .unwrap()
                    .table_object_id = [9; 16]
            }
        }
        let invalid = CatalogReadView::new(snapshot.clone());
        assert!(names.project(&invalid).is_err());
        snapshot.tables.get_mut(&relation).unwrap().persistence =
            uqa_sql::ast::RelationPersistence::Temporary;
        let temporary = CatalogReadView::new(snapshot);
        let (projected, changed) = names.project(&temporary).unwrap();
        assert!(changed.is_empty());
        assert!(std::ptr::eq(temporary.snapshot(), projected.snapshot()));
    }
}

#[test]
fn invalid_inherited_owner_does_not_publish_an_earlier_name_change() {
    let (catalog, names) = catalog(0);
    let mut snapshot = catalog.snapshot().clone();
    let relation = RelationIdentity::new("public", "t");
    let table = snapshot.tables.get_mut(&relation).unwrap();
    Arc::make_mut(&mut table.keys)[0].name = Some("old".into());
    Arc::make_mut(&mut table.hierarchy).partition_inherited_key_constraints[0].catalog_identity =
        None;
    let catalog = CatalogReadView::new(snapshot);
    assert!(names.project(&catalog).is_err());
    assert_eq!(
        catalog.snapshot().tables[&relation].keys[0].name.as_deref(),
        Some("old")
    );
}
