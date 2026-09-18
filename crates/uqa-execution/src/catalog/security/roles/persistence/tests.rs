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
    let initial = restore_and_migrate(&catalog).unwrap();
    assert_eq!(initial.roles.len(), 1);
    assert_eq!(initial.roles["uqa"], RoleDefinition::bootstrap());
    assert!(initial.memberships.is_empty());
    let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    catalog
        .set_metadata(
            "sql_roles_json",
            r#"{"reader":{"oid":20001,"name":"reader","attributes":[],"connection_limit":-1}}"#,
        )
        .unwrap();
    catalog.set_metadata("sql_role_memberships_json", r#"[{"oid":2500000001,"role":"reader","member":"uqa","grantor":"uqa","admin_option":true,"inherit_option":false,"set_option":true}]"#).unwrap();
    let restored = restore_and_migrate(&catalog).unwrap();
    assert_eq!(restored.roles["reader"].oid, 20_001);
    assert_eq!(restored.roles["uqa"].oid, 10);
    let entry = restored.memberships.values().next().unwrap();
    assert_eq!(entry.oid, 2_500_000_001);
    assert!(entry.admin_option && entry.set_option && !entry.inherit_option);
    assert_eq!(restored.memberships.get(&entry.key()), Some(entry));
    assert!(serde_json::from_str::<BTreeMap<String, RoleDefinition>>(
        &catalog.get_metadata(ROLES_METADATA_KEY).unwrap().unwrap()
    )
    .is_err());
    let destination = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    let initial = restore_and_migrate(&destination).unwrap();
    persist_roles(Some(&destination), &initial.roles, &restored.roles).unwrap();
    persist_memberships(Some(&destination), &restored.memberships).unwrap();
    let reopened = restore(&destination).unwrap();
    assert_eq!(reopened.roles, restored.roles);
    assert_eq!(reopened.memberships, restored.memberships);
}

