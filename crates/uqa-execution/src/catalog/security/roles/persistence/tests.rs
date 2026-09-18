//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::{KeyValueCatalog, MemoryKeyValueStore};

#[test]
fn role_metadata_restoration_preserves_legacy_values_and_round_trips_membership_keys() {
    let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    let initial = restore(&catalog).unwrap();
    assert_eq!(initial.roles.len(), 1);
    assert_eq!(initial.roles["uqa"], RoleDefinition::bootstrap());
    assert!(initial.memberships.is_empty());
    catalog
        .set_metadata(
            "sql_roles_json",
            r#"{"reader":{"oid":20001,"name":"reader","attributes":[],"connection_limit":-1}}"#,
        )
        .unwrap();
    catalog.set_metadata("sql_role_memberships_json", r#"[{"oid":2500000001,"role":"reader","member":"uqa","grantor":"uqa","admin_option":true,"inherit_option":false,"set_option":true}]"#).unwrap();
    let restored = restore(&catalog).unwrap();
    assert_eq!(restored.roles["reader"].oid, 20_001);
    assert_eq!(restored.roles["uqa"].oid, 10);
    let entry = restored.memberships.values().next().unwrap();
    assert_eq!(entry.oid, 2_500_000_001);
    assert!(entry.admin_option && entry.set_option && !entry.inherit_option);
    assert_eq!(restored.memberships.get(&entry.key()), Some(entry));
    let destination = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    persist_roles(Some(&destination), &restored.roles).unwrap();
    persist_memberships(Some(&destination), &restored.memberships).unwrap();
    let reopened = restore(&destination).unwrap();
    assert_eq!(reopened.roles, restored.roles);
    assert_eq!(reopened.memberships, restored.memberships);
}

#[test]
fn role_metadata_restoration_validates_definitions_before_loading_memberships() {
    let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    catalog
        .set_metadata(
            "sql_roles_json",
            r#"{"wrong":{"oid":20001,"name":"reader","attributes":[],"connection_limit":-1}}"#,
        )
        .unwrap();
    catalog
        .set_metadata("sql_role_memberships_json", "{")
        .unwrap();
    assert!(
        matches!(restore(&catalog), Err(StorageBackendError::Other(message)) if message == "persisted role key `wrong` does not match role name `reader`")
    );
    persist_roles(Some(&catalog), &BTreeMap::new()).unwrap();
    assert!(restore(&catalog).is_err());
    catalog.set_metadata("sql_role_memberships_json", r#"[{"oid":2500000001,"role":"missing","member":"uqa","grantor":"uqa","admin_option":false,"inherit_option":true,"set_option":true}]"#).unwrap();
    assert!(
        matches!(restore(&catalog), Err(StorageBackendError::Other(message)) if message.contains("missing role or grantor"))
    );
}

#[test]
fn unchanged_role_metadata_uses_the_committed_catalog_instead_of_stale_session_values() {
    let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    let values = restore(&catalog).unwrap();
    let current = RoleCatalogSnapshot {
        roles: Arc::new(values.roles),
        memberships: Arc::new(values.memberships),
    };
    let mut roles = (*current.roles).clone();
    let mut extra = RoleDefinition::bootstrap();
    extra.name = "committed".into();
    extra.oid = 20_001;
    roles.insert(extra.name.clone(), extra);
    persist_roles(Some(&catalog), &roles).unwrap();
    let values = restore(&catalog).unwrap();
    let latest = RoleCatalogSnapshot {
        roles: Arc::new(values.roles),
        memberships: Arc::new(values.memberships),
    }
    .merge_private(Some(&catalog), &current)
    .unwrap();
    assert!(latest.roles.contains_key("committed"));
    assert!(!current.roles.contains_key("committed"));
}
