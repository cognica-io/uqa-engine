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
};
use uqa_storage::{KeyValueCatalog, MemoryKeyValueStore};

#[test]
fn system_acl_restoration_round_trips_independent_table_and_attribute_tuples() {
    let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    let relation = SystemRelation::PgAuthid;
    let roles = BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]);
    let mut expected = relation.bootstrap_security();
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
    let restored = restore(&catalog, &roles).unwrap();
    assert_eq!(security(&restored, relation), expected);
    assert_eq!(security(&live, relation), expected);
    SystemPrivilegeUpdate::new(relation, Some("rolname".into()), Vec::new())
        .unwrap()
        .persist(Some(&catalog))
        .unwrap();
    assert!(security(&restore(&catalog, &roles).unwrap(), relation)
        .column_acls
        .is_empty());
}

#[test]
fn system_acl_restoration_rejects_malformed_identity_role_and_attribute_records() {
    let relation = SystemRelation::PgAuthid;
    let roles = BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]);
    let valid =
        SystemPrivilegeUpdate::new(relation, None, relation.bootstrap_security().acl.unwrap())
            .unwrap();
    let encoded = serde_json::to_string(&valid.entry).unwrap();
    for (key, json) in [
        (metadata_key(relation, None), "{".to_string()),
        (format!("{METADATA_PREFIX}public.unknown:"), encoded.clone()),
        (metadata_key(relation, Some("missing")), encoded.clone()),
        (
            metadata_key(relation, None),
            serde_json::to_string(&SystemAcl {
                revision: [0; 16],
                acl: valid.entry.acl.clone(),
            })
            .unwrap(),
        ),
    ] {
        let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
        catalog.set_metadata(&key, &json).unwrap();
        assert!(restore(&catalog, &roles).is_err(), "{key}");
    }
    let mut invalid = relation.bootstrap_security();
    grant_acl(
        &mut invalid,
        TableAclPrivilege::Select,
        &["missing".into()],
        "uqa",
        false,
    );
    let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    SystemPrivilegeUpdate::new(relation, None, invalid.acl.unwrap())
        .unwrap()
        .persist(Some(&catalog))
        .unwrap();
    assert!(restore(&catalog, &roles).is_err());
}
