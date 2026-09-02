//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn save_view(catalog: &Catalog, name: &str, owner: &str) {
    catalog
        .save_view(&ViewRow {
            relation: RelationIdentity::new("public", name),
            role_owner: owner.into(),
            definition_json: "{}".into(),
        })
        .unwrap();
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
