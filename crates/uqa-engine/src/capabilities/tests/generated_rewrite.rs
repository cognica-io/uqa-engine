//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated-column rewrites bind catalog expressions independently of row count.

use super::{Engine, Value, CATALOG_CAPTURES};
use std::sync::{atomic::Ordering, Arc};

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn generated_rewrite_catalog_captures_do_not_grow_with_rows(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("generated-rewrite.db");
    let engine = match provider {
        0 => Engine::new(),
        1 => Engine::open(&path).unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    };
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, Ordering::Release);
    engine.sql("SET work_mem='64kB'", &[]).unwrap();
    let captures = [1, 33].map(|rows| {
        engine.sql(&format!("CREATE TABLE items(id text PRIMARY KEY, body text); INSERT INTO items SELECT 'item-' || lpad(i::text,3,'0'),repeat('x',4000) FROM generate_series(1,{rows}) AS g(i)"), &[]).unwrap();
        CATALOG_CAPTURES.set(0);
        engine.sql("ALTER TABLE items ADD COLUMN suffix text GENERATED ALWAYS AS (right(lower(id),4)) STORED", &[]).unwrap();
        let captures = CATALOG_CAPTURES.get();
        let result = engine.sql("SELECT count(*),min(suffix),max(suffix),sum(length(body)) FROM items", &[]).unwrap();
        assert_eq!(result.value_at(0,0), Some(&Value::Int(rows)));
        assert_eq!(result.value_at(0,1), Some(&Value::Str("-001".into())));
        assert_eq!(result.value_at(0,2), Some(&Value::Str(format!("-{rows:03}"))));
        assert_eq!(result.value_at(0,3), Some(&Value::Int(rows * 4000)));
        engine.sql("DROP TABLE items", &[]).unwrap();
        captures
    });
    assert!(
        captures[1] <= captures[0],
        "catalog captures grow with generated rewrite rows: {captures:?}"
    );
    verify_rewrite_semantics(&engine);
}

fn verify_rewrite_semantics(engine: &Engine) {
    engine.sql("CREATE TABLE typed(id int PRIMARY KEY, source char(4)); INSERT INTO typed VALUES(1,'a'),(2,'bc'),(3,NULL); ALTER TABLE typed ADD COLUMN as_text text GENERATED ALWAYS AS (source) STORED; CREATE SEQUENCE calls; CREATE FUNCTION folded() RETURNS bigint IMMUTABLE LANGUAGE sql AS 'SELECT nextval(''calls'')'; ALTER TABLE typed ADD COLUMN folded_value bigint GENERATED ALWAYS AS (folded()) STORED", &[]).unwrap();
    let assert_values = |upper: bool| {
        let result = engine
            .sql(
                "SELECT as_text,length(as_text),folded_value FROM typed ORDER BY id",
                &[],
            )
            .unwrap();
        for (index, (value, length)) in [
            (
                Value::Str(if upper { "A" } else { "a" }.into()),
                Value::Int(1),
            ),
            (
                Value::Str(if upper { "BC" } else { "bc" }.into()),
                Value::Int(2),
            ),
            (Value::Null, Value::Null),
        ]
        .iter()
        .enumerate()
        {
            assert_eq!(result.value_at(index, 0), Some(value));
            assert_eq!(result.value_at(index, 1), Some(length));
            assert_eq!(result.value_at(index, 2), Some(&Value::Int(2)));
        }
    };
    // PostgreSQL 18.4 folds once during validation and once for the rewrite plan, independently of row count.
    assert_values(false);
    assert_eq!(
        engine
            .sql("SELECT last_value FROM calls", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(2))
    );
    engine.sql("BEGIN; SAVEPOINT before_rewrite; ALTER TABLE typed ALTER COLUMN as_text SET EXPRESSION AS (upper(source))", &[]).unwrap();
    assert_values(true);
    engine.sql("ROLLBACK TO before_rewrite", &[]).unwrap();
    assert_values(false);
    engine.sql("ROLLBACK", &[]).unwrap();
    let error = engine
        .sql(
            "ALTER TABLE typed ADD COLUMN bad int GENERATED ALWAYS AS (10/(id-2)) STORED",
            &[],
        )
        .unwrap_err();
    assert!(error.to_string().contains("division by zero"));
    assert_values(false);
    assert!(engine.sql("SELECT bad FROM typed", &[]).is_err());
}

#[test]
fn empty_generated_rewrites_prepare_their_constant_expressions() {
    let engine = Engine::new();
    engine.sql("CREATE SEQUENCE empty_calls; CREATE FUNCTION empty_folded() RETURNS bigint IMMUTABLE LANGUAGE sql AS 'SELECT nextval(''empty_calls'')'; CREATE TABLE empty_rewrite(id int); ALTER TABLE empty_rewrite ADD COLUMN value bigint GENERATED ALWAYS AS(empty_folded()) STORED", &[]).unwrap();
    assert_eq!(
        engine
            .sql("SELECT last_value FROM empty_calls", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(2))
    );
    engine
        .sql(
            "ALTER TABLE empty_rewrite ALTER COLUMN value SET EXPRESSION AS(empty_folded()+1)",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine
            .sql("SELECT last_value FROM empty_calls", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(4))
    );
}
