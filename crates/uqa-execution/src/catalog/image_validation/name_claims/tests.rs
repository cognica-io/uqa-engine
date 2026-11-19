//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::test_support::{empty_catalog, table_snapshot};
use uqa_core::{memory::RetainedValue, RelationIdentity};
use uqa_sql::{
    ast::TableConstraintSet,
    catalog::{enum_type::StoredEnum, roles::RoleIdentity},
};

fn snapshot(array_name: &str) -> CatalogReadSnapshot {
    let mut snapshot = empty_catalog().snapshot().clone();
    snapshot.tables.insert(
        RelationIdentity::new("public", "items"),
        table_snapshot([1; 16], vec![], TableConstraintSet::default()),
    );
    RetainedValue::make_mut(&mut snapshot.definitions.enums)
        .insert(
            "public.mood".into(),
            StoredEnum {
                object_id: [2; 16],
                oid: 20_000,
                array_oid: 20_001,
                array_name: array_name.into(),
                identity: RelationIdentity::new("public", "mood"),
                owner: RoleIdentity::BOOTSTRAP,
                labels: vec![],
                usage_acl: None,
            },
        )
        .unwrap();
    snapshot
}

#[test]
fn native_catalog_names_join_declared_arrays_with_row_type_names() {
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    for (array_name, valid) in [("_mood", true), ("items", false), ("_items", false)] {
        assert_eq!(
            validate(&snapshot(array_name), &memory, &token).is_ok(),
            valid,
            "{array_name}"
        );
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn native_catalog_names_keep_recorded_row_array_names_and_release_workspace() {
    let mut snapshot = snapshot("_items");
    snapshot
        .tables
        .get_mut(&RelationIdentity::new("public", "items"))
        .unwrap()
        .row_type_array_name = Some("_items_1".into()).into();
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    validate(&snapshot, &memory, &token).unwrap();
    assert_eq!(memory.used(), 0);
    let empty = MemoryBudget::new(0);
    assert!(validate(&snapshot, &empty, &token).is_err());
    assert_eq!(empty.used(), 0);
    token.cancel();
    assert!(validate(&snapshot, &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
}

#[derive(Clone, Copy)]
enum LateDamage {
    Namespace,
    ArrayName,
}

fn large_catalog(count: u32, damage: LateDamage) -> CatalogReadView {
    use uqa_sql::catalog::security::BoundSchemaSecurity;
    let mut snapshot = empty_catalog().snapshot().clone();
    snapshot.definitions.schemas = RetainedValue::new(BoundSchemaSecurity::initial_catalog());
    for position in 1..=count {
        let name = format!("m{position:05}");
        let last = position == count;
        let schema = if last && matches!(damage, LateDamage::Namespace) {
            "zz_missing"
        } else {
            "public"
        };
        let array = if last && matches!(damage, LateDamage::ArrayName) {
            "_m00001".to_owned()
        } else {
            format!("_{name}")
        };
        RetainedValue::make_mut(&mut snapshot.definitions.enums)
            .insert(
                format!("{schema}.{name}"),
                StoredEnum {
                    object_id: u128::from(position).to_be_bytes(),
                    oid: 20000 + position * 2,
                    array_oid: 20001 + position * 2,
                    array_name: array,
                    identity: RelationIdentity::new(schema, name),
                    owner: RoleIdentity::BOOTSTRAP,
                    labels: vec![],
                    usage_acl: None,
                },
            )
            .unwrap();
    }
    CatalogReadView::new(snapshot)
}

fn failure_at(catalog: &CatalogReadView, expected: &str, window: usize) -> usize {
    use crate::catalog::{image_validation::RelationshipValidation, test_support::NoRoutines};
    let memory = MemoryBudget::new(2 << 20);
    let cancel = CancellationToken::new();
    let mut check = RelationshipValidation::new(catalog.clone(), &memory, &cancel);
    for position in 1..200_000 {
        match check.advance_with_steps(&NoRoutines, &NoRoutines, window) {
            Ok(Some(next)) => check = next,
            Ok(None) => panic!("accepted a damaged native catalog"),
            Err(error) => {
                assert_eq!(error.to_string(), expected);
                drop(error);
                assert_eq!(memory.used(), 0);
                return position;
            }
        }
    }
    panic!("catalog validation made no progress")
}

#[test]
fn late_namespaces_and_array_collisions_yield_in_order_and_retire_their_private_claims() {
    use crate::catalog::{image_validation::RelationshipValidation, test_support::NoRoutines};
    for (damage, expected) in [
        (
            LateDamage::Namespace,
            "internal error: missing native catalog namespace",
        ),
        (
            LateDamage::ArrayName,
            "internal error: invalid native catalog name: duplicate declaration",
        ),
    ] {
        let small = failure_at(&large_catalog(12, damage), expected, 1);
        let catalog = large_catalog(72, damage);
        let single = failure_at(&catalog, expected, 1);
        let batched = failure_at(&catalog, expected, 17);
        assert!(single >= small + 60);
        assert!(batched < single);
        for cancel in [false, true] {
            let memory = MemoryBudget::new(2 << 20);
            let token = CancellationToken::new();
            let mut check = RelationshipValidation::new(catalog.clone(), &memory, &token);
            for _ in 0..single / 2 {
                check = check.advance(&NoRoutines, &NoRoutines).unwrap().unwrap();
            }
            assert!(memory.used() > 0);
            if cancel {
                token.cancel();
                assert!(matches!(
                    check.advance(&NoRoutines, &NoRoutines),
                    Err(SQLError::Cancelled(_))
                ));
            } else {
                drop(check);
            }
            assert_eq!(memory.used(), 0);
        }
    }
}
