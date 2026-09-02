//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{TableAclEntry, TablePrivileges};

fn save_view(catalog: &Catalog, name: &str, owner: &str) {
    catalog
        .save_view(&ViewRow {
            relation: RelationIdentity::new("public", name),
            role_owner: owner.into(),
            acl: None,
            column_acls: std::collections::BTreeMap::new(),
            definition_json: "{}".into(),
        })
        .unwrap();
}

fn view_acl() -> Vec<TableAclEntry> {
    vec![TableAclEntry {
        role: "reader".into(),
        grantor: Some("owner".into()),
        privileges: TablePrivileges {
            select: true,
            ..TablePrivileges::default()
        },
        grant_options: TablePrivileges {
            select: true,
            ..TablePrivileges::default()
        },
    }]
}

#[test]
fn migration_38_adds_bootstrap_ownership_to_legacy_views() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    save_view(&current, "legacy_view_owner", "former_owner");
    drop(current);
    connection
        .with(|database| {
            database.execute("ALTER TABLE _views DROP COLUMN role_owner", [])?;
            database.execute(
                "UPDATE _metadata SET value = '37' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    let view = upgraded.load_views().unwrap().remove(0);
    assert_eq!(view.role_owner, "uqa");
    assert_eq!(view.definition_json, "{}");
}

#[test]
fn migration_38_preserves_view_ownership_when_the_column_precedes_its_marker() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    save_view(&current, "early_view_owner", "early_owner");
    drop(current);
    connection
        .with(|database| {
            database.execute(
                "UPDATE _metadata SET value = '37' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    assert_eq!(upgraded.load_views().unwrap()[0].role_owner, "early_owner");
}

#[test]
fn migration_39_adds_empty_acls_to_legacy_views() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    save_view(&current, "legacy_view_acl", "owner");
    drop(current);
    connection
        .with(|database| {
            database.execute("ALTER TABLE _views DROP COLUMN acl_json", [])?;
            database.execute("ALTER TABLE _views DROP COLUMN column_acls_json", [])?;
            database.execute(
                "UPDATE _metadata SET value = '38' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    let view = upgraded.load_views().unwrap().remove(0);
    assert_eq!(view.role_owner, "owner");
    assert_eq!(view.acl, None);
    assert!(view.column_acls.is_empty());
}

#[test]
fn migration_39_preserves_view_acls_when_columns_precede_their_marker() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    let acl = view_acl();
    let column_acls = std::collections::BTreeMap::from([("id".into(), view_acl())]);
    current
        .save_view(&ViewRow {
            relation: RelationIdentity::new("public", "early_view_acl"),
            role_owner: "owner".into(),
            acl: Some(acl.clone()),
            column_acls: column_acls.clone(),
            definition_json: "{}".into(),
        })
        .unwrap();
    drop(current);
    connection
        .with(|database| {
            database.execute(
                "UPDATE _metadata SET value = '38' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    let view = upgraded.load_views().unwrap().remove(0);
    assert_eq!(view.acl, Some(acl));
    assert_eq!(view.column_acls, column_acls);
}
