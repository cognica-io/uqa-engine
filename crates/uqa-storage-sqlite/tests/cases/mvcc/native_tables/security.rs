//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Nullable migrated descriptors and table/column privileges retain their catalog snapshot boundary.

use super::{bind, schema, BTreeMap, Catalog, ManagedConnection};
use uqa_storage::{RelationSecurityRow, TableAclEntry, TablePrivileges};

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
    assert_eq!(row.security, RelationSecurityRow::legacy("owner"));
    let other = connection.new_session();
    let old = Catalog::open(other.clone()).unwrap();
    other.begin_transaction().unwrap();
    assert_eq!(
        old.load_tables().unwrap()[0].security,
        RelationSecurityRow::legacy("owner")
    );
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
    row.security = RelationSecurityRow::Legacy(uqa_core::catalog_acl::LegacyRelationSecurity {
        role_owner: "new_owner".into(),
        acl: Some(acl.clone()),
        column_acls: BTreeMap::from([("n".into(), acl)]),
    });
    catalog.save_table(&row).unwrap();
    let updated = catalog.load_tables().unwrap().remove(0);
    assert_eq!(updated.security, row.security);
    assert_eq!(updated.columns_json, row.columns_json);
    assert_eq!(
        old.load_tables().unwrap()[0].security,
        RelationSecurityRow::legacy("owner")
    );
    other.rollback_transaction().unwrap();
    assert_eq!(old.load_tables().unwrap()[0].security, row.security);
}
