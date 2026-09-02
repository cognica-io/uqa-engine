//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn migration_adds_schema_ownership_and_acl_metadata() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    current.save_schema("app").unwrap();
    drop(current);
    connection
        .with(|database| {
            database.execute_batch(
                "ALTER TABLE _schemas DROP COLUMN acl_json;
                 ALTER TABLE _schemas DROP COLUMN role_owner;
                 UPDATE _metadata SET value = '32' WHERE key = 'schema_version';",
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection.clone()).unwrap();
    assert_eq!(
        upgraded.load_schema_rows().unwrap(),
        vec![SchemaRow::legacy("app"), SchemaRow::legacy("public")]
    );
    let app = SchemaRow {
        name: "app".into(),
        role_owner: "app_owner".into(),
        acl: Some(vec![crate::catalog::SchemaAclEntry {
            role: "app_writer".into(),
            grantor: Some("app_owner".into()),
            privileges: crate::catalog::SchemaPrivileges {
                usage: true,
                create: false,
            },
            grant_options: crate::catalog::SchemaPrivileges {
                usage: true,
                create: false,
            },
        }]),
    };
    upgraded.save_schema_row(&app).unwrap();
    drop(upgraded);

    let reopened = Catalog::open(connection).unwrap();
    assert_eq!(reopened.load_schema_rows().unwrap()[0], app);
}
