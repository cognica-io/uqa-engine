//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn save_foreign_table(catalog: &Catalog, name: &str, owner: &str) {
    catalog.save_schema("public").unwrap();
    catalog
        .save_foreign_server("memory", "memory_fdw", "{}")
        .unwrap();
    catalog
        .save_foreign_table(&ForeignTableRow {
            relation: RelationIdentity::new("public", name),
            role_owner: owner.into(),
            acl: None,
            column_acls: std::collections::BTreeMap::new(),
            server_name: "memory".into(),
            columns_json: "[]".into(),
            options_json: "{}".into(),
        })
        .unwrap();
}

#[test]
fn migration_40_adds_bootstrap_ownership_to_legacy_foreign_tables() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    save_foreign_table(&current, "legacy_foreign_owner", "former_owner");
    drop(current);
    connection
        .with(|database| {
            database.execute("ALTER TABLE _foreign_tables DROP COLUMN role_owner", [])?;
            database.execute(
                "UPDATE _metadata SET value = '39' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    let foreign_table = upgraded.load_foreign_tables().unwrap().remove(0);
    assert_eq!(foreign_table.role_owner, "uqa");
    assert_eq!(foreign_table.server_name, "memory");
}

#[test]
fn migration_40_preserves_foreign_table_ownership_when_the_column_precedes_its_marker() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    save_foreign_table(&current, "early_foreign_owner", "early_owner");
    drop(current);
    connection
        .with(|database| {
            database.execute(
                "UPDATE _metadata SET value = '39' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    assert_eq!(
        upgraded.load_foreign_tables().unwrap()[0].role_owner,
        "early_owner"
    );
}

#[test]
fn migration_41_adds_explicit_default_acl_state_to_owned_foreign_tables() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    save_foreign_table(&current, "legacy_foreign_acl", "foreign_owner");
    drop(current);
    connection
        .with(|database| {
            database.execute("ALTER TABLE _foreign_tables DROP COLUMN acl_json", [])?;
            database.execute(
                "ALTER TABLE _foreign_tables DROP COLUMN column_acls_json",
                [],
            )?;
            database.execute(
                "UPDATE _metadata SET value = '40' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    let foreign_table = upgraded.load_foreign_tables().unwrap().remove(0);
    assert_eq!(foreign_table.role_owner, "foreign_owner");
    assert_eq!(foreign_table.acl, None);
    assert!(foreign_table.column_acls.is_empty());
    assert_eq!(foreign_table.server_name, "memory");
}

#[test]
fn migration_41_converts_only_legacy_null_column_acl_state() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    save_foreign_table(&current, "legacy_null_foreign_acl", "foreign_owner");
    drop(current);
    connection
        .with(|database| {
            database.execute("ALTER TABLE _foreign_tables DROP COLUMN acl_json", [])?;
            database.execute(
                "ALTER TABLE _foreign_tables DROP COLUMN column_acls_json",
                [],
            )?;
            database.execute("ALTER TABLE _foreign_tables ADD COLUMN acl_json TEXT", [])?;
            database.execute(
                "ALTER TABLE _foreign_tables ADD COLUMN column_acls_json TEXT",
                [],
            )?;
            database.execute(
                "UPDATE _metadata SET value = '40' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    let foreign_table = upgraded.load_foreign_tables().unwrap().remove(0);
    assert_eq!(foreign_table.role_owner, "foreign_owner");
    assert_eq!(foreign_table.acl, None);
    assert!(foreign_table.column_acls.is_empty());
}

#[test]
fn migration_41_preserves_acl_state_when_columns_precede_their_marker() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    current.save_schema("public").unwrap();
    current
        .save_foreign_server("memory", "memory_fdw", "{}")
        .unwrap();
    let acl = vec![crate::catalog::TableAclEntry {
        role: "foreign_reader".into(),
        grantor: Some("foreign_owner".into()),
        privileges: crate::catalog::TablePrivileges {
            select: true,
            ..crate::catalog::TablePrivileges::default()
        },
        grant_options: crate::catalog::TablePrivileges::default(),
    }];
    let column_acls = std::collections::BTreeMap::from([(
        "id".into(),
        vec![crate::catalog::TableAclEntry {
            role: "foreign_column_reader".into(),
            grantor: Some("foreign_owner".into()),
            privileges: crate::catalog::TablePrivileges {
                select: true,
                ..crate::catalog::TablePrivileges::default()
            },
            grant_options: crate::catalog::TablePrivileges::default(),
        }],
    )]);
    current
        .save_foreign_table(&ForeignTableRow {
            relation: RelationIdentity::new("public", "early_foreign_acl"),
            role_owner: "foreign_owner".into(),
            acl: Some(acl.clone()),
            column_acls: column_acls.clone(),
            server_name: "memory".into(),
            columns_json: "[]".into(),
            options_json: "{}".into(),
        })
        .unwrap();
    drop(current);
    connection
        .with(|database| {
            database.execute(
                "UPDATE _metadata SET value = '40' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    let foreign_table = upgraded.load_foreign_tables().unwrap().remove(0);
    assert_eq!(foreign_table.role_owner, "foreign_owner");
    assert_eq!(foreign_table.acl, Some(acl));
    assert_eq!(foreign_table.column_acls, column_acls);
}
