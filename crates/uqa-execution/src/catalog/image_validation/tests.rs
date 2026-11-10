//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::test_support::{empty_catalog, table_snapshot};
use uqa_core::{memory::RetainedValue, RelationIdentity};
use uqa_sql::{
    ast::{ColumnDef, TableConstraintSet},
    catalog::{
        roles::{RoleDefinition, RoleDefinitions},
        security::{BoundSchemaSecurity, BoundTableSecurity},
    },
};

fn snapshot() -> CatalogReadSnapshot {
    let mut snapshot = empty_catalog().snapshot().clone();
    snapshot.definitions.roles = RetainedValue::new(RoleDefinitions::initial_catalog());
    snapshot.definitions.schemas = RetainedValue::new(BoundSchemaSecurity::initial_catalog());
    snapshot
}

#[test]
fn native_catalog_authority_rejects_missing_and_recreated_table_owners() {
    let mut snapshot = snapshot();
    let memory = MemoryBudget::new(64 << 10);
    let cancellation = CancellationToken::new();
    let mut role = RoleDefinition::bootstrap();
    role.oid = 20000;
    role.object_id = [20; 16];
    role.name = "owner".into();
    RetainedValue::make_mut(&mut snapshot.definitions.roles)
        .insert(role.name.clone(), role.clone())
        .unwrap();
    let mut table = table_snapshot(
        [1; 16],
        vec![ColumnDef::nullable("value", uqa_sql::ColumnType::Integer)],
        TableConstraintSet::default(),
    );
    table.security = std::sync::Arc::new(BoundTableSecurity::owner(role.identity())).into();
    snapshot
        .tables
        .insert(RelationIdentity::new("public", "items"), table);
    validate_authority(&snapshot, &memory, &cancellation).unwrap();
    RetainedValue::make_mut(&mut snapshot.definitions.roles)
        .remove("owner")
        .unwrap();
    assert!(validate_authority(&snapshot, &memory, &cancellation).is_err());
    role.object_id = [21; 16];
    RetainedValue::make_mut(&mut snapshot.definitions.roles)
        .insert(role.name.clone(), role)
        .unwrap();
    assert!(validate_authority(&snapshot, &memory, &cancellation).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_catalog_authority_validation_is_read_only_and_cancellation_releases_workspace() {
    let snapshot = snapshot();
    let memory = MemoryBudget::new(64 << 10);
    let cancellation = CancellationToken::new();
    validate_authority(&snapshot, &memory, &cancellation).unwrap();
    assert_eq!(memory.used(), 0);
    assert_eq!(snapshot.definitions.roles.len(), 1);
    cancellation.cancel();
    assert!(validate_authority(&snapshot, &memory, &cancellation).is_err());
    assert_eq!(memory.used(), 0);
}

fn typed_snapshot() -> CatalogReadSnapshot {
    let mut snapshot = snapshot();
    let definition = uqa_sql::catalog::enum_type::StoredEnum {
        object_id: [7; 16],
        oid: 25_000,
        array_oid: 25_001,
        array_name: "_mood".into(),
        identity: RelationIdentity::new("public", "mood"),
        owner: uqa_sql::catalog::roles::RoleIdentity::BOOTSTRAP,
        labels: Vec::new(),
        usage_acl: None,
    };
    let table = table_snapshot(
        [8; 16],
        vec![ColumnDef::nullable("value", definition.column_type())],
        TableConstraintSet::default(),
    );
    RetainedValue::make_mut(&mut snapshot.definitions.enums)
        .insert("public.mood".into(), definition)
        .unwrap();
    snapshot
        .tables
        .insert(RelationIdentity::new("public", "items"), table);
    snapshot
}

#[test]
fn native_catalog_types_reject_missing_and_replaced_column_type_identities() {
    let mut snapshot = typed_snapshot();
    let memory = MemoryBudget::new(64 << 10);
    let cancellation = CancellationToken::new();
    validate_types(&snapshot, &memory, &cancellation).unwrap();
    let mut definition = snapshot
        .definitions
        .enums
        .get("public.mood")
        .unwrap()
        .clone();
    RetainedValue::make_mut(&mut snapshot.definitions.enums)
        .remove("public.mood")
        .unwrap();
    assert!(validate_types(&snapshot, &memory, &cancellation).is_err());
    definition.oid += 2;
    definition.array_oid += 2;
    RetainedValue::make_mut(&mut snapshot.definitions.enums)
        .insert("public.mood".into(), definition)
        .unwrap();
    assert!(validate_types(&snapshot, &memory, &cancellation).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_catalog_types_reject_cross_owner_row_type_collisions() {
    let mut snapshot = typed_snapshot();
    let memory = MemoryBudget::new(64 << 10);
    let cancellation = CancellationToken::new();
    let table = snapshot
        .tables
        .get_mut(&RelationIdentity::new("public", "items"))
        .unwrap();
    table.catalog_oids.row_type = Some(25_001);
    assert!(validate_types(&snapshot, &memory, &cancellation).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_catalog_types_retire_workspace_on_quota_denial_and_cancellation() {
    let snapshot = typed_snapshot();
    let cancellation = CancellationToken::new();
    let empty = MemoryBudget::new(0);
    assert!(validate_types(&snapshot, &empty, &cancellation).is_err());
    let memory = MemoryBudget::new(64 << 10);
    cancellation.cancel();
    assert!(validate_types(&snapshot, &memory, &cancellation).is_err());
    assert_eq!(memory.used(), 0);
}
