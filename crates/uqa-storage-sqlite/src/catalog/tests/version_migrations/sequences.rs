//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn migration_26_adds_persistent_sequence_object_identities() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    current
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "legacy_sequence_object"),
            security: uqa_storage::SequenceSecurityRow::bootstrap(),
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
            security: uqa_storage::SequenceSecurityRow::bootstrap(),
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
            security: uqa_storage::SequenceSecurityRow::bootstrap(),
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
            security: uqa_storage::SequenceSecurityRow::bootstrap(),
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
            security: uqa_storage::SequenceSecurityRow::bootstrap(),
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
            security: uqa_storage::SequenceSecurityRow::bootstrap(),
            object_id: [32; 16],
            definition_generation: [33; 16],
            start: 1,
            increment: 1,
            current: 1,
            called: false,
            log_count: 0,
            persistence: "p".into(),
            options: SequenceOptions::default(),
            owner: Some(uqa_storage::catalog::SequenceOwner {
                table_object_id: [34; 16],
                column_object_id: [35; 16],
                dependency: uqa_storage::catalog::SequenceOwnerDependency::Automatic,
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
    let owner = uqa_storage::catalog::SequenceOwner {
        table_object_id: [36; 16],
        column_object_id: [37; 16],
        dependency: uqa_storage::catalog::SequenceOwnerDependency::Internal,
    };
    current
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "already_migrated_owner"),
            security: uqa_storage::SequenceSecurityRow::bootstrap(),
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
            security: uqa_storage::SequenceSecurityRow::Legacy(
                uqa_core::catalog_sequence::LegacySequenceSecurity {
                    role_owner: "discarded_owner".into(),
                    acl: None,
                },
            ),
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
    assert_eq!(
        sequence.security,
        uqa_storage::SequenceSecurityRow::legacy("uqa")
    );
}

#[test]
fn migration_30_preserves_role_owner_when_column_precedes_version_marker() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    current
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "already_migrated_role_owner"),
            security: uqa_storage::SequenceSecurityRow::Legacy(
                uqa_core::catalog_sequence::LegacySequenceSecurity {
                    role_owner: "retained_owner".into(),
                    acl: None,
                },
            ),
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
    assert_eq!(
        sequence.security,
        uqa_storage::SequenceSecurityRow::legacy("retained_owner")
    );
}

#[test]
fn migration_31_adds_nullable_sequence_acl() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    current
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "legacy_acl"),
            security: uqa_storage::SequenceSecurityRow::bootstrap(),
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
    assert!(match sequence.security {
        uqa_storage::SequenceSecurityRow::Bound(security) => security.acl.is_none(),
        uqa_storage::SequenceSecurityRow::Legacy(security) => security.acl.is_none(),
    });
}

#[test]
fn migration_31_preserves_sequence_acl_when_column_precedes_version_marker() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    let acl = vec![uqa_storage::catalog::SequenceAclEntry {
        role: "reader".into(),
        grantor: Some("uqa".into()),
        privileges: uqa_storage::catalog::SequencePrivileges {
            select: true,
            update: false,
            usage: false,
        },
        grant_options: uqa_storage::catalog::SequencePrivileges::default(),
    }];
    current
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "already_migrated_acl"),
            security: uqa_storage::SequenceSecurityRow::Legacy(
                uqa_core::catalog_sequence::LegacySequenceSecurity {
                    role_owner: "uqa".into(),
                    acl: Some(acl.clone()),
                },
            ),
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
    assert_eq!(
        sequence.security,
        uqa_storage::SequenceSecurityRow::Legacy(
            uqa_core::catalog_sequence::LegacySequenceSecurity {
                role_owner: "uqa".into(),
                acl: Some(acl)
            }
        )
    );
}

#[test]
fn migration_18_preserves_legacy_sequence_sentinel_semantics() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let current = Catalog::open(connection.clone()).unwrap();
    current
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "legacy_uncalled"),
            security: uqa_storage::SequenceSecurityRow::bootstrap(),
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
            security: uqa_storage::SequenceSecurityRow::bootstrap(),
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
