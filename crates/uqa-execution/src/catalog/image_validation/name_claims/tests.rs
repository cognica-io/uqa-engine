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
        .row_type_array_name = Some("_items_1".into());
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
