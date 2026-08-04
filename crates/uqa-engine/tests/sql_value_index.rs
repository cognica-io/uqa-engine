//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Value-index correctness: indexed scalar predicates must return
//! exactly what the evaluated scan returns, across inserts, updates,
//! deletes, truncates, and persistent reopens. `indexed` carries a
//! PRIMARY KEY plus btree indexes; `shadow` is the same data with no
//! indexable columns, so every query there takes the scan path.

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::SQLResult;
use uqa_storage::{sqlite::CURRENT_SCHEMA_VERSION, RelationIdentity};

fn persisted_index_count(path: &std::path::Path, table: &str, field: &str) -> i64 {
    let table = RelationIdentity::from_legacy_name(table)
        .unwrap()
        .qualified_name();
    rusqlite::Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM _btree_index_entries
             WHERE table_name = ?1 AND field = ?2",
            [&table, field],
            |row| row.get(0),
        )
        .unwrap()
}

fn persisted_index_definition_count(path: &std::path::Path, table: &str, field: &str) -> i64 {
    let table = RelationIdentity::from_legacy_name(table)
        .unwrap()
        .qualified_name();
    rusqlite::Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM _btree_indexes
             WHERE table_name = ?1 AND field = ?2",
            [&table, field],
            |row| row.get(0),
        )
        .unwrap()
}

fn ids(result: &SQLResult) -> Vec<i64> {
    result
        .rows
        .iter()
        .map(|row| match row.get("id") {
            Some(Value::Int(v)) => *v,
            other => panic!("unexpected id value: {other:?}"),
        })
        .collect()
}

