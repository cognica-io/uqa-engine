//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_core::{catalog_acl::LegacyRelationSecurity, RelationIdentity};
use uqa_storage::{KeyValueCatalog, MemoryKeyValueStore};

fn fixture() -> (KeyValueCatalog, BTreeMap<String, RoleDefinition>) {
    let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    catalog.save_schema("public").unwrap();
    (
        catalog,
        BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]),
    )
}

fn table(name: &str, owner: &str) -> TableSchema {
    TableSchema {
        relation: RelationIdentity::new("public", name),
        security: RelationSecurityRow::legacy(owner),
        object_id: [1; 16],
        storage_generation: [2; 16],
        analyzer_json: String::new(),
        fts_fields: Vec::new(),
        vector_fields: Vec::new(),
        columns_json: String::new(),
        constraints_json: String::new(),
    }
}

#[test]
fn relation_restore_binds_once_and_rejects_secondary_name_conversion() {
    let (catalog, mut roles) = fixture();
    let legacy = table("secured", "uqa");
    catalog.save_table(&legacy).unwrap();
    assert!(restore_tables(&catalog, &roles, false)
        .unwrap_err()
        .to_string()
        .contains("initial catalog migration"));
    assert_eq!(
        serde_json::to_value(catalog.load_tables().unwrap()).unwrap(),
        serde_json::to_value([legacy]).unwrap()
    );
    let restored = restore_tables(&catalog, &roles, true).unwrap();
    let encoded = catalog.load_tables().unwrap();
    assert!(matches!(encoded[0].security, RelationSecurityRow::Bound(_)));
    let mut owner = roles.remove("uqa").unwrap();
    owner.name = "renamed".into();
    roles.insert(owner.name.clone(), owner);
    assert_eq!(
        restore_tables(&catalog, &roles, false)
            .unwrap()
            .into_iter()
            .map(|(_, security)| security)
            .collect::<Vec<_>>(),
        restored
            .into_iter()
            .map(|(_, security)| security)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        serde_json::to_value(catalog.load_tables().unwrap()).unwrap(),
        serde_json::to_value(&encoded).unwrap()
    );
    roles.get_mut("renamed").unwrap().object_id = [9; 16];
    assert!(restore_tables(&catalog, &roles, true).is_err());
    assert_eq!(
        serde_json::to_value(catalog.load_tables().unwrap()).unwrap(),
        serde_json::to_value(&encoded).unwrap()
    );
}

#[test]
fn all_table_security_is_validated_before_any_legacy_row_is_replaced() {
    let (catalog, roles) = fixture();
    catalog.save_table(&table("a_valid", "uqa")).unwrap();
    catalog.save_table(&table("z_invalid", "missing")).unwrap();
    let before = catalog.load_tables().unwrap();
    assert!(restore_tables(&catalog, &roles, true)
        .unwrap_err()
        .to_string()
        .contains("missing role `missing`"));
    assert_eq!(
        serde_json::to_value(catalog.load_tables().unwrap()).unwrap(),
        serde_json::to_value(&before).unwrap()
    );
}

#[test]
fn relation_restore_validates_column_grants_and_grantor_paths() {
    let (_, roles) = fixture();
    let mut legacy = LegacyRelationSecurity::owner("uqa");
    legacy.column_acls.insert(
        "id".into(),
        vec![uqa_core::catalog_acl::TableAclEntry {
            role: uqa_core::catalog_acl::AclGrantee::Public,
            grantor: Some("uqa".into()),
            privileges: uqa_core::catalog_acl::TablePrivileges {
                select: true,
                ..Default::default()
            },
            grant_options: uqa_core::catalog_acl::TablePrivileges::default(),
        }],
    );
    let row = RelationSecurityRow::Legacy(legacy);
    assert!(restore_security(&row, None, &roles, true).is_err());
    let columns = ["id".into()];
    let bound = restore_security(&row, Some(&columns), &roles, true).unwrap();
    let mut encoded = bound.row();
    encoded.column_acls.get_mut("id").unwrap()[0]
        .grantor
        .object_id = [9; 16];
    assert!(restore_security(&encoded.into(), Some(&columns), &roles, true).is_err());
    assert_eq!(
        restore_security(&bound.row().into(), Some(&columns), &roles, false).unwrap(),
        bound
    );
}
