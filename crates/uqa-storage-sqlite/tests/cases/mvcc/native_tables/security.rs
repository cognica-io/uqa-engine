//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Nullable migrated descriptors and table/column privileges retain their catalog snapshot boundary.

use super::{bind, schema, BTreeMap, Catalog, ManagedConnection};
use uqa_storage::{TableAclEntry, TablePrivileges};

#[test]
fn native_table_security_preserves_nullable_migrations_and_retained_privileges() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection.clone()).unwrap();
    catalog.save_table(&schema("docs", 1, 1)).unwrap();
    connection
        .with_mut(|sql| {
            sql.execute(
                "UPDATE _tables SET columns = NULL, acl_json = NULL, column_acls_json = NULL",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    bind(&connection);
    let mut row = catalog.load_tables().unwrap().remove(0);
    assert!(row.columns_json.is_empty());
    assert_eq!(row.acl, None);
    assert!(row.column_acls.is_empty());
    let other = connection.new_session();
    let old = Catalog::open(other.clone()).unwrap();
    other.begin_transaction().unwrap();
    assert_eq!(old.load_tables().unwrap()[0].acl, None);
    let acl = vec![TableAclEntry {
        role: "reader".into(),
        grantor: Some("new_owner".into()),
        privileges: TablePrivileges {
            select: true,
            ..TablePrivileges::default()
        },
        grant_options: TablePrivileges {
            select: true,
            ..TablePrivileges::default()
        },
    }];
    row.acl = Some(acl.clone());
    row.column_acls = BTreeMap::from([("n".into(), acl)]);
    row.role_owner = "new_owner".into();
    catalog.save_table(&row).unwrap();
    let updated = catalog.load_tables().unwrap().remove(0);
    assert_eq!(updated.role_owner, row.role_owner);
    assert_eq!(updated.acl, row.acl);
    assert_eq!(updated.column_acls, row.column_acls);
    assert_eq!(updated.columns_json, row.columns_json);
    assert_eq!(old.load_tables().unwrap()[0].acl, None);
    other.rollback_transaction().unwrap();
    assert_eq!(old.load_tables().unwrap()[0].acl, row.acl);
}