fn setup(engine: &Engine) {
    engine
        .sql(
            "CREATE TABLE indexed (id INTEGER PRIMARY KEY, qty INTEGER, owner TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE TABLE shadow (id INTEGER, qty INTEGER, owner TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql("CREATE INDEX indexed_qty ON indexed USING btree (qty)", &[])
        .unwrap();
    engine
        .sql(
            "CREATE INDEX indexed_owner ON indexed USING btree (owner)",
            &[],
        )
        .unwrap();
    let mut values = Vec::new();
    for i in 1..=500_i64 {
        let owner = format!("owner{}", i % 7);
        values.push(format!("({i}, {}, '{owner}')", i % 50));
    }
    for table in ["indexed", "shadow"] {
        engine
            .sql(
                &format!(
                    "INSERT INTO {table} (id, qty, owner) VALUES {}",
                    values.join(", ")
                ),
                &[],
            )
            .unwrap();
    }
}

fn assert_same(engine: &Engine, predicate: &str) {
    let indexed = engine
        .sql(
            &format!("SELECT id FROM indexed WHERE {predicate} ORDER BY id"),
            &[],
        )
        .unwrap();
    let shadow = engine
        .sql(
            &format!("SELECT id FROM shadow WHERE {predicate} ORDER BY id"),
            &[],
        )
        .unwrap();
    assert_eq!(
        ids(&indexed),
        ids(&shadow),
        "index and scan disagree for `{predicate}`"
    );
}

const PREDICATES: &[&str] = &[
    "qty = 25",
    "qty = 0",
    "qty <> 25",
    "qty > 45",
    "qty >= 45",
    "qty < 3",
    "qty <= 3",
    "qty BETWEEN 10 AND 12",
    "qty IN (1, 2, 3)",
    "qty IS NULL",
    "qty IS NOT NULL",
    "owner = 'owner3'",
    "owner = 'missing'",
    "qty = 25 AND owner = 'owner4'",
    "qty = 25 OR qty = 26",
    "id = 250",
    "id BETWEEN 100 AND 105",
];

fn assert_all_predicates(engine: &Engine) {
    for predicate in PREDICATES {
        assert_same(engine, predicate);
    }
}

#[test]
fn indexed_predicates_match_scan_across_writes() {
    let engine = Engine::new();
    setup(&engine);
    // First pass builds the lazy indexes; second pass reads them.
    assert_all_predicates(&engine);
    assert_all_predicates(&engine);

    // Mutate through every write shape and re-compare.
    for table in ["indexed", "shadow"] {
        engine
            .sql(&format!("UPDATE {table} SET qty = 999 WHERE id = 250"), &[])
            .unwrap();
        engine
            .sql(
                &format!("UPDATE {table} SET owner = 'owner3' WHERE qty = 7"),
                &[],
            )
            .unwrap();
        engine
            .sql(&format!("UPDATE {table} SET qty = NULL WHERE id = 10"), &[])
            .unwrap();
        engine
            .sql(&format!("DELETE FROM {table} WHERE qty = 11"), &[])
            .unwrap();
        engine
            .sql(
                &format!("INSERT INTO {table} (id, qty, owner) VALUES (601, 25, 'owner9')"),
                &[],
            )
            .unwrap();
    }
    assert_same(&engine, "qty = 999");
    assert_same(&engine, "qty = 25");
    assert_same(&engine, "qty IS NULL");
    assert_same(&engine, "qty = 11");
    assert_same(&engine, "owner = 'owner9'");
    assert_all_predicates(&engine);

    // Aggregate over an indexed filter agrees with the scan table.
    let indexed = engine
        .sql("SELECT count(*) AS n FROM indexed WHERE qty = 25", &[])
        .unwrap();
    let shadow = engine
        .sql("SELECT count(*) AS n FROM shadow WHERE qty = 25", &[])
        .unwrap();
    assert_eq!(indexed.rows[0].get("n"), shadow.rows[0].get("n"));
}

#[test]
fn truncate_resets_indexes() {
    let engine = Engine::new();
    setup(&engine);
    assert_all_predicates(&engine);
    engine.sql("TRUNCATE indexed", &[]).unwrap();
    engine.sql("TRUNCATE shadow", &[]).unwrap();
    assert_all_predicates(&engine);
    for table in ["indexed", "shadow"] {
        engine
            .sql(
                &format!("INSERT INTO {table} (id, qty, owner) VALUES (1, 5, 'owner1')"),
                &[],
            )
            .unwrap();
    }
    assert_same(&engine, "qty = 5");
    assert_same(&engine, "qty IS NOT NULL");
}

#[test]
fn persistent_reopen_keeps_index_semantics() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("value-index.db");
    {
        let engine = Engine::open(&path).unwrap();
        setup(&engine);
        assert_all_predicates(&engine);
        engine
            .sql("UPDATE indexed SET qty = 999 WHERE id = 250", &[])
            .unwrap();
        engine
            .sql("UPDATE shadow SET qty = 999 WHERE id = 250", &[])
            .unwrap();
    }

    // PRIMARY KEY plus both explicit btree indexes are complete durable
    // posting sets. The scan-only shadow table has no durable value index.
    assert_eq!(persisted_index_count(&path, "indexed", "id"), 500);
    assert_eq!(persisted_index_count(&path, "indexed", "qty"), 500);
    assert_eq!(persisted_index_count(&path, "indexed", "owner"), 500);
    assert_eq!(persisted_index_definition_count(&path, "shadow", "id"), 0);

    // Reopen and indexed reads must only hydrate the compact postings. A
    // rebuild would insert all 1,500 rows again and fire this audit trigger.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE _btree_rebuild_audit (writes INTEGER NOT NULL);
             CREATE TRIGGER _btree_rebuild_audit_insert
             AFTER INSERT ON _btree_index_entries
             BEGIN
                 INSERT INTO _btree_rebuild_audit (writes) VALUES (1);
             END;",
        )
        .unwrap();
    }
    let engine = Engine::open(&path).unwrap();
    assert_same(&engine, "qty = 999");
    assert_all_predicates(&engine);
    let rebuild_writes: i64 = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM _btree_rebuild_audit", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rebuild_writes, 0, "reopen rebuilt durable btree postings");

    // Writes after reopen keep the rebuilt indexes in sync.
    for table in ["indexed", "shadow"] {
        engine
            .sql(&format!("DELETE FROM {table} WHERE qty = 25"), &[])
            .unwrap();
    }
    assert_same(&engine, "qty = 25");
    assert_all_predicates(&engine);
}

