//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_sql::catalog::security::database::{DatabaseAclEntry, DatabasePrivileges};
use uqa_storage::{KeyValueCatalog, MemoryKeyValueStore};

fn fixture() -> (
    KeyValueCatalog,
    BTreeMap<String, RoleDefinition>,
    DatabaseSecurity,
) {
    let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    let bootstrap = RoleDefinition::bootstrap();
    let mut reader = bootstrap.clone();
    reader.name = "reader".into();
    reader.oid = 20_001;
    reader.object_id = [1; 16];
    reader.attributes.clear();
    let roles = BTreeMap::from([
        (bootstrap.name.clone(), bootstrap),
        (reader.name.clone(), reader),
    ]);
    let security = DatabaseSecurity {
        role_owner: "uqa".into(),
        acl: Some(vec![DatabaseAclEntry {
            role: "reader".into(),
            grantor: None,
            privileges: DatabasePrivileges {
                create: true,
                ..DatabasePrivileges::default()
            },
            grant_options: DatabasePrivileges::default(),
        }]),
    };
    (catalog, roles, security)
}

#[test]
fn legacy_database_acl_is_bound_once_during_initial_restore() {
    let (catalog, roles, legacy) = fixture();
    let before = serde_json::to_string(&legacy).unwrap();
    catalog
        .set_metadata(DATABASE_SECURITY_METADATA_KEY, &before)
        .unwrap();
    assert!(restore(&catalog, &roles, false)
        .unwrap_err()
        .to_string()
        .contains("initial catalog migration"));
    assert_eq!(
        catalog
            .get_metadata(DATABASE_SECURITY_METADATA_KEY)
            .unwrap()
            .as_deref(),
        Some(before.as_str())
    );
    let restored = restore(&catalog, &roles, true).unwrap();
    let names = restored.resolve(&roles).unwrap();
    let acl = names.acl.as_ref().unwrap();
    assert_eq!(
        acl[0].role,
        uqa_core::catalog_acl::AclGrantee::from("reader")
    );
    assert_eq!(acl[0].grantor.as_deref(), Some("uqa"));
    assert!(acl[0].privileges.create);
    let converted = catalog
        .get_metadata(DATABASE_SECURITY_METADATA_KEY)
        .unwrap()
        .unwrap();
    assert!(serde_json::from_str::<DatabaseSecurity>(&converted).is_err());
    assert_eq!(restore(&catalog, &roles, false).unwrap(), restored);
    assert_eq!(restore(&catalog, &roles, true).unwrap(), restored);
    assert_eq!(
        catalog
            .get_metadata(DATABASE_SECURITY_METADATA_KEY)
            .unwrap()
            .unwrap(),
        converted
    );
}

#[test]
fn restoring_renamed_database_roles_does_not_rewrite_metadata_or_rebind_names() {
    let (catalog, mut roles, security) = fixture();
    let stored = encode(
        &BoundDatabaseSecurity::bind(&security, &roles).unwrap(),
        &roles,
    )
    .unwrap();
    catalog
        .set_metadata(DATABASE_SECURITY_METADATA_KEY, &stored)
        .unwrap();
    let mut renamed = roles.remove("reader").unwrap();
    renamed.name = "renamed".into();
    let mut replacement = renamed.clone();
    replacement.name = "reader".into();
    replacement.oid += 1;
    replacement.object_id = [2; 16];
    roles.insert("reader".into(), replacement);
    roles.insert("renamed".into(), renamed);
    assert_eq!(
        restore(&catalog, &roles, false)
            .unwrap()
            .resolve(&roles)
            .unwrap()
            .acl
            .unwrap()[0]
            .role,
        uqa_core::catalog_acl::AclGrantee::from("renamed")
    );
    assert_eq!(
        catalog
            .get_metadata(DATABASE_SECURITY_METADATA_KEY)
            .unwrap()
            .unwrap(),
        stored
    );
    roles.get_mut("renamed").unwrap().object_id = [3; 16];
    assert!(restore(&catalog, &roles, true)
        .unwrap_err()
        .to_string()
        .contains("missing role incarnation"));
    assert_eq!(
        catalog
            .get_metadata(DATABASE_SECURITY_METADATA_KEY)
            .unwrap()
            .unwrap(),
        stored
    );
}

