//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Joined UPDATE keeps volatile, prepared, lazy and transactional evaluation.

use super::*;
use std::sync::Arc;

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn update_predicate_preparation_preserves_postgresql_evaluation(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("predicates.db");
    let open = || match provider {
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
    let engine = open();
    let fixture = include_str!("../../../../tests/parity/pg18/update_from_predicate.sql");
    let mut observations = Vec::new();
    // This fixed fixture has no embedded semicolons; send the same individual statements as psql.
    for statement in fixture
        .split(';')
        .map(str::trim)
        .filter(|sql| !sql.is_empty())
    {
        let result = engine.sql(statement, &[]).unwrap();
        if !result.rows.is_empty() {
            observations.push(result);
        }
    }
    // Independently captured with PostgreSQL 18.4, including the rolled-back prepared execution and empty source.
    assert_eq!(observations.len(), 5);
    for (result, expected) in observations.iter().zip([
        vec![100, 35, 5],
        vec![57],
        vec![100, 35, 7],
        vec![100, 35, 7],
    ]) {
        assert_eq!(result.rows.len(), 1);
        for (position, value) in expected.into_iter().enumerate() {
            assert_eq!(result.value_at(0, position), Some(&Value::Int(value)));
        }
    }
    let result = observations.last().unwrap();
    assert_eq!(result.rows.len(), 5);
    for (position, (row, expected)) in result.rows.iter().zip([9, 7, 11, 7, 7]).enumerate() {
        assert_eq!(row["id"], Value::Int(position as i64 + 1));
        assert_eq!(row["v"], Value::Int(expected));
    }
    let sequence = engine
        .sql("SELECT currval('predicate_ticks')", &[])
        .unwrap();
    assert_eq!(sequence.value_at(0, 0), Some(&Value::Int(7)));
    let verify = |engine: &Engine| {
        let result = engine
            .sql(
                "SELECT count(*), sum(v), sum(length(body)) FROM predicate_items",
                &[],
            )
            .unwrap();
        for (position, expected) in [100, 41, 200_000].into_iter().enumerate() {
            assert_eq!(result.value_at(0, position), Some(&Value::Int(expected)));
        }
    };
    verify(&engine);
    if provider != 0 {
        verify(&engine.new_session().unwrap());
        drop(engine);
        verify(&open());
    }
}