#[test]
fn role_definition_records_gain_stable_incarnations_only_during_initial_open() {
    let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    let mut roles = restore_and_migrate(&catalog).unwrap().roles;
    let mut role = RoleDefinition::bootstrap();
    role.name = "legacy".into();
    role.oid = 20_001;
    role.object_id = [1; 16];
    roles.insert(role.name.clone(), role);
    for (name, role) in &roles {
        let mut value = serde_json::to_value(role).unwrap();
        value.as_object_mut().unwrap().remove("object_id");
        catalog
            .set_metadata(&records::role_key(name), &value.to_string())
            .unwrap();
        catalog
            .set_metadata(&format!("uqa.sql.role_oid.v1:{}", role.oid), name)
            .unwrap();
    }
    catalog
        .set_metadata(ROLES_METADATA_KEY, r#"{"role_catalog_format":1}"#)
        .unwrap();
    assert!(restore(&catalog)
        .err()
        .unwrap()
        .to_string()
        .contains("initial-open record migration"));
    let initial = restore_and_migrate(&catalog).unwrap();
    assert_eq!(initial.roles["legacy"].oid, 20_001);
    assert_ne!(initial.roles["legacy"].object_id, [0; 16]);
    assert_eq!(initial.roles["uqa"], RoleDefinition::bootstrap());
    assert_eq!(restore(&catalog).unwrap().roles, initial.roles);
    catalog
        .set_metadata(
            "uqa.sql.role.v1:legacy",
            r#"{"oid":20001,"name":"legacy","attributes":[],"connection_limit":-1}"#,
        )
        .unwrap();
    assert!(restore_and_migrate(&catalog)
        .err()
        .unwrap()
        .to_string()
        .contains("has no object identity"));
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
    catalog.set_metadata(ROLES_METADATA_KEY, "{}").unwrap();
    assert!(restore(&catalog).is_err());
    catalog.set_metadata("sql_role_memberships_json", r#"[{"oid":2500000001,"role":"missing","member":"uqa","grantor":"uqa","admin_option":false,"inherit_option":true,"set_option":true}]"#).unwrap();
    assert!(
        matches!(restore(&catalog), Err(StorageBackendError::Other(message)) if message.contains("missing role or grantor"))
    );
}

#[test]
fn unchanged_role_metadata_uses_the_committed_catalog_instead_of_stale_session_values() {
    let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    let values = restore_and_migrate(&catalog).unwrap();
    let current = RoleCatalogSnapshot {
        roles: Arc::new(values.roles),
        memberships: Arc::new(values.memberships),
    };
    let mut roles = (*current.roles).clone();
    let mut extra = RoleDefinition::bootstrap();
    extra.name = "committed".into();
    extra.oid = 20_001;
    extra.object_id = [1; 16];
    roles.insert(extra.name.clone(), extra);
    persist_roles(Some(&catalog), &current.roles, &roles).unwrap();
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

#[test]
fn role_migration_validates_memberships_before_replacing_legacy_metadata() {
    let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    catalog.set_metadata(ROLES_METADATA_KEY, "{}").unwrap();
    catalog
        .set_metadata(ROLE_MEMBERSHIPS_METADATA_KEY, "{")
        .unwrap();
    assert!(restore_and_migrate(&catalog).is_err());
    assert_eq!(
        catalog.get_metadata(ROLES_METADATA_KEY).unwrap().as_deref(),
        Some("{}")
    );
    assert!(catalog
        .metadata_with_prefix(records::ROLE_PREFIX)
        .unwrap()
        .is_empty());
}

#[test]
fn read_only_role_restoration_requires_completed_initial_migration() {
    let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    catalog.set_metadata(ROLES_METADATA_KEY, "{}").unwrap();
    assert!(restore(&catalog)
        .err()
        .unwrap()
        .to_string()
        .contains("initial-open record migration"));
    assert_eq!(
        catalog.get_metadata(ROLES_METADATA_KEY).unwrap().as_deref(),
        Some("{}")
    );
    assert!(catalog
        .metadata_with_prefix(records::ROLE_PREFIX)
        .unwrap()
        .is_empty());
}

#[test]
fn role_records_validate_format_name_and_oid_ownership() {
    for corruption in 0..6 {
        let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
        restore_and_migrate(&catalog).unwrap();
        match corruption {
            0 => catalog.set_metadata(ROLES_METADATA_KEY, r#"{"role_catalog_format":3}"#),
            1 => catalog.delete_metadata("uqa.sql.role_oid.v1:10"),
            2 => catalog.set_metadata("uqa.sql.role_oid.v1:10", "absent"),
            3 => catalog.set_metadata(
                "uqa.sql.role.v1:other",
                &serde_json::to_string(&RoleDefinition::bootstrap()).unwrap(),
            ),
            4 => catalog.set_metadata(ROLES_METADATA_KEY, "{}"),
            5 => catalog.delete_metadata("uqa.sql.role.v1:uqa"),
            _ => unreachable!(),
        }
        .unwrap();
        assert!(restore(&catalog).is_err(), "corruption {corruption}");
    }
}

#[test]
fn role_records_keep_literal_names_and_reject_reassigned_oids_before_writing() {
    let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    let before = restore_and_migrate(&catalog).unwrap().roles;
    let mut after = before.clone();
    for (offset, name) in ["role_catalog_format", "a:%_日本語", "a:%_日本語:suffix"]
        .into_iter()
        .enumerate()
    {
        let mut role = RoleDefinition::bootstrap();
        role.name = name.into();
        role.oid = 20_001 + offset as i64;
        role.object_id = [offset as u8 + 1; 16];
        after.insert(name.into(), role);
    }
    persist_roles(Some(&catalog), &before, &after).unwrap();
    assert_eq!(restore(&catalog).unwrap().roles, after);
    let mut collision = after.clone();
    let mut role = after["a:%_日本語"].clone();
    role.name = "collision".into();
    collision.insert(role.name.clone(), role);
    assert_eq!(
        persist_roles(Some(&catalog), &after, &collision)
            .unwrap_err()
            .sqlstate(),
        Some("23505")
    );
    assert_eq!(restore(&catalog).unwrap().roles, after);
    let mut same_candidate = after.clone();
    for name in ["fresh_one", "fresh_two"] {
        let mut role = RoleDefinition::bootstrap();
        role.name = name.into();
        role.oid = 30_000;
        same_candidate.insert(name.into(), role);
    }
    assert_eq!(
        persist_roles(Some(&catalog), &after, &same_candidate)
            .unwrap_err()
            .sqlstate(),
        Some("23505")
    );
    assert_eq!(restore(&catalog).unwrap().roles, after);
    let mut deleted = after.clone();
    deleted.remove("a:%_日本語");
    persist_roles(Some(&catalog), &after, &deleted).unwrap();
    assert_eq!(restore(&catalog).unwrap().roles, deleted);
    assert!(catalog
        .get_metadata("uqa.sql.role.v1:a:%_日本語")
        .unwrap()
        .is_none());
    assert!(catalog
        .get_metadata("uqa.sql.role_oid.v1:20002")
        .unwrap()
        .is_none());
}
