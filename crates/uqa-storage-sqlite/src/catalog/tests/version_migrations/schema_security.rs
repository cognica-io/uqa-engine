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
    let app = SchemaRow::Legacy(uqa_core::catalog_schema::SchemaRow {
        name: "app".into(),
        role_owner: "app_owner".into(),
        acl: Some(vec![uqa_storage::catalog::SchemaAclEntry {
            role: "app_writer".into(),
            grantor: Some("app_owner".into()),
            privileges: uqa_storage::catalog::SchemaPrivileges {
                usage: true,
                create: false,
            },
            grant_options: uqa_storage::catalog::SchemaPrivileges {
                usage: true,
                create: false,
            },
        }]),
    });
    upgraded.save_schema_row(&app).unwrap();
    drop(upgraded);

    let reopened = Catalog::open(connection).unwrap();
    assert_eq!(reopened.load_schema_rows().unwrap()[0], app);
}

#[test]
fn physical_schema_role_identities_survive_reopen_and_native_conversion() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    let mut row = uqa_core::catalog_schema::BoundSchemaRow::bootstrap("public");
    row.role_owner.oid = 20_001;
    row.role_owner.object_id = [7; 16];
    row.acl.as_mut().unwrap()[0].grantor = row.role_owner;
    let row = SchemaRow::Bound(row);
    catalog.save_schema_row(&row).unwrap();
    drop(catalog);
    let catalog = Catalog::open(connection.clone()).unwrap();
    assert_eq!(
        catalog.load_schema_rows().unwrap().as_slice(),
        std::slice::from_ref(&row)
    );
    connection
        .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
        .unwrap();
    assert_eq!(
        catalog.load_schema_rows().unwrap().as_slice(),
        std::slice::from_ref(&row)
    );
    connection.begin_transaction().unwrap();
    catalog
        .save_schema_row(&SchemaRow::bootstrap("public"))
        .unwrap();
    connection.rollback_transaction().unwrap();
    assert_eq!(catalog.load_schema_rows().unwrap(), [row]);
}
