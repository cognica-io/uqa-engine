//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn migration_35_adds_bootstrap_ownership_to_legacy_tables() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    current
        .save_table(&TableSchema {
            relation: RelationIdentity::new("public", "legacy_owner"),
            role_owner: "former_owner".into(),
            acl: None,
            object_id: [12; 16],
            storage_generation: [13; 16],
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
            database.execute("ALTER TABLE _tables DROP COLUMN role_owner", [])?;
            database.execute(
                "UPDATE _metadata SET value = '34' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    let schema = upgraded.load_tables().unwrap().remove(0);
    assert_eq!(schema.role_owner, "uqa");
    assert_eq!(schema.object_id, [12; 16]);
    assert_eq!(schema.storage_generation, [13; 16]);
}

#[test]
fn migration_35_preserves_table_ownership_when_the_column_precedes_its_marker() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    current
        .save_table(&TableSchema {
            relation: RelationIdentity::new("public", "early_owner"),
            role_owner: "early_table_owner".into(),
            acl: None,
            object_id: [14; 16],
            storage_generation: [15; 16],
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
                "UPDATE _metadata SET value = '34' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    let schema = upgraded.load_tables().unwrap().remove(0);
    assert_eq!(schema.role_owner, "early_table_owner");
}

#[test]
fn migration_36_adds_a_null_table_acl_without_reconstructing_security_state() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    current
        .save_table(&TableSchema {
            relation: RelationIdentity::new("public", "legacy_acl"),
            role_owner: "legacy_owner".into(),
            acl: None,
            object_id: [16; 16],
            storage_generation: [17; 16],
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
            database.execute("ALTER TABLE _tables DROP COLUMN acl_json", [])?;
            database.execute(
                "UPDATE _metadata SET value = '35' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    let schema = upgraded.load_tables().unwrap().remove(0);
    assert_eq!(schema.role_owner, "legacy_owner");
    assert_eq!(schema.acl, None);
    assert_eq!(schema.object_id, [16; 16]);
    assert_eq!(schema.storage_generation, [17; 16]);
}

#[test]
fn migration_36_preserves_an_acl_column_installed_before_its_version_marker() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    let acl = vec![crate::catalog::TableAclEntry {
        role: "reader".into(),
        grantor: Some("early_owner".into()),
        privileges: crate::catalog::TablePrivileges {
            select: true,
            ..crate::catalog::TablePrivileges::default()
        },
        grant_options: crate::catalog::TablePrivileges::default(),
    }];
    current
        .save_table(&TableSchema {
            relation: RelationIdentity::new("public", "early_acl"),
            role_owner: "early_owner".into(),
            acl: Some(acl.clone()),
            object_id: [18; 16],
            storage_generation: [19; 16],
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
                "UPDATE _metadata SET value = '35' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    let schema = upgraded.load_tables().unwrap().remove(0);
    assert_eq!(schema.role_owner, "early_owner");
    assert_eq!(schema.acl, Some(acl));
}
