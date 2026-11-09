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
