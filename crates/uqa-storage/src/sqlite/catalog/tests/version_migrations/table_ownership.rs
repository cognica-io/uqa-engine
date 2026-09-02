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
