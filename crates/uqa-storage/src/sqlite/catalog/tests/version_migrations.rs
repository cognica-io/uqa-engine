//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

mod index_relations;
mod log_count;
mod schema_security;
mod schema_version;
mod table_ownership;

#[test]
fn migration_is_idempotent() {
    let mc = ManagedConnection::open_in_memory().unwrap();
    let _cat1 = Catalog::open(mc.clone()).unwrap();
    // Reopen on the same handle: should not re-run migrations or
    // raise an error.
    let _cat2 = Catalog::open(mc).unwrap();
}

#[test]
fn migration_15_creates_document_blob_storage_for_existing_catalogs() {
    let mc = ManagedConnection::open_in_memory().unwrap();
    let _current = Catalog::open(mc.clone()).unwrap();
    mc.with(|conn| {
        conn.execute("DROP TABLE _document_blobs", [])?;
        conn.execute(
            "UPDATE _metadata SET value = '14' WHERE key = 'schema_version'",
            [],
        )?;
        Ok(())
    })
    .unwrap();

    let _upgraded = Catalog::open(mc.clone()).unwrap();
    mc.with(|conn| {
        let count: u32 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name = '_document_blobs'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(count, 1);
        Ok(())
    })
    .unwrap();
}

#[test]
fn migration_16_adds_backward_compatible_table_constraints() {
    let mc = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(mc.clone()).unwrap();
    current
        .save_table(&TableSchema {
            relation: RelationIdentity::new("public", "legacy"),
            role_owner: "uqa".into(),
            acl: None,
            object_id: [1; 16],
            storage_generation: [1; 16],
            analyzer_json: "{}".into(),
            fts_fields: Vec::new(),
            vector_fields: Vec::new(),
            columns_json: "[]".into(),
            constraints_json: String::new(),
        })
        .unwrap();
    drop(current);
    mc.with(|conn| {
        conn.execute("ALTER TABLE _tables DROP COLUMN constraints", [])?;
        conn.execute(
            "UPDATE _metadata SET value = '15' WHERE key = 'schema_version'",
            [],
        )?;
        Ok(())
    })
    .unwrap();

    let upgraded = Catalog::open(mc).unwrap();
    let schemas = upgraded.load_tables().unwrap();
    assert_eq!(schemas.len(), 1);
    assert_eq!(schemas[0].relation.qualified_name(), "public.legacy");
    assert!(schemas[0].constraints_json.is_empty());
}

#[test]
fn migration_24_adds_persistent_table_storage_generations() {
    let mc = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(mc.clone()).unwrap();
    current
        .save_table(&TableSchema {
            relation: RelationIdentity::new("public", "legacy_generation"),
            role_owner: "uqa".into(),
            acl: None,
            object_id: [7; 16],
            storage_generation: [7; 16],
            analyzer_json: "{}".into(),
            fts_fields: Vec::new(),
            vector_fields: Vec::new(),
            columns_json: "[]".into(),
            constraints_json: String::new(),
        })
        .unwrap();
    drop(current);
    mc.with(|connection| {
        connection.execute("ALTER TABLE _tables DROP COLUMN storage_generation", [])?;
        connection.execute(
            "UPDATE _metadata SET value = '23' WHERE key = 'schema_version'",
            [],
        )?;
        Ok(())
    })
    .unwrap();

    let upgraded = Catalog::open(mc).unwrap();
    let mut schema = upgraded.load_tables().unwrap().remove(0);
    assert_eq!(schema.storage_generation, [0; 16]);
    schema.storage_generation = [9; 16];
    upgraded.save_table(&schema).unwrap();
    assert_eq!(
        upgraded.load_tables().unwrap()[0].storage_generation,
        [9; 16]
    );
}

