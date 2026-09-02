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
        .save_foreign_table(
            &RelationIdentity::new("public", name),
            owner,
            "memory",
            "[]",
            "{}",
        )
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
