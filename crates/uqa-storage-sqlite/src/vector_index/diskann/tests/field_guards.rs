//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::{RelationIdentity, RelationSecurityRow, TableSchema, VectorFieldSchema};

#[test]
fn native_vector_field_guard_reclamation_bounds_deleted_tables_and_preserves_writers() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("field-guards.db");
        let connection = open(&path, mode);
        let catalog = Catalog::open(connection.clone()).unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let records = SQLiteRecordStore::for_native(&connection, &control).unwrap();
        let prefix = records
            .vector_field_guard_layout()
            .prefix(&control)
            .unwrap();
        for cycle in 1..=3 {
            let row = TableSchema {
                relation: RelationIdentity::new("public", format!("guard_lifetime_{cycle}")),
                security: RelationSecurityRow::legacy("owner"),
                object_id: [cycle; 16],
                storage_generation: [cycle; 16],
                analyzer_json: "{}".into(),
                fts_fields: vec![],
                vector_fields: vec![VectorFieldSchema {
                    field: "embedding\0日本語".into(),
                    dimensions: 2,
                }],
                columns_json: "[]".into(),
                constraints_json: "{}".into(),
            };
            catalog.save_table(&row).unwrap();
            let table = row.relation.qualified_name();
            let source = canonical(&connection, &table, "embedding\0日本語", 2);
            let original = source.replace(1, &[vec![1.0, -0.0]], &control).unwrap();
            let retained = source.retain(&control).unwrap();
            connection.reclaim_obsolete().unwrap();
            assert_eq!(guard_count(&connection), 1);
            catalog.drop_table_and_data(&table).unwrap();
            connection.reclaim_obsolete().unwrap();
            assert_eq!(guard_count(&connection), 0);
            assert_eq!(retained.origin(1, &control).unwrap(), Some(original));
            drop(retained);
            connection.reclaim_obsolete().unwrap();
            assert_no_history(&connection, &prefix);
        }
        let source = canonical(&connection, "guard-race", "embedding", 2);
        source.replace(0, &[], &control).unwrap();
        let peer = connection.new_session();
        let writer = canonical(&peer, "guard-race", "embedding", 2);
        peer.begin_transaction().unwrap();
        writer.replace(1, &[vec![1.0, 0.0]], &control).unwrap();
        connection.reclaim_obsolete().unwrap();
        peer.commit_transaction().unwrap();
        connection.reclaim_obsolete().unwrap();
        assert_eq!(guard_count(&connection), 1);
    }
}

fn guard_count(connection: &ManagedConnection) -> i64 {
    connection
        .with_physical(|sqlite| {
            Ok(sqlite.query_row(
                "SELECT count(*) FROM _metadata WHERE key LIKE 'vector_field_guard::%'",
                [],
                |row| row.get(0),
            )?)
        })
        .unwrap()
}

fn assert_no_history(connection: &ManagedConnection, prefix: &[u8]) {
    connection
        .with_physical(|sqlite| {
            for table in ["_uqa_mvcc_heads", "_uqa_mvcc_versions"] {
                let count: i64 = sqlite.query_row(
                    &format!("SELECT count(*) FROM {table} WHERE substr(key, 1, ?1) = ?2"),
                    rusqlite::params![prefix.len() as i64, prefix],
                    |row| row.get(0),
                )?;
                assert_eq!(count, 0, "empty field guard identities remain in {table}");
            }
            Ok(())
        })
        .unwrap();
}
