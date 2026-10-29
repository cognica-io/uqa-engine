//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::tests::providers::Provider;
use uqa_core::Value;
use uqa_sql::{prepared::planning::PreparedPlanProvider, SQLParam};

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_prepared_executions_share_cached_plans_until_replacement(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("shared-plans.db"));
    engine
        .sql(
            "CREATE TABLE shared_rows (id integer); INSERT INTO shared_rows VALUES (1),(2),(3)",
            &[],
        )
        .unwrap();
    engine.sql(
        "BEGIN ISOLATION LEVEL REPEATABLE READ; SET plan_cache_mode=force_generic_plan; \
         PREPARE shared(integer) AS SELECT id, $1 AS marker FROM shared_rows WHERE id >= $1 ORDER BY id; \
         EXECUTE shared(1)",
        &[],
    ).unwrap();
    let retained = engine.session.prepared.read()["shared"]
        .plan
        .clone()
        .unwrap();
    assert!(retained.reserved_bytes() > 0);
    for lower in [1, 3, 2] {
        let observed = engine.lookup_prepared("shared").unwrap();
        assert!(observed.ptr_eq(&retained));
        let rows = engine
            .sql(&format!("EXECUTE shared({lower})"), &[])
            .unwrap();
        let actual = rows
            .rows
            .iter()
            .map(|row| (row["id"].clone(), row["marker"].clone()))
            .collect::<Vec<_>>();
        let expected = (lower..=3)
            .map(|id| (Value::Int(id), Value::Int(lower)))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected, "{provider:?}");
        let selected = engine
            .plan_for_execution("shared", &[SQLParam::Scalar(Value::Int(lower))])
            .unwrap()
            .unwrap();
        assert!(selected.ptr_eq(&retained));
        assert!(engine.session.prepared.read()["shared"]
            .plan
            .as_ref()
            .unwrap()
            .ptr_eq(&retained));
    }
    {
        let entries = engine.session.prepared.read();
        let entry = &entries["shared"];
        assert_eq!((entry.generic_plans, entry.custom_plans), (7, 0));
    }
    engine.sql("DEALLOCATE shared; PREPARE shared(integer) AS SELECT $1 + 10 AS value; EXECUTE shared(2)", &[]).unwrap();
    let replacement = engine.session.prepared.read()["shared"]
        .plan
        .clone()
        .unwrap();
    assert!(!replacement.ptr_eq(&retained));
    assert_eq!(retained.strong_count(), 1);
    let control = engine.query_retention_control().unwrap();
    let before = control.memory().used();
    let reserved = retained.reserved_bytes();
    drop(retained);
    assert_eq!(control.memory().used(), before - reserved);
    engine.sql("DEALLOCATE ALL", &[]).unwrap();
    assert_eq!(replacement.strong_count(), 1);
    engine.sql("COMMIT", &[]).unwrap();
    engine.close().unwrap();
    drop(engine);
    drop(replacement);
    assert_eq!(control.memory().used(), 0);
}
