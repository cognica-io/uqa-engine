//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` generated-column dependency selection, including trigger and MERGE descriptors.

use super::*;

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn generated_updates_follow_postgresql_target_dependencies(#[case] provider: usize) {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("generated-updates.db");
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
    let result = engine
        .sql(
            include_str!("../../../../tests/parity/pg18/generated_update_dependencies.sql"),
            &[],
        )
        .unwrap();
    // Captured independently with PostgreSQL 18.4: disabled/column-specific BEFORE triggers still prevent dependency pruning, and MERGE uses the union of its UPDATE targets.
    let expected = [
        ("before_row", 1, 7, 1, 107),
        ("column_trigger", 1, 7, 1, 107),
        ("conflict", 1, 7, 10, 7),
        ("defaulted", 1, 7, 0, 7),
        ("disabled", 1, 7, 1, 107),
        ("merge", 1, 7, 1, 107),
        ("ordinary", 1, 7, 1, 7),
        ("partition", 11, 7, 0, 1107),
        ("partition_trigger", 12, 9, 0, 1109),
        ("referential", 2, 7, 0, 7),
    ];
    assert_eq!(result.rows.len(), expected.len());
    for (row, (step, id, source, unrelated, stored)) in result.rows.iter().zip(expected) {
        assert_eq!(row["step"], Value::Str(step.into()));
        assert_eq!(int(row, "id"), id);
        assert_eq!(int(row, "source"), source);
        assert_eq!(int(row, "unrelated"), unrelated);
        assert_eq!(row.get("stored"), Some(&Value::Int(stored)), "{step}");
    }
    let result = engine
        .sql(
            "UPDATE generated_dependencies.ordinary SET source=source RETURNING stored",
            &[],
        )
        .unwrap();
    assert_eq!(result.value_at(0, 0), Some(&Value::Int(107)));
    engine
        .sql(
            "BEGIN; UPDATE generated_dependencies.ordinary SET source=9; ROLLBACK",
            &[],
        )
        .unwrap();
    let verify = |engine: &Engine| {
        let result = engine
            .sql(
                "SELECT source, stored FROM generated_dependencies.ordinary",
                &[],
            )
            .unwrap();
        assert_eq!(result.value_at(0, 0), Some(&Value::Int(7)));
        assert_eq!(result.value_at(0, 1), Some(&Value::Int(107)));
    };
    verify(&engine);
    if provider != 0 {
        verify(&engine.new_session().unwrap());
        drop(engine);
        verify(&open());
    }
}