#[test]
fn migration_24_preserves_a_storage_generation_installed_before_its_version_marker() {
    let mc = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(mc.clone()).unwrap();
    current
        .save_table(&TableSchema {
            relation: RelationIdentity::new("public", "early_generation"),
            role_owner: "uqa".into(),
            acl: None,
            object_id: [11; 16],
            storage_generation: [11; 16],
            analyzer_json: "{}".into(),
            fts_fields: Vec::new(),
            vector_fields: Vec::new(),
            columns_json: "[]".into(),
            constraints_json: String::new(),
        })
        .unwrap();
    drop(current);
    mc.with(|connection| {
        connection.execute(
            "UPDATE _metadata SET value = '23' WHERE key = 'schema_version'",
            [],
        )?;
        Ok(())
    })
    .unwrap();

    let upgraded = Catalog::open(mc.clone()).unwrap();
    assert_eq!(
        upgraded.load_tables().unwrap()[0].storage_generation,
        [11; 16]
    );
    mc.with(|connection| {
        let version: String = connection.query_row(
            "SELECT value FROM _metadata WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(version, CURRENT_SCHEMA_VERSION.to_string());
        Ok(())
    })
    .unwrap();
}

#[test]
fn migration_25_adds_persistent_table_object_identities() {
    let mc = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(mc.clone()).unwrap();
    current
        .save_table(&TableSchema {
            relation: RelationIdentity::new("public", "legacy_object"),
            role_owner: "uqa".into(),
            acl: None,
            object_id: [7; 16],
            storage_generation: [8; 16],
            analyzer_json: "{}".into(),
            fts_fields: Vec::new(),
            vector_fields: Vec::new(),
            columns_json: "[]".into(),
            constraints_json: String::new(),
        })
        .unwrap();
    drop(current);
    mc.with(|connection| {
        connection.execute("ALTER TABLE _tables DROP COLUMN object_id", [])?;
        connection.execute(
            "UPDATE _metadata SET value = '24' WHERE key = 'schema_version'",
            [],
        )?;
        Ok(())
    })
    .unwrap();

    let upgraded = Catalog::open(mc).unwrap();
    let mut schema = upgraded.load_tables().unwrap().remove(0);
    assert_eq!(schema.object_id, [0; 16]);
    schema.object_id = [9; 16];
    upgraded.save_table(&schema).unwrap();
    assert_eq!(upgraded.load_tables().unwrap()[0].object_id, [9; 16]);
}

#[test]
fn migration_26_adds_persistent_sequence_object_identities() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    current
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "legacy_sequence_object"),
            role_owner: "uqa".into(),
            acl: None,
            object_id: [7; 16],
            definition_generation: [7; 16],
            start: 1,
            increment: 1,
            current: 1,
            called: false,
            log_count: 0,
            persistence: "p".into(),
            options: SequenceOptions::default(),
            owner: None,
        })
        .unwrap();
    drop(current);
    connection
        .with(|database| {
            database.execute("ALTER TABLE _sequences DROP COLUMN object_id", [])?;
            database.execute(
                "UPDATE _metadata SET value = '25' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    let mut sequence = upgraded.load_sequence_rows().unwrap().remove(0);
    assert_eq!(sequence.object_id, [0; 16]);
    sequence.object_id = [9; 16];
    assert!(upgraded.replace_sequence_row(&sequence).unwrap());
    assert_eq!(upgraded.load_sequence_rows().unwrap()[0].object_id, [9; 16]);
}

#[test]
fn migration_27_adds_postgresql_sequence_defaults() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    current
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "legacy_descending_options"),
            role_owner: "uqa".into(),
            acl: None,
            object_id: [27; 16],
            definition_generation: [27; 16],
            start: -1,
            increment: -3,
            current: -1,
            called: false,
            log_count: 0,
            persistence: "p".into(),
            options: SequenceOptions::default(),
            owner: None,
        })
        .unwrap();
    drop(current);
    connection
        .with(|database| {
            database.execute("ALTER TABLE _sequences DROP COLUMN cycle", [])?;
            database.execute("ALTER TABLE _sequences DROP COLUMN max_value", [])?;
            database.execute("ALTER TABLE _sequences DROP COLUMN min_value", [])?;
            database.execute("ALTER TABLE _sequences DROP COLUMN data_type", [])?;
            database.execute(
                "UPDATE _metadata SET value = '26' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    let sequence = upgraded.load_sequence_rows().unwrap().remove(0);
    assert_eq!(sequence.options.data_type, "bigint");
    assert_eq!(sequence.options.min_value, Some(i64::MIN));
    assert_eq!(sequence.options.max_value, Some(-1));
    assert!(!sequence.options.cycle);
}

#[test]
fn migration_27_preserves_options_when_columns_precede_the_version_marker() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    current
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "already_migrated_options"),
            role_owner: "uqa".into(),
            acl: None,
            object_id: [28; 16],
            definition_generation: [28; 16],
            start: 3,
            increment: 2,
            current: 3,
            called: false,
            log_count: 0,
            persistence: "p".into(),
            options: SequenceOptions {
                data_type: "integer".into(),
                min_value: Some(2),
                max_value: Some(9),
                cycle: true,
                cache_size: 7,
            },
            owner: None,
        })
        .unwrap();
    drop(current);
    connection
        .with(|database| {
            database.execute(
                "UPDATE _metadata SET value = '26' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    let sequence = upgraded.load_sequence_rows().unwrap().remove(0);
    assert_eq!(sequence.options.data_type, "integer");
    assert_eq!(sequence.options.min_value, Some(2));
    assert_eq!(sequence.options.max_value, Some(9));
    assert!(sequence.options.cycle);
}

#[test]
fn migration_28_adds_sequence_cache_and_definition_generation() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    current
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "legacy_cache"),
            role_owner: "uqa".into(),
            acl: None,
            object_id: [28; 16],
            definition_generation: [29; 16],
            start: 1,
            increment: 1,
            current: 1,
            called: false,
            log_count: 0,
            persistence: "p".into(),
            options: SequenceOptions {
                cache_size: 7,
                ..SequenceOptions::default()
            },
            owner: None,
        })
        .unwrap();
    drop(current);
    connection
        .with(|database| {
            database.execute(
                "ALTER TABLE _sequences DROP COLUMN definition_generation",
                [],
            )?;
            database.execute("ALTER TABLE _sequences DROP COLUMN cache_size", [])?;
            database.execute(
                "UPDATE _metadata SET value = '27' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    let sequence = upgraded.load_sequence_rows().unwrap().remove(0);
    assert_eq!(sequence.options.cache_size, 1);
    assert_eq!(sequence.definition_generation, sequence.object_id);
}

#[test]
fn migration_28_preserves_cache_state_when_columns_precede_the_version_marker() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    current
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "already_migrated_cache"),
            role_owner: "uqa".into(),
            acl: None,
            object_id: [30; 16],
            definition_generation: [31; 16],
            start: 1,
            increment: 1,
            current: 1,
            called: false,
            log_count: 0,
            persistence: "p".into(),
            options: SequenceOptions {
                cache_size: 9,
                ..SequenceOptions::default()
            },
            owner: None,
        })
        .unwrap();
    drop(current);
    connection
        .with(|database| {
            database.execute(
                "UPDATE _metadata SET value = '27' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    let sequence = upgraded.load_sequence_rows().unwrap().remove(0);
    assert_eq!(sequence.options.cache_size, 9);
    assert_eq!(sequence.definition_generation, [31; 16]);
}

#[test]
fn migration_29_adds_sequence_owner_columns() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    current
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "legacy_owner"),
            role_owner: "uqa".into(),
            acl: None,
            object_id: [32; 16],
            definition_generation: [33; 16],
            start: 1,
            increment: 1,
            current: 1,
            called: false,
            log_count: 0,
            persistence: "p".into(),
            options: SequenceOptions::default(),
            owner: Some(crate::catalog::SequenceOwner {
                table_object_id: [34; 16],
                column_object_id: [35; 16],
                dependency: crate::catalog::SequenceOwnerDependency::Automatic,
            }),
        })
        .unwrap();
    drop(current);
    connection
        .with(|database| {
            database.execute("ALTER TABLE _sequences DROP COLUMN owner_dependency", [])?;
            database.execute(
                "ALTER TABLE _sequences DROP COLUMN owner_column_object_id",
                [],
            )?;
            database.execute(
                "ALTER TABLE _sequences DROP COLUMN owner_table_object_id",
                [],
            )?;
            database.execute(
                "UPDATE _metadata SET value = '28' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    let sequence = upgraded.load_sequence_rows().unwrap().remove(0);
    assert_eq!(sequence.owner, None);
}

#[test]
fn migration_29_preserves_owner_when_columns_precede_the_version_marker() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    let owner = crate::catalog::SequenceOwner {
        table_object_id: [36; 16],
        column_object_id: [37; 16],
        dependency: crate::catalog::SequenceOwnerDependency::Internal,
    };
    current
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "already_migrated_owner"),
            role_owner: "uqa".into(),
            acl: None,
            object_id: [38; 16],
            definition_generation: [39; 16],
            start: 1,
            increment: 1,
            current: 1,
            called: false,
            log_count: 0,
            persistence: "p".into(),
            options: SequenceOptions::default(),
            owner: Some(owner),
        })
        .unwrap();
    drop(current);
    connection
        .with(|database| {
            database.execute(
                "UPDATE _metadata SET value = '28' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    let sequence = upgraded.load_sequence_rows().unwrap().remove(0);
    assert_eq!(sequence.owner, Some(owner));
}

#[test]
fn migration_30_adds_sequence_role_owner_with_bootstrap_default() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    current
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "legacy_role_owner"),
            role_owner: "discarded_owner".into(),
            acl: None,
            object_id: [40; 16],
            definition_generation: [41; 16],
            start: 1,
            increment: 1,
            current: 1,
            called: false,
            log_count: 0,
            persistence: "p".into(),
            options: SequenceOptions::default(),
            owner: None,
        })
        .unwrap();
    drop(current);
    connection
        .with(|database| {
            database.execute("ALTER TABLE _sequences DROP COLUMN role_owner", [])?;
            database.execute(
                "UPDATE _metadata SET value = '29' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    let sequence = upgraded.load_sequence_rows().unwrap().remove(0);
    assert_eq!(sequence.role_owner, "uqa");
}

#[test]
fn migration_30_preserves_role_owner_when_column_precedes_version_marker() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    current
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "already_migrated_role_owner"),
            role_owner: "retained_owner".into(),
            acl: None,
            object_id: [42; 16],
            definition_generation: [43; 16],
            start: 1,
            increment: 1,
            current: 1,
            called: false,
            log_count: 0,
            persistence: "p".into(),
            options: SequenceOptions::default(),
            owner: None,
        })
        .unwrap();
    drop(current);
    connection
        .with(|database| {
            database.execute(
                "UPDATE _metadata SET value = '29' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    let sequence = upgraded.load_sequence_rows().unwrap().remove(0);
    assert_eq!(sequence.role_owner, "retained_owner");
}

#[test]
fn migration_31_adds_nullable_sequence_acl() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    current
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "legacy_acl"),
            role_owner: "uqa".into(),
            acl: None,
            object_id: [44; 16],
            definition_generation: [45; 16],
            start: 1,
            increment: 1,
            current: 1,
            called: false,
            log_count: 0,
            persistence: "p".into(),
            options: SequenceOptions::default(),
            owner: None,
        })
        .unwrap();
    drop(current);
    connection
        .with(|database| {
            database.execute("ALTER TABLE _sequences DROP COLUMN acl_json", [])?;
            database.execute(
                "UPDATE _metadata SET value = '30' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    let sequence = upgraded.load_sequence_rows().unwrap().remove(0);
    assert_eq!(sequence.acl, None);
}

#[test]
fn migration_31_preserves_sequence_acl_when_column_precedes_version_marker() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    let acl = vec![crate::catalog::SequenceAclEntry {
        role: "reader".into(),
        grantor: Some("uqa".into()),
        privileges: crate::catalog::SequencePrivileges {
            select: true,
            update: false,
            usage: false,
        },
        grant_options: crate::catalog::SequencePrivileges::default(),
    }];
    current
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "already_migrated_acl"),
            role_owner: "uqa".into(),
            acl: Some(acl.clone()),
            object_id: [46; 16],
            definition_generation: [47; 16],
            start: 1,
            increment: 1,
            current: 1,
            called: false,
            log_count: 0,
            persistence: "p".into(),
            options: SequenceOptions::default(),
            owner: None,
        })
        .unwrap();
    drop(current);
    connection
        .with(|database| {
            database.execute(
                "UPDATE _metadata SET value = '30' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection).unwrap();
    let sequence = upgraded.load_sequence_rows().unwrap().remove(0);
    assert_eq!(sequence.acl, Some(acl));
}

#[test]
fn migration_18_preserves_legacy_sequence_sentinel_semantics() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    current
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "legacy_uncalled"),
            role_owner: "uqa".into(),
            acl: None,
            object_id: [18; 16],
            definition_generation: [18; 16],
            start: 1,
            increment: 1,
            current: 0,
            called: false,
            log_count: 0,
            persistence: "p".into(),
            options: SequenceOptions::default(),
            owner: None,
        })
        .unwrap();
    drop(current);
    connection
        .with(|conn| {
            conn.execute("ALTER TABLE _sequences DROP COLUMN called", [])?;
            conn.execute(
                "UPDATE _metadata SET value = '17' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection.clone()).unwrap();
    let row = upgraded.load_sequence_rows().unwrap().remove(0);
    assert!(
        row.called,
        "legacy current values are already sentinel-adjusted"
    );
    assert_eq!(
        upgraded
            .next_sequence_value("public.legacy_uncalled", row.object_id)
            .unwrap(),
        Some(1)
    );
    connection
        .with(|conn| {
            let version: String = conn.query_row(
                "SELECT value FROM _metadata WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(version, CURRENT_SCHEMA_VERSION.to_string());
            Ok(())
        })
        .unwrap();
}

#[test]
fn migration_23_moves_sequence_persistence_into_typed_rows() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    current
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "unlogged_ids"),
            role_owner: "uqa".into(),
            acl: None,
            object_id: [23; 16],
            definition_generation: [23; 16],
            start: 1,
            increment: 1,
            current: 1,
            called: false,
            log_count: 0,
            persistence: "u".into(),
            options: SequenceOptions::default(),
            owner: None,
        })
        .unwrap();
    drop(current);
    connection
        .with(|conn| {
            conn.execute("ALTER TABLE _sequences DROP COLUMN persistence", [])?;
            conn.execute(
                "INSERT OR REPLACE INTO _metadata(key, value) VALUES ('sequence-persistence:public.unlogged_ids', 'u')",
                [],
            )?;
            conn.execute(
                "UPDATE _metadata SET value = '22' WHERE key = 'schema_version'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection.clone()).unwrap();
    let rows = upgraded.load_sequence_rows().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].persistence, "u");
    assert_eq!(
        upgraded
            .get_metadata("sequence-persistence:public.unlogged_ids")
            .unwrap(),
        None
    );
}