fn seed_engine_meta(path: &std::path::Path) {
    let engine = Engine::open(path).unwrap();
    engine
        .sql(
            "CREATE TABLE engine_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            &[],
        )
        .unwrap();
    for index in 1..=20 {
        engine
            .sql(
                "INSERT INTO engine_meta (key, value) VALUES ($1, $2)",
                &[
                    uqa_sql::SQLParam::scalar(Value::Str(format!("key-{index}"))),
                    uqa_sql::SQLParam::scalar(Value::Str(format!("value-{index}"))),
                ],
            )
            .unwrap();
    }
}

fn simulate_legacy_btree_inconsistency(path: &std::path::Path) {
    // Recreate both historical structural failure shapes: a dangling posting
    // after delete and a missing posting after insert.
    // The schema version is lowered only to model a database last written by
    // the pre-atomic persistent-index implementation.
    let connection = rusqlite::Connection::open(path).unwrap();
    connection
        .execute_batch(
            "DROP TRIGGER IF EXISTS _btree_documents_delete;
             DROP TRIGGER IF EXISTS _btree_entries_document_insert;
             DROP TRIGGER IF EXISTS _btree_entries_document_update;
             DROP TRIGGER IF EXISTS _btree_documents_doc_id_update;
             CREATE TABLE _btree_repair_audit (operation TEXT NOT NULL);
             CREATE TRIGGER test_btree_repair_delete
                 AFTER DELETE ON _btree_index_entries
                 BEGIN
                     INSERT INTO _btree_repair_audit(operation) VALUES ('delete');
                 END;
             CREATE TRIGGER test_btree_repair_insert
                 AFTER INSERT ON _btree_index_entries
                 BEGIN
                     INSERT INTO _btree_repair_audit(operation) VALUES ('insert');
                 END;",
        )
        .unwrap();
    let doc_id = |key: &str| -> i64 {
        connection
            .query_row(
                "SELECT doc_id FROM _documents
                 WHERE table_name = 'public.engine_meta'
                   AND json_extract(body, '$.key') = ?1",
                [key],
                |row| row.get(0),
            )
            .unwrap()
    };
    connection
        .execute(
            "DELETE FROM _documents
             WHERE table_name = 'public.engine_meta' AND doc_id = ?1",
            [doc_id("key-19")],
        )
        .unwrap();
    connection
        .execute(
            "DELETE FROM _btree_index_entries
             WHERE table_name = 'public.engine_meta'
               AND field = 'key' AND doc_id = ?1",
            [doc_id("key-17")],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE _metadata SET value = '20' WHERE key = 'schema_version'",
            [],
        )
        .unwrap();
    connection
        .execute("DELETE FROM _btree_repair_audit", [])
        .unwrap();
}

fn assert_repaired_queries(path: &std::path::Path) {
    let engine = Engine::open(path).unwrap();
    let deleted = engine
        .sql("SELECT value FROM engine_meta WHERE key = 'key-19'", &[])
        .unwrap();
    assert!(deleted.rows.is_empty());
    for index in [17, 18] {
        let result = engine
            .sql(
                &format!("SELECT value FROM engine_meta WHERE key = 'key-{index}'"),
                &[],
            )
            .unwrap();
        assert_eq!(
            result.rows[0].get("value"),
            Some(&Value::Str(format!("value-{index}")))
        );
    }
}

fn assert_sparse_repair(path: &std::path::Path) {
    let connection = rusqlite::Connection::open(path).unwrap();
    let document_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM _documents
             WHERE table_name = 'public.engine_meta'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        persisted_index_count(path, "engine_meta", "key"),
        document_count
    );
    let audit_connection = rusqlite::Connection::open(path).unwrap();
    let mut audit_statement = audit_connection
        .prepare(
            "SELECT operation, COUNT(*) FROM _btree_repair_audit
             GROUP BY operation ORDER BY operation",
        )
        .unwrap();
    let repaired_rows = audit_statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        repaired_rows,
        vec![("delete".into(), 1), ("insert".into(), 1)]
    );
}

