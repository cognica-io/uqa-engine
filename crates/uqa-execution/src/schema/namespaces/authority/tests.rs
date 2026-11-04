//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::catalog::roles::RoleDefinition;
use uqa_sql::catalog::roles::RoleDefinitions;
use uqa_sql::catalog::security::schema_registry::SchemaRegistry;
use uqa_sql::catalog::security::BoundSchemaSecurity;

fn schemas(names: &[&str]) -> SchemaRegistry {
    SchemaRegistry::try_from_iter(
        names
            .iter()
            .map(|name| ((*name).into(), BoundSchemaSecurity::bootstrap(name))),
    )
    .unwrap()
}

#[test]
fn private_schema_authority_preserves_deletions_and_uses_fresh_unmodified_records() {
    let mut current = schemas(&["private", "changed", "untouched"]);
    current
        .edit("changed", |value| {
            value.acl = Some(Vec::new());
            Ok(())
        })
        .unwrap()
        .unwrap();
    let committed = Arc::new(schemas(&["changed", "removed", "peer"]));
    let roles =
        RoleDefinitions::try_from_iter([("uqa".into(), RoleDefinition::bootstrap())]).unwrap();
    let merged = merge_private_records(&current, committed.clone(), &roles, |name| {
        Ok(matches!(name, "private" | "changed" | "removed"))
    })
    .unwrap();
    assert_eq!(
        merged.keys().map(String::as_str).collect::<Vec<_>>(),
        ["changed", "peer", "private"]
    );
    assert_eq!(merged["changed"].acl, Some(Vec::new()));
    assert_eq!(committed["changed"].acl, None);
    assert!(committed.contains_key("removed"));
}

#[test]
fn private_schema_authority_validates_role_incarnations_and_provenance_errors() {
    let roles =
        RoleDefinitions::try_from_iter([("uqa".into(), RoleDefinition::bootstrap())]).unwrap();
    let mut current = schemas(&["private"]);
    current
        .edit("private", |value| {
            value.role_owner.object_id = [9; 16];
            Ok(())
        })
        .unwrap()
        .unwrap();
    let committed = Arc::new(schemas(&["peer"]));
    assert!(merge_private_records(&current, committed.clone(), &roles, |_| Ok(true)).is_err());
    assert!(
        merge_private_records(&current, committed.clone(), &roles, |_| {
            Err(StorageBackendError::Other("provenance unavailable".into()))
        })
        .unwrap_err()
        .to_string()
        .contains("provenance unavailable")
    );
    let merged = merge_private_records(&current, committed.clone(), &roles, |_| Ok(false)).unwrap();
    assert!(Arc::ptr_eq(&merged, &committed));
}
