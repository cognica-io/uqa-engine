//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_sql::catalog::security::{
    columns::grant_column_acl,
    system_relations::security,
    table::{grant_acl, TableAclPrivilege},
    BoundTableSecurity,
};
use uqa_storage::{KeyValueCatalog, MemoryKeyValueStore};

#[test]
fn system_acl_restoration_round_trips_independent_table_and_attribute_tuples() {
    let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    let relation = SystemRelation::Projected(uqa_sql::catalog::VirtualRelation::PgAuthid);
    let roles = BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]);
    let mut expected = relation.bootstrap_security().resolve(&roles).unwrap();
    grant_acl(
        &mut expected,
        TableAclPrivilege::Select,
        &["PUBLIC".into()],
        "uqa",
        false,
    );
    grant_column_acl(
        &mut expected,
        "rolname",
        TableAclPrivilege::Update,
        &["PUBLIC".into()],
        "uqa",
        false,
    );
    let expected = BoundTableSecurity::bind(&expected, &roles).unwrap();
    let table = SystemPrivilegeUpdate::new(relation, None, expected.acl.clone().unwrap()).unwrap();
    let column = SystemPrivilegeUpdate::new(
        relation,
        Some("rolname".into()),
        expected.column_acls["rolname"].clone(),
    )
    .unwrap();
    let mut live = SystemRelationSecurities::new();
    for update in [table, column] {
        update.persist(Some(&catalog)).unwrap();
        update.publish(&mut live);
    }
    let restored = restore(&catalog, &roles, false).unwrap();
    assert_eq!(security(&restored, relation), expected);
    assert_eq!(security(&live, relation), expected);
    SystemPrivilegeUpdate::new(relation, Some("rolname".into()), Vec::new())
        .unwrap()
        .persist(Some(&catalog))
        .unwrap();
    assert!(
        security(&restore(&catalog, &roles, false).unwrap(), relation)
            .column_acls
            .is_empty()
    );
}

#[test]
fn system_acl_restoration_rejects_malformed_identity_role_and_attribute_records() {
    let relation = SystemRelation::Projected(uqa_sql::catalog::VirtualRelation::PgAuthid);
    let roles = BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]);
    let valid =
        SystemPrivilegeUpdate::new(relation, None, relation.bootstrap_security().acl.unwrap())
            .unwrap();
    let encoded = persistence::encode(&valid.entry).unwrap();
    for (key, json) in [
        (metadata_key(relation, None), "{".to_string()),
        (format!("{METADATA_PREFIX}public.unknown:"), encoded.clone()),
        (metadata_key(relation, Some("missing")), encoded.clone()),
        (
            metadata_key(relation, None),
            persistence::encode(&SystemAcl {
                revision: [0; 16],
                acl: valid.entry.acl.clone(),
            })
            .unwrap(),
        ),
    ] {
        let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
        catalog.set_metadata(&key, &json).unwrap();
        assert!(restore(&catalog, &roles, false).is_err(), "{key}");
    }
    let mut invalid = relation.bootstrap_security().resolve(&roles).unwrap();
    grant_acl(
        &mut invalid,
        TableAclPrivilege::Select,
        &["missing".into()],
        "uqa",
        false,
    );
    let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    catalog
        .set_metadata(
            &metadata_key(relation, None),
            &serde_json::json!({
                "revision": vec![1; 16], "acl": invalid.acl.unwrap()
            })
            .to_string(),
        )
        .unwrap();
    assert!(restore(&catalog, &roles, true).is_err());
}

#[test]
fn legacy_system_acl_conversion_validates_every_tuple_before_writing_and_retains_revisions() {
    let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    let relation = SystemRelation::Projected(uqa_sql::catalog::VirtualRelation::PgAuthid);
    let roles = BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]);
    let baseline = relation.bootstrap_security().resolve(&roles).unwrap();
    let table_key = metadata_key(relation, None);
    let column_key = metadata_key(relation, Some("rolname"));
    let legacy =
        serde_json::json!({"revision": vec![7; 16], "acl": baseline.acl.unwrap()}).to_string();
    catalog.set_metadata(&table_key, &legacy).unwrap();
    catalog.set_metadata(&column_key, "{").unwrap();
    assert!(restore(&catalog, &roles, true).is_err());
    assert_eq!(
        catalog.get_metadata(&table_key).unwrap().as_deref(),
        Some(legacy.as_str())
    );
    catalog.delete_metadata(&column_key).unwrap();
    assert!(restore(&catalog, &roles, false)
        .unwrap_err()
        .to_string()
        .contains("initial catalog migration"));
    let migrated = restore(&catalog, &roles, true).unwrap();
    assert_eq!(
        migrated
            .values()
            .next()
            .unwrap()
            .table
            .as_ref()
            .unwrap()
            .revision,
        [7; 16]
    );
    assert_eq!(security(&migrated, relation), relation.bootstrap_security());
    assert_eq!(restore(&catalog, &roles, false).unwrap(), migrated);
    let encoded = catalog.get_metadata(&table_key).unwrap().unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&encoded).unwrap()["system_acl_format"],
        1
    );
}

#[test]
fn current_system_acl_format_never_falls_back_to_legacy_or_implicit_public() {
    let relation = SystemRelation::Projected(uqa_sql::catalog::VirtualRelation::PgAuthid);
    let roles = BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]);
    let entry =
        SystemPrivilegeUpdate::new(relation, None, relation.bootstrap_security().acl.unwrap())
            .unwrap()
            .entry;
    let baseline: serde_json::Value =
        serde_json::from_str(&persistence::encode(&entry).unwrap()).unwrap();
    for corruption in [
        "version",
        "null_version",
        "grantee",
        "grantor",
        "incarnation",
    ] {
        let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
        let mut invalid = baseline.clone();
        match corruption {
            "version" => invalid["system_acl_format"] = 99.into(),
            "null_version" => invalid["system_acl_format"] = serde_json::Value::Null,
            "grantee" => {
                invalid["entry"]["acl"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("role");
            }
            "grantor" => {
                invalid["entry"]["acl"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("grantor");
            }
            _ => {
                invalid["entry"]["acl"][0]["grantor"]["object_id"] =
                    serde_json::json!(vec![42; 16]);
            }
        }
        let json = invalid.to_string();
        let key = metadata_key(relation, None);
        catalog.set_metadata(&key, &json).unwrap();
        for allow_migration in [false, true] {
            assert!(
                restore(&catalog, &roles, allow_migration).is_err(),
                "{corruption}"
            );
            assert_eq!(
                catalog.get_metadata(&key).unwrap().as_deref(),
                Some(json.as_str())
            );
        }
    }
}