fn assert_delete_guard_without_foreign_keys(path: &std::path::Path) {
    // The v21 delete guard is independent of SQLite's optional foreign-key
    // setting, so even a direct storage-level delete cannot leave the exact
    // dangling-posting state from the crash report.
    let connection = rusqlite::Connection::open(path).unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = OFF;")
        .unwrap();
    let cascaded_doc_id: i64 = connection
        .query_row(
            "SELECT doc_id FROM _documents
             WHERE table_name = 'public.engine_meta'
               AND json_extract(body, '$.key') = 'key-16'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    connection
        .execute(
            "DELETE FROM _documents
             WHERE table_name = 'public.engine_meta' AND doc_id = ?1",
            [cascaded_doc_id],
        )
        .unwrap();
    let dangling: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM _btree_index_entries
             WHERE table_name = 'public.engine_meta' AND doc_id = ?1",
            [cascaded_doc_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(dangling, 0);
}

fn remove_key_posting(path: &std::path::Path, key: &str) {
    // If an external writer deletes a posting itself, loading the durable
    // index still verifies its document-id support and rebuilds the in-memory
    // accelerator from authoritative documents instead of returning a false
    // negative or surfacing an invalid access-path document.
    let connection = rusqlite::Connection::open(path).unwrap();
    connection
        .execute(
            "DELETE FROM _btree_index_entries
             WHERE table_name = 'public.engine_meta'
               AND field = 'key'
               AND doc_id = (
                   SELECT doc_id FROM _documents
                    WHERE table_name = 'public.engine_meta'
                      AND json_extract(body, '$.key') = ?1
               )",
            [key],
        )
        .unwrap();
}

#[test]
fn schema_21_rebuilds_historically_inconsistent_btree_postings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy-inconsistent-btree.db");
    seed_engine_meta(&path);
    simulate_legacy_btree_inconsistency(&path);
    assert_repaired_queries(&path);
    assert_sparse_repair(&path);
    assert_delete_guard_without_foreign_keys(&path);
    remove_key_posting(&path, "key-18");

    let engine = Engine::open(&path).unwrap();
    let repaired = engine
        .sql("SELECT value FROM engine_meta WHERE key = 'key-18'", &[])
        .unwrap();
    assert_eq!(
        repaired.rows[0].get("value"),
        Some(&Value::Str("value-18".into()))
    );
}

#[test]
fn persistent_btree_tracks_rollback_savepoint_and_truncate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("value-index-transactions.db");
    let engine = Engine::open(&path).unwrap();
    setup(&engine);

    engine.begin().unwrap();
    engine
        .sql("UPDATE indexed SET qty = 999 WHERE id = 250", &[])
        .unwrap();
    engine
        .sql("UPDATE shadow SET qty = 999 WHERE id = 250", &[])
        .unwrap();
    assert_same(&engine, "qty = 999");
    engine.rollback().unwrap();
    assert_same(&engine, "qty = 999");
    assert!(ids(&engine
        .sql("SELECT id FROM indexed WHERE qty = 999", &[])
        .unwrap())
    .is_empty());

    engine.begin().unwrap();
    engine.savepoint("before_update").unwrap();
    engine
        .sql("UPDATE indexed SET owner = 'rolled-back' WHERE id = 1", &[])
        .unwrap();
    engine
        .sql("UPDATE shadow SET owner = 'rolled-back' WHERE id = 1", &[])
        .unwrap();
    assert_same(&engine, "owner = 'rolled-back'");
    engine.rollback_to_savepoint("before_update").unwrap();
    assert_same(&engine, "owner = 'rolled-back'");
    engine.commit().unwrap();

    engine.sql("TRUNCATE indexed", &[]).unwrap();
    assert_eq!(persisted_index_definition_count(&path, "indexed", "qty"), 1);
    assert_eq!(persisted_index_count(&path, "indexed", "id"), 0);
    assert_eq!(persisted_index_count(&path, "indexed", "qty"), 0);
    assert_eq!(persisted_index_count(&path, "indexed", "owner"), 0);
}