#[test]
fn corrupt_database_security_never_falls_back_to_legacy_name_binding() {
    let (catalog, roles, security) = fixture();
    let encoded = encode(
        &BoundDatabaseSecurity::bind(&security, &roles).unwrap(),
        &roles,
    )
    .unwrap();
    let original: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    let mut candidates = Vec::new();
    let mut unknown = original.clone();
    unknown["database_security_format"] = serde_json::json!(2);
    candidates.push(unknown);
    let mut malformed = original.clone();
    malformed["database_security_format"] = serde_json::json!("1");
    malformed["role_owner"] = serde_json::json!("uqa");
    malformed["acl"] = serde_json::Value::Null;
    candidates.push(malformed);
    for (field, value) in [
        ("oid", serde_json::json!(0)),
        ("object_id", serde_json::json!(vec![0; 16])),
    ] {
        let mut invalid = original.clone();
        invalid["security"]["role_owner"][field] = value;
        candidates.push(invalid);
    }
    let mut missing_grantee = original;
    missing_grantee["security"]["acl"][0]["role"]["object_id"] = serde_json::json!(vec![9; 16]);
    candidates.push(missing_grantee);
    for candidate in candidates {
        let json = candidate.to_string();
        catalog
            .set_metadata(DATABASE_SECURITY_METADATA_KEY, &json)
            .unwrap();
        for migration in [false, true] {
            assert!(restore(&catalog, &roles, migration).is_err());
            assert_eq!(
                catalog
                    .get_metadata(DATABASE_SECURITY_METADATA_KEY)
                    .unwrap()
                    .as_deref(),
                Some(json.as_str())
            );
        }
    }
}

#[test]
fn initial_default_security_and_invalid_legacy_references_preserve_atomic_validation() {
    let (catalog, mut roles, security) = fixture();
    assert!(restore(&catalog, &roles, false).is_err());
    assert!(catalog
        .get_metadata(DATABASE_SECURITY_METADATA_KEY)
        .unwrap()
        .is_none());
    let default = restore(&catalog, &roles, true).unwrap();
    assert_eq!(default, BoundDatabaseSecurity::bootstrap());
    let encoded = catalog
        .get_metadata(DATABASE_SECURITY_METADATA_KEY)
        .unwrap()
        .unwrap();
    assert_eq!(restore(&catalog, &roles, false).unwrap(), default);
    assert_eq!(
        catalog
            .get_metadata(DATABASE_SECURITY_METADATA_KEY)
            .unwrap()
            .unwrap(),
        encoded
    );
    let mut invalid_owner = security.clone();
    invalid_owner.role_owner = "missing".into();
    let mut invalid_grantee = security.clone();
    invalid_grantee.acl.as_mut().unwrap()[0].role = "missing".into();
    let mut invalid_grantor = security;
    invalid_grantor.acl.as_mut().unwrap()[0].role = uqa_core::catalog_acl::AclGrantee::Public;
    invalid_grantor.acl.as_mut().unwrap()[0].grantor = Some("missing".into());
    for (invalid, expected) in [
        (
            invalid_owner,
            "persisted database owner `missing` does not exist",
        ),
        (
            invalid_grantee,
            "persisted database ACL `missing` from `uqa` references a missing role",
        ),
        (
            invalid_grantor,
            "persisted database ACL `PUBLIC` from `missing` references a missing role",
        ),
    ] {
        let invalid = serde_json::to_string(&invalid).unwrap();
        catalog
            .set_metadata(DATABASE_SECURITY_METADATA_KEY, &invalid)
            .unwrap();
        let error = restore(&catalog, &roles, true).unwrap_err();
        assert!(matches!(error, StorageBackendError::Other(message) if message == expected));
        assert_eq!(
            catalog
                .get_metadata(DATABASE_SECURITY_METADATA_KEY)
                .unwrap()
                .as_deref(),
            Some(invalid.as_str())
        );
    }
    let legacy = serde_json::to_string(&DatabaseSecurity::bootstrap()).unwrap();
    catalog
        .set_metadata(DATABASE_SECURITY_METADATA_KEY, &legacy)
        .unwrap();
    roles.get_mut("uqa").unwrap().object_id = [0; 16];
    assert!(restore(&catalog, &roles, true).is_err());
    assert_eq!(
        catalog
            .get_metadata(DATABASE_SECURITY_METADATA_KEY)
            .unwrap()
            .as_deref(),
        Some(legacy.as_str())
    );
}

#[test]
fn missing_database_metadata_uses_the_bootstrap_identity_independently_of_its_name() {
    let (catalog, mut roles, _) = fixture();
    let mut bootstrap = roles.remove("uqa").unwrap();
    bootstrap.name = "renamed_bootstrap".into();
    roles.insert(bootstrap.name.clone(), bootstrap);
    let mut replacement = roles["reader"].clone();
    replacement.name = "uqa".into();
    roles.remove("reader");
    roles.insert("uqa".into(), replacement);
    let bound = restore(&catalog, &roles, true).unwrap();
    assert_eq!(bound, BoundDatabaseSecurity::bootstrap());
    assert_eq!(
        bound.resolve(&roles).unwrap().role_owner,
        "renamed_bootstrap"
    );
    assert!(!bound.depends_on(roles["uqa"].identity()));
}