#[test]
fn migration_19_creates_complete_hnsw_storage_schema() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    drop(current);
    connection
        .with(|conn| {
            conn.execute_batch(
                "DROP TABLE _hnsw_edges;
                 DROP TABLE _hnsw_nodes;
                 DROP TABLE _hnsw_indexes;
                 UPDATE _metadata SET value = '18' WHERE key = 'schema_version';",
            )?;
            Ok(())
        })
        .unwrap();

    let _upgraded = Catalog::open(connection.clone()).unwrap();
    connection
        .with(|conn| {
            for table in ["_hnsw_indexes", "_hnsw_nodes", "_hnsw_edges"] {
                let count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |row| row.get(0),
                )?;
                assert_eq!(count, 1, "missing HNSW migration table {table}");
            }
            let mut statement = conn.prepare("PRAGMA table_info(_hnsw_indexes)")?;
            let columns = statement
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<rusqlite::Result<std::collections::BTreeSet<_>>>()?;
            assert!(columns.contains("revision"));
            assert!(columns.contains("format_version"));
            let version: String = conn.query_row(
                "SELECT value FROM _metadata WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(version, CURRENT_SCHEMA_VERSION.to_string());
            Ok(())
        })
        .unwrap();
}

#[test]
fn migration_20_repairs_only_hnsw_rows_backed_by_legacy_ivf_metadata() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    current
        .save_table(&empty_table("public", "legacy"))
        .unwrap();
    current
        .save_table(&empty_table("public", "native"))
        .unwrap();
    drop(current);
    connection
        .with(|conn| {
            conn.execute_batch(
                "DROP TABLE _catalog_indexes;
                 CREATE TABLE _catalog_indexes (
                     name TEXT PRIMARY KEY,
                     index_type TEXT NOT NULL,
                     table_name TEXT NOT NULL,
                     columns TEXT NOT NULL,
                     parameters TEXT NOT NULL
                 );
                 INSERT INTO _catalog_indexes
                     (name, index_type, table_name, columns, parameters)
                 VALUES
                     ('legacy_idx', 'hnsw', 'public.legacy', '[\"embedding\"]', '{}'),
                     ('native_idx', 'hnsw', 'public.native', '[\"embedding\"]', '{}');
                 INSERT INTO _ivf_indexes
                     (table_name, field, dimensions, nlist, nprobe,
                      train_threshold, state, trained_size,
                      deletes_since_train, vector_count)
                 VALUES
                     ('public.legacy', 'embedding', 2, 100, 10,
                      256, 'untrained', 0, 0, 0);
                 INSERT INTO _hnsw_indexes
                     (table_name, field, dimensions, m, ef_construction,
                      ef_search, rebuild_threshold, seed, entry_node_id,
                      max_level, next_node_id, live_count, deleted_count,
                      revision, format_version)
                 VALUES
                     ('public.native', 'embedding', 2, 16, 200,
                      64, 1000, '7', NULL, 0, 0, 0, 0, 1, 1);
                 UPDATE _metadata SET value = '19'
                  WHERE key = 'schema_version';",
            )?;
            Ok(())
        })
        .unwrap();

    let upgraded = Catalog::open(connection.clone()).unwrap();
    let rows = upgraded.load_catalog_indexes().unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.relation.name == "legacy_idx")
            .map(|row| row.index_type.as_str()),
        Some("ivf")
    );
    assert_eq!(
        rows.iter()
            .find(|row| row.relation.name == "native_idx")
            .map(|row| row.index_type.as_str()),
        Some("hnsw")
    );
}

