//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    catalog_acl::{AclGrantee, TableAclEntry, TablePrivileges},
    catalog_role::{BoundAclEntry, RoleIdentity},
};

#[test]
fn captured_owner_and_acl_endpoints_round_trip_without_names() {
    let owner = RoleIdentity::BOOTSTRAP;
    let reader = RoleIdentity {
        oid: 20001,
        object_id: [11; 16],
    };
    let mut security = BoundRelationSecurity::owner(owner);
    security.acl = Some(vec![BoundAclEntry {
        role: Some(reader),
        grantor: owner,
        privileges: TablePrivileges::ALL,
        grant_options: TablePrivileges::default(),
    }]);
    security.column_acls.insert(
        "value".into(),
        vec![BoundAclEntry {
            role: None,
            grantor: reader,
            privileges: TablePrivileges {
                select: true,
                ..TablePrivileges::default()
            },
            grant_options: TablePrivileges::default(),
        }],
    );
    let row = RelationSecurityRow::Bound(security);
    let encoded = serde_json::to_value(&row).unwrap();
    assert_eq!(encoded["relation_security_format"], 1);
    assert_eq!(encoded["role_owner"]["oid"], owner.oid);
    assert_eq!(
        encoded["column_acls"]["value"][0]["role"],
        serde_json::Value::Null
    );
    assert_eq!(
        serde_json::from_value::<RelationSecurityRow>(encoded).unwrap(),
        row
    );
}

#[test]
fn current_null_acl_is_explicit_and_missing_security_fields_are_rejected() {
    let encoded = serde_json::to_value(RelationSecurityRow::bootstrap()).unwrap();
    assert!(encoded.as_object().unwrap().contains_key("acl"));
    for field in ["role_owner", "acl", "column_acls"] {
        let mut incomplete = encoded.clone();
        incomplete.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<RelationSecurityRow>(incomplete).is_err(),
            "{field}"
        );
    }
    for version in [
        serde_json::json!(null),
        serde_json::json!(0),
        serde_json::json!(2),
        serde_json::json!("1"),
    ] {
        let mut unsupported = encoded.clone();
        unsupported["relation_security_format"] = version;
        assert!(serde_json::from_value::<RelationSecurityRow>(unsupported).is_err());
    }
    let mut mixed = encoded;
    mixed["role_owner"] = serde_json::json!("uqa");
    assert!(serde_json::from_value::<RelationSecurityRow>(mixed).is_err());
}

#[test]
fn legacy_names_remain_legacy_until_sql_binds_the_complete_catalog() {
    assert!(serde_json::from_str::<RelationSecurityRow>("{}").is_err());
    let mut legacy = LegacyRelationSecurity::owner("owner");
    legacy.acl = Some(vec![TableAclEntry {
        role: AclGrantee::Role("PUBLIC".into()),
        grantor: Some("owner".into()),
        privileges: TablePrivileges::ALL,
        grant_options: TablePrivileges::default(),
    }]);
    let encoded = serde_json::to_value(RelationSecurityRow::Legacy(legacy.clone())).unwrap();
    assert!(encoded.get("relation_security_format").is_none());
    assert_eq!(
        serde_json::from_value::<RelationSecurityRow>(encoded).unwrap(),
        RelationSecurityRow::Legacy(legacy)
    );
    let bound_without_format =
        serde_json::json!({"role_owner": RoleIdentity::BOOTSTRAP, "acl": null, "column_acls": {}});
    assert!(serde_json::from_value::<RelationSecurityRow>(bound_without_format).is_err());
}

#[test]
fn table_rows_preserve_security_discriminators_and_reject_corrupt_current_fields() {
    for security in [
        RelationSecurityRow::bootstrap(),
        RelationSecurityRow::legacy("owner"),
    ] {
        let schema = crate::TableSchema {
            relation: crate::RelationIdentity::new("public", "items"),
            security: security.clone(),
            object_id: [1; 16],
            storage_generation: [2; 16],
            analyzer_json: String::new(),
            fts_fields: Vec::new(),
            vector_fields: Vec::new(),
            columns_json: String::new(),
            constraints_json: String::new(),
        };
        let mut encoded = serde_json::to_value(&schema).unwrap();
        let restored: crate::TableSchema = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(restored.security, security);
        assert_eq!(restored.object_id, schema.object_id);
        assert_eq!(restored.storage_generation, schema.storage_generation);
        encoded["unexpected_security_field"] = serde_json::json!(true);
        assert!(serde_json::from_value::<crate::TableSchema>(encoded).is_err());
    }
}
