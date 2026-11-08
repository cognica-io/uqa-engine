//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recovery retains immutable transaction generations rather than copying their catalogs.

use super::{statement_abort_snapshot, Engine};
use std::sync::Arc;
use uqa_storage::read_control::StorageReadControl;

fn sql(engine: &Engine, source: &str) {
    engine.sql(source, &[]).unwrap();
}

fn image(snapshot: &crate::EngineDataSnapshot) -> Vec<u8> {
    let table = snapshot.data.tables.values().next().unwrap();
    let mut bytes = Vec::new();
    assert!(table
        .document_store
        .read()
        .write_image_document(1, &mut bytes, &StorageReadControl::with_limit(1 << 20))
        .unwrap()
        .is_some());
    bytes
}

#[test]
fn transaction_recovery_retains_large_catalog_generations_without_metadata_copies() {
    for count in [1, 64] {
        let engine = Engine::new();
        let columns = (0..32)
            .map(|column| format!("column_with_retained_metadata_{column} text DEFAULT 'value'"))
            .collect::<Vec<_>>()
            .join(",");
        for table in 0..count {
            sql(&engine, &format!("CREATE TABLE t{table} ({columns})"));
        }
        sql(&engine, "BEGIN");
        for savepoint in [false, true] {
            if savepoint {
                sql(&engine, "SAVEPOINT retained");
            }
            let stack = engine.session.transactions.lock();
            let frame = stack.last().unwrap();
            let expected = if savepoint {
                frame.savepoints.last().unwrap().data_snapshot.as_ref()
            } else {
                frame.data_snapshot.as_ref()
            }
            .unwrap();
            let mut recovery = None;
            let allocated = allocation_counter::measure(|| {
                recovery = Some(statement_abort_snapshot(frame));
            });
            let recovery = recovery.unwrap();
            assert!(allocated.bytes_total < 64 * 1024, "{count}: {allocated:?}");
            assert!(Arc::ptr_eq(expected, recovery.data.as_ref().unwrap()));
            assert_eq!(recovery.data.as_ref().unwrap().data.tables.len(), count);
        }
        sql(&engine, "ROLLBACK");
    }
}

#[test]
fn transaction_generations_preserve_private_savepoint_and_failed_statement_state() {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE docs(id integer PRIMARY KEY, body text); INSERT INTO docs VALUES(1,'committed')");
    sql(&engine, "BEGIN");
    let committed = engine.session.transactions.lock()[0]
        .data_snapshot
        .clone()
        .unwrap();
    let committed_image = image(&committed);
    sql(&engine, "ALTER TABLE docs ADD COLUMN extra integer DEFAULT 7; UPDATE docs SET body='private'; SAVEPOINT retained");
    let private = engine.session.transactions.lock()[0].savepoints[0]
        .data_snapshot
        .clone()
        .unwrap();
    let private_image = image(&private);
    assert_ne!(committed_image, private_image);
    assert!(!Arc::ptr_eq(&committed, &private));
    assert_eq!(
        committed.data.tables.values().next().unwrap().columns.len(),
        2
    );
    assert_eq!(
        private.data.tables.values().next().unwrap().columns.len(),
        3
    );

    let error = engine
        .sql("UPDATE docs SET body='failed'; SELECT 1/0", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("22012"));
    assert_eq!(
        engine
            .sql("SELECT * FROM docs", &[])
            .unwrap_err()
            .sqlstate(),
        Some("25P02")
    );
    sql(&engine, "ROLLBACK TO retained");
    let result = engine.sql("SELECT body,extra FROM docs", &[]).unwrap();
    assert_eq!(
        result.rows[0].get("body"),
        Some(&crate::Value::Str("private".into()))
    );
    assert_eq!(result.rows[0].get("extra"), Some(&crate::Value::Int(7)));
    sql(
        &engine,
        "DROP TABLE docs; CREATE TABLE docs(replaced boolean); ROLLBACK TO retained; COMMIT",
    );
    assert_eq!(
        engine.table("docs").unwrap().unwrap().columns.read().len(),
        3
    );
    sql(
        &engine,
        "UPDATE docs SET body='later'; ALTER TABLE docs DROP COLUMN extra",
    );
    drop(engine);
    assert_eq!(image(&committed), committed_image);
    assert_eq!(image(&private), private_image);
    assert_eq!(
        committed.data.tables.values().next().unwrap().columns.len(),
        2
    );
    assert_eq!(
        private.data.tables.values().next().unwrap().columns.len(),
        3
    );
}

#[test]
fn transaction_generations_share_large_table_metadata_and_restore_original_roots() {
    let engine = Engine::new();
    let default = "x".repeat(512 * 1024);
    sql(
        &engine,
        &format!("CREATE TABLE docs(id integer PRIMARY KEY, body text DEFAULT '{default}')"),
    );
    let table = engine.table("docs").unwrap().unwrap();
    let columns = table.columns.snapshot();
    let constraints = table.key_constraints.snapshot();
    let mut snapshot = None;
    let allocated = allocation_counter::measure(|| {
        snapshot = engine
            .snapshot_transaction_data(super::super::snapshots::SnapshotBoundary::CommittedOriginal)
            .unwrap();
    });
    assert!(allocated.bytes_total < 64 * 1024, "{allocated:?}");
    let snapshot = snapshot.unwrap();
    let retained = snapshot.data.tables.values().next().unwrap();
    assert!(Arc::ptr_eq(&columns, &retained.columns));
    assert!(Arc::ptr_eq(
        &constraints,
        &retained.key_constraints.snapshot()
    ));

    sql(&engine, "BEGIN; ALTER TABLE docs ALTER COLUMN body SET DEFAULT 'changed'; ALTER TABLE docs ADD COLUMN extra boolean");
    assert!(!Arc::ptr_eq(&columns, &table.columns.snapshot()));
    sql(&engine, "ROLLBACK");
    assert!(Arc::ptr_eq(&columns, &table.columns.snapshot()));
    assert!(Arc::ptr_eq(&constraints, &table.key_constraints.snapshot()));
    sql(&engine, "INSERT INTO docs(id) VALUES(1)");
    let result = engine
        .sql("SELECT length(body) AS size FROM docs", &[])
        .unwrap();
    assert_eq!(
        result.rows[0].get("size"),
        Some(&crate::Value::Int(512 * 1024))
    );
}