#[test]
fn persistent_btree_maintains_an_index_while_its_memory_copy_is_cold() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("value-index-cold-write.db");
    {
        let engine = Engine::open(&path).unwrap();
        setup(&engine);
    }

    // A reopen starts with lazy in-memory indexes. Updating by primary key
    // heats `id`, but `owner` remains cold; its durable posting still has to
    // change before this engine is dropped.
    {
        let engine = Engine::open(&path).unwrap();
        engine
            .sql(
                "UPDATE indexed SET owner = 'after-reopen' WHERE id = 1",
                &[],
            )
            .unwrap();
        engine
            .sql("UPDATE shadow SET owner = 'after-reopen' WHERE id = 1", &[])
            .unwrap();
    }

    let engine = Engine::open(&path).unwrap();
    assert_same(&engine, "owner = 'after-reopen'");
    assert_same(&engine, "owner = 'owner1'");
}

#[test]
fn persistent_btree_metadata_follows_ddl_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("value-index-ddl.db");
    let engine = Engine::open(&path).unwrap();
    engine
        .sql(
            "CREATE TABLE messages (id INTEGER PRIMARY KEY, public_id TEXT, body TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE INDEX messages_public_id_idx ON messages USING btree (public_id)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO messages (id, public_id, body) VALUES
             (1, 'm1', 'one'), (2, 'm2', 'two')",
            &[],
        )
        .unwrap();
    assert_eq!(persisted_index_count(&path, "messages", "public_id"), 2);

    engine
        .sql("ALTER TABLE messages RENAME TO archived_messages", &[])
        .unwrap();
    assert_eq!(
        persisted_index_definition_count(&path, "messages", "public_id"),
        0
    );
    assert_eq!(
        persisted_index_count(&path, "archived_messages", "public_id"),
        2
    );

    engine
        .sql(
            "ALTER TABLE archived_messages RENAME COLUMN public_id TO message_id",
            &[],
        )
        .unwrap();
    assert_eq!(
        persisted_index_definition_count(&path, "archived_messages", "public_id"),
        0
    );
    assert_eq!(
        persisted_index_count(&path, "archived_messages", "message_id"),
        2
    );
    let result = engine
        .sql(
            "SELECT id FROM archived_messages WHERE message_id IN ('m1', 'm2') ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(ids(&result), vec![1, 2]);

    engine
        .sql("DROP INDEX messages_public_id_idx", &[])
        .unwrap();
    assert_eq!(
        persisted_index_definition_count(&path, "archived_messages", "message_id"),
        0
    );
    assert_eq!(persisted_index_count(&path, "archived_messages", "id"), 2);

    engine.sql("DROP TABLE archived_messages", &[]).unwrap();
    let remaining: i64 = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM _btree_indexes", [], |row| row.get(0))
        .unwrap();
    assert_eq!(remaining, 0);
}

#[test]
fn schema_v10_database_backfills_btree_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("value-index-upgrade.db");
    {
        let engine = Engine::open(&path).unwrap();
        setup(&engine);
    }
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP TABLE _btree_index_entries;
             DROP TABLE _btree_indexes;
             UPDATE _metadata SET value = '10' WHERE key = 'schema_version';",
        )
        .unwrap();
    }

    let engine = Engine::open(&path).unwrap();
    assert_same(&engine, "owner = 'owner3'");
    assert_same(&engine, "qty IN (1, 2, 3)");
    assert_same(&engine, "id BETWEEN 1 AND 500");
    assert_eq!(persisted_index_count(&path, "indexed", "id"), 500);
    assert_eq!(persisted_index_count(&path, "indexed", "qty"), 500);
    assert_eq!(persisted_index_count(&path, "indexed", "owner"), 500);
    let version: i64 = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM _metadata WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, i64::from(CURRENT_SCHEMA_VERSION));
}
