//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::build_pg_database;
use crate::catalog::{test_support::empty_catalog, CatalogReadView};
use std::{collections::BTreeMap, sync::Arc};

#[test]
fn database_catalog_snapshots_keep_owner_oids_and_project_current_acl_names() {
    use uqa_core::{ArrayValue, Value};
    use uqa_sql::catalog::{
        roles::RoleDefinition,
        security::database::{
            BoundDatabaseSecurity, DatabaseAclEntry, DatabasePrivileges, DatabaseSecurity,
        },
    };

    let owner = RoleDefinition::bootstrap();
    let mut reader = owner.clone();
    reader.name = "reader".into();
    reader.oid = 20_000;
    reader.object_id = [1; 16];
    reader.attributes.clear();
    let mut roles = BTreeMap::from([(owner.name.clone(), owner), (reader.name.clone(), reader)]);
    let create = DatabasePrivileges {
        create: true,
        ..DatabasePrivileges::default()
    };
    let security = DatabaseSecurity {
        role_owner: "uqa".into(),
        acl: Some(vec![
            DatabaseAclEntry {
                role: "reader".into(),
                grantor: None,
                privileges: create,
                grant_options: create,
            },
            DatabaseAclEntry {
                role: uqa_core::catalog_acl::AclGrantee::Public,
                grantor: None,
                privileges: DatabasePrivileges {
                    connect: true,
                    ..DatabasePrivileges::default()
                },
                grant_options: DatabasePrivileges::default(),
            },
        ]),
    };
    let mut snapshot = empty_catalog().snapshot().clone();
    snapshot.definitions.database_security =
        Arc::new(BoundDatabaseSecurity::bind(&security, &roles).unwrap());
    snapshot.definitions.roles = Arc::new(roles.clone());
    let original = CatalogReadView::new(snapshot.clone());
    for name in ["uqa", "reader"] {
        let mut renamed = roles.remove(name).unwrap();
        let mut replacement = renamed.clone();
        renamed.name = format!("renamed {name}");
        replacement.oid += 30_000;
        replacement.object_id = if name == "uqa" { [8; 16] } else { [9; 16] };
        roles.insert(renamed.name.clone(), renamed);
        roles.insert(name.into(), replacement);
    }
    snapshot.definitions.roles = Arc::new(roles);
    let renamed = CatalogReadView::new(snapshot);
    assert_eq!(original.database_security(), renamed.database_security());
    for (catalog, expected) in [
        (&original, ["reader=C*/uqa", "=c/uqa"]),
        (
            &renamed,
            [
                "\"renamed reader\"=C*/\"renamed uqa\"",
                "=c/\"renamed uqa\"",
            ],
        ),
    ] {
        let rows = build_pg_database(catalog).unwrap();
        assert_eq!(rows[0]["datdba"], Value::Int(10));
        assert_eq!(
            rows[0]["datacl"],
            Value::Array(
                ArrayValue::try_new(expected.map(|entry| Value::Str(entry.into())).to_vec())
                    .unwrap()
            )
        );
    }
}
