//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn column_acl() -> std::collections::BTreeMap<String, Vec<crate::catalog::TableAclEntry>> {
    std::collections::BTreeMap::from([(
        "visible".into(),
        vec![crate::catalog::TableAclEntry {
            role: "reader".into(),
            grantor: Some("owner".into()),
            privileges: crate::catalog::TablePrivileges {
                select: true,
                ..crate::catalog::TablePrivileges::default()
            },
            grant_options: crate::catalog::TablePrivileges::default(),
        }],
    )])
}

#[test]
fn migration_37_adds_empty_column_acls_to_legacy_tables() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    current
        .save_table(&TableSchema {
            relation: RelationIdentity::new("public", "legacy_column_acl"),
            role_owner: "owner".into(),
            acl: None,
            column_acls: column_acl(),
            object_id: [37; 16],
            storage_generation: [37; 16],
            analyzer_json: "{}".into(),
            fts_fields: Vec::new(),
            vector_fields: Vec::new(),
            columns_json: "[]".into(),
            constraints_json: String::new(),
        })
        .unwrap();
    drop(current);
    connection
        .with(|database| {
            database.execute("ALTER TABLE _tables DROP COLUMN column_acls_json", [])?;
            database.execute(
                "UPDATE _metadata SET value = '36' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    assert!(upgraded.load_tables().unwrap()[0].column_acls.is_empty());
}

#[test]
fn migration_37_preserves_column_acls_installed_before_its_version_marker() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    let expected = column_acl();
    current
        .save_table(&TableSchema {
            relation: RelationIdentity::new("public", "early_column_acl"),
            role_owner: "owner".into(),
            acl: None,
            column_acls: expected.clone(),
            object_id: [38; 16],
            storage_generation: [38; 16],
            analyzer_json: "{}".into(),
            fts_fields: Vec::new(),
            vector_fields: Vec::new(),
            columns_json: "[]".into(),
            constraints_json: String::new(),
        })
        .unwrap();
    drop(current);
    connection
        .with(|database| {
            database.execute(
                "UPDATE _metadata SET value = '36' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    assert_eq!(upgraded.load_tables().unwrap()[0].column_acls, expected);
}
