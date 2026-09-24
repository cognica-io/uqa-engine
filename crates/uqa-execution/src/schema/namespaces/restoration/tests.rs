//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_storage::{KeyValueCatalog, MemoryKeyValueStore};

fn fixture() -> (KeyValueCatalog, BTreeMap<String, RoleDefinition>) {
    (
        KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new())),
        BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())]),
    )
}

#[test]
fn schema_migration_binds_once_and_secondary_restore_never_writes() {
    let (catalog, mut roles) = fixture();
    let legacy = SchemaRow::legacy("public");
    catalog.save_schema_row(&legacy).unwrap();
    assert!(restore(&catalog, &roles, false)
        .unwrap_err()
        .to_string()
        .contains("initial catalog migration"));
    assert_eq!(catalog.load_schema_rows().unwrap(), [legacy]);
    let restored = restore(&catalog, &roles, true).unwrap();
    assert_eq!(restored["public"], BoundSchemaSecurity::bootstrap("public"));
    let encoded = catalog.load_schema_rows().unwrap();
    assert!(matches!(encoded[0], SchemaRow::Bound(_)));
    let mut owner = roles.remove("uqa").unwrap();
    owner.name = "renamed".into();
    roles.insert(owner.name.clone(), owner);
    assert_eq!(restore(&catalog, &roles, false).unwrap(), restored);
    assert_eq!(catalog.load_schema_rows().unwrap(), encoded);
    assert_eq!(
        restored["public"].resolve(&roles).unwrap().role_owner,
        "renamed"
    );
    roles.get_mut("renamed").unwrap().object_id = [9; 16];
    assert!(restore(&catalog, &roles, true).is_err());
    assert_eq!(catalog.load_schema_rows().unwrap(), encoded);
}

#[test]
fn all_schema_references_are_validated_before_any_legacy_conversion_is_written() {
    let (catalog, roles) = fixture();
    catalog
        .save_schema_row(&SchemaRow::legacy("a_valid"))
        .unwrap();
    let mut missing = uqa_core::catalog_schema::SchemaRow::legacy("z_invalid");
    missing.role_owner = "missing".into();
    catalog
        .save_schema_row(&SchemaRow::Legacy(missing))
        .unwrap();
    let before = catalog.load_schema_rows().unwrap();
    assert!(restore(&catalog, &roles, true)
        .unwrap_err()
        .to_string()
        .contains("missing role `missing`"));
    assert_eq!(catalog.load_schema_rows().unwrap(), before);
}

#[test]
fn bound_role_predecessors_receive_namespace_identity_once_without_rebinding_authority() {
    let (catalog, roles) = fixture();
    let row = SchemaRow::bootstrap("old_namespace");
    catalog.save_schema_row(&row).unwrap();
    assert!(restore(&catalog, &roles, false).is_err());
    assert_eq!(catalog.load_schema_rows().unwrap(), [row]);
    let restored = restore(&catalog, &roles, true).unwrap();
    let security = &restored["old_namespace"];
    assert_eq!(
        security.namespace_oid("old_namespace"),
        uqa_sql::catalog::oids::schema_oid("old_namespace")
    );
    assert!(security.tuple.unwrap().is_valid());
    assert_eq!(security.role_owner, roles["uqa"].identity());
    assert_eq!(restore(&catalog, &roles, false).unwrap(), restored);
    assert_eq!(restore(&catalog, &roles, true).unwrap(), restored);
}

#[test]
fn duplicate_namespace_oid_or_incarnation_rejects_the_whole_migration_before_writes() {
    for same_oid in [true, false] {
        let (catalog, roles) = fixture();
        catalog
            .save_schema_row(&SchemaRow::legacy("a_legacy"))
            .unwrap();
        let mut first = BoundSchemaSecurity::bootstrap("first");
        let mut second = BoundSchemaSecurity::bootstrap("second");
        first.tuple = Some(uqa_core::catalog_schema::SchemaTupleIdentity {
            oid: 40_001,
            object_id: [1; 16],
            revision: [2; 16],
        });
        second.tuple = Some(uqa_core::catalog_schema::SchemaTupleIdentity {
            oid: if same_oid { 40_001 } else { 40_002 },
            object_id: if same_oid { [3; 16] } else { [1; 16] },
            revision: [4; 16],
        });
        catalog.save_schema_row(&first.row("first").into()).unwrap();
        catalog
            .save_schema_row(&second.row("second").into())
            .unwrap();
        let before = catalog.load_schema_rows().unwrap();
        assert!(restore(&catalog, &roles, true)
            .unwrap_err()
            .to_string()
            .contains("duplicate schema catalog identity"));
        assert_eq!(catalog.load_schema_rows().unwrap(), before);
    }
}