#[test]
fn migration_21_schedules_only_invalid_btree_fields_and_installs_guards() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    drop(current);
    connection
        .with(|conn| {
            conn.execute_batch(
                "INSERT INTO _documents (table_name, doc_id, body)
                     VALUES
                     ('public.engine_meta', 1, '{\"key\":\"missing\"}'),
                     ('public.clean', 1, '{\"key\":\"kept\"}');
                 INSERT INTO _btree_indexes (table_name, field)
                     VALUES
                     ('public.engine_meta', 'key'),
                     ('public.clean', 'key');
                 INSERT INTO _btree_index_entries
                     (table_name, field, doc_id, value_json)
                     VALUES
                     ('public.clean', 'key', 1,
                      '{\"type\":\"Str\",\"value\":\"kept\"}');
                 DROP TRIGGER IF EXISTS _btree_documents_delete;
                 DROP TRIGGER IF EXISTS _btree_entries_document_insert;
                 DROP TRIGGER IF EXISTS _btree_entries_document_update;
                 DROP TRIGGER IF EXISTS _btree_documents_doc_id_update;
                 UPDATE _metadata SET value = '20'
                  WHERE key = 'schema_version';",
            )?;
            Ok(())
        })
        .unwrap();

    let _upgraded = Catalog::open(connection.clone()).unwrap();
    connection
        .with(|conn| {
            let definitions: i64 =
                conn.query_row("SELECT COUNT(*) FROM _btree_indexes", [], |row| row.get(0))?;
            let entries: i64 =
                conn.query_row("SELECT COUNT(*) FROM _btree_index_entries", [], |row| {
                    row.get(0)
                })?;
            assert_eq!(definitions, 2);
            assert_eq!(entries, 1);
            let kept: i64 = conn.query_row(
                "SELECT COUNT(*) FROM _btree_indexes
                  WHERE table_name = 'public.clean' AND field = 'key'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(kept, 1);
            let pending: i64 = conn.query_row(
                "SELECT COUNT(*) FROM _btree_index_repairs
                  WHERE table_name = 'public.engine_meta' AND field = 'key'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(pending, 1);

            let guards: i64 = conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master
                  WHERE type = 'trigger' AND name IN (
                      '_btree_documents_delete',
                      '_btree_entries_document_insert',
                      '_btree_entries_document_update',
                      '_btree_documents_doc_id_update'
                  )",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(guards, 4);

            conn.pragma_update(None, "foreign_keys", "OFF")?;
            conn.execute(
                "DELETE FROM _documents
                  WHERE table_name = 'public.clean' AND doc_id = 1",
                [],
            )?;
            let cascaded: i64 = conn.query_row(
                "SELECT COUNT(*) FROM _btree_index_entries
                  WHERE table_name = 'public.clean' AND doc_id = 1",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(cascaded, 0);
            Ok(())
        })
        .unwrap();
}
