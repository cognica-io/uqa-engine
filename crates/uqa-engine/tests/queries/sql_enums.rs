//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for user-defined enum types: catalogs, labels, ordering, casts, functions, transactions and dependencies.

use std::path::Path;
use std::sync::Arc;

use uqa_engine::Engine;
use uqa_storage::PersistentStorageProvider;
use uqa_storage_redb::RedbStorage;
use uqa_storage_sqlite::SQLiteKeyValueStorage;

fn verify_enums(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../tests/parity/pg18/enum_types_oracle.expected.json"),
    );
}

#[test]
fn enum_semantics_match_postgresql_memory() {
    verify_enums(&Engine::new());
}

fn open_sqlite(path: &Path) -> Engine {
    Engine::open(path).unwrap()
}

fn open_sqlite_key_value(path: &Path) -> Engine {
    let storage: Arc<dyn PersistentStorageProvider> =
        Arc::new(SQLiteKeyValueStorage::open(path).unwrap());
    Engine::from_persistent_provider(storage).unwrap()
}

fn open_redb(path: &Path) -> Engine {
    let storage: Arc<dyn PersistentStorageProvider> = Arc::new(RedbStorage::open(path).unwrap());
    Engine::from_persistent_provider(storage).unwrap()
}

#[test]
fn enum_semantics_match_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    verify_enums(&open_sqlite(&directory.path().join("enums.db")));
}

#[test]
fn enum_semantics_match_postgresql_sqlite_key_value() {
    let directory = tempfile::tempdir().unwrap();
    verify_enums(&open_sqlite_key_value(&directory.path().join("enums.db")));
}

#[test]
fn enum_semantics_match_postgresql_redb() {
    let directory = tempfile::tempdir().unwrap();
    verify_enums(&open_redb(&directory.path().join("enums.redb")));
}

#[rstest::rstest]
#[case::memory(None)]
#[case::sqlite(Some(open_sqlite as fn(&Path) -> Engine))]
#[case::sqlite_key_value(Some(open_sqlite_key_value as fn(&Path) -> Engine))]
#[case::redb(Some(open_redb as fn(&Path) -> Engine))]
fn enum_partition_key_catalog_output_matches_postgresql(#[case] open: Option<fn(&Path) -> Engine>) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("enum-partition-output.db");
    let engine = open.map_or_else(Engine::new, |open| open(&path));
    let mut reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/parity/pg18/enum_partition_key_oracle.expected.json"
    ))
    .unwrap();
    crate::pg18_oracle::verify(&engine, &reference.to_string());
    assert_partition_expression_label(&engine, "neutral");
    engine
        .sql(
            "BEGIN; ALTER TYPE current_partition_mood RENAME VALUE 'neutral' TO 'temporary'",
            &[],
        )
        .unwrap();
    assert_partition_expression_label(&engine, "temporary");
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_partition_expression_label(&engine, "neutral");
    drop(engine);
    if let Some(open) = open {
        let ids = reference["reopen_ids"].as_array().unwrap().clone();
        reference["cases"]
            .as_array_mut()
            .unwrap()
            .retain(|case| ids.contains(&case["id"]));
        let reopened = open(&path);
        crate::pg18_oracle::verify(&reopened, &reference.to_string());
        assert_partition_expression_label(&reopened, "neutral");
    }
}

fn assert_partition_expression_label(engine: &Engine, label: &str) {
    let rows = text_rows(engine, "SELECT partexprs::text FROM pg_partitioned_table WHERE partrelid = 'enum_partition_rows'::regclass");
    let expression = rows[0][0].as_ref().unwrap();
    assert!(
        expression.contains(&format!("'{label}'::current_partition_mood")),
        "partition expression uses stale type or label metadata: {expression}"
    );
}

/// Every result row of one statement as `PostgreSQL` text, NULL as `None`.
fn text_rows(engine: &Engine, sql: &str) -> Vec<Vec<Option<String>>> {
    let case = crate::pg18_oracle::run_case(engine, sql);
    assert!(case["error"].is_null(), "{sql}: {}", case["error"]);
    case["results"][0]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            row.as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .collect()
}

fn text(values: &[Option<&str>]) -> Vec<Option<String>> {
    values
        .iter()
        .map(|value| value.map(str::to_owned))
        .collect()
}

/// Stored values keep their label identity and declaration order across a label rename and reopen, and new labels continue the persisted order.
fn verify_reopen(open: fn(&Path) -> Engine, file: &str) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(file);
    {
        let engine = open(&path);
        for statement in [
            "CREATE TYPE mood AS ENUM ('sad', 'happy')",
            "ALTER TYPE mood ADD VALUE 'ok' BEFORE 'happy'",
            "CREATE TABLE person (id integer PRIMARY KEY, m mood, ms mood[])",
            "INSERT INTO person VALUES (1, 'happy', '{sad,ok}'), (2, 'sad', NULL), (3, 'ok', '{happy}')",
            "ALTER TYPE mood RENAME VALUE 'ok' TO 'neutral'",
        ] {
            engine.sql(statement, &[]).unwrap();
        }
    }
    let engine = open(&path);
    assert_eq!(
        text_rows(&engine, "SELECT id, m, ms FROM person ORDER BY m, id"),
        [
            text(&[Some("2"), Some("sad"), None]),
            text(&[Some("3"), Some("neutral"), Some("{happy}")]),
            text(&[Some("1"), Some("happy"), Some("{sad,neutral}")]),
        ]
    );
    assert_eq!(
        text_rows(
            &engine,
            "SELECT enumlabel, enumsortorder FROM pg_enum WHERE enumtypid = 'mood'::regtype ORDER BY enumsortorder"
        ),
        [
            text(&[Some("sad"), Some("1")]),
            text(&[Some("neutral"), Some("1.5")]),
            text(&[Some("happy"), Some("2")]),
        ]
    );
    engine
        .sql("ALTER TYPE mood ADD VALUE 'calm' AFTER 'sad'", &[])
        .unwrap();
    engine
        .sql("INSERT INTO person VALUES (4, 'calm', '{calm,happy}')", &[])
        .unwrap();
    assert_eq!(
        text_rows(
            &engine,
            "SELECT enum_range(NULL::mood), (SELECT count(*) FROM person WHERE m < 'neutral'), (SELECT a.typname FROM pg_type t JOIN pg_type a ON a.oid = t.typarray WHERE t.typname = 'mood')"
        ),
        [text(&[Some("{sad,calm,neutral,happy}"), Some("2"), Some("_mood")])]
    );
    drop(engine);
    let engine = open(&path);
    assert_eq!(
        text_rows(
            &engine,
            "SELECT id FROM person WHERE m > 'sad' AND ms @> '{happy}' ORDER BY m"
        ),
        [text(&[Some("4")]), text(&[Some("3")])]
    );
    let error = engine.sql("DROP TYPE mood", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("2BP01"), "{error}");
}

#[test]
fn enum_definitions_and_values_survive_reopen_sqlite() {
    verify_reopen(open_sqlite, "enums.db");
}

#[test]
fn enum_definitions_and_values_survive_reopen_sqlite_key_value() {
    verify_reopen(open_sqlite_key_value, "enums.db");
}

#[test]
fn enum_definitions_and_values_survive_reopen_redb() {
    verify_reopen(open_redb, "enums.redb");
}
