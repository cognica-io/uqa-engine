//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::tests::providers::Provider;
use std::sync::Arc;
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
        "SET plan_cache_mode=force_generic_plan; \
         PREPARE shared(integer) AS SELECT id, $1 AS marker FROM shared_rows WHERE id >= $1 ORDER BY id; \
         EXECUTE shared(1)",
        &[],
    ).unwrap();
    let retained = engine.session.prepared.read()["shared"]
        .plan
        .clone()
        .unwrap();
    let weak = Arc::downgrade(&retained);
    for lower in [1, 3, 2] {
        let observed = engine.lookup_prepared("shared").unwrap();
        assert!(Arc::ptr_eq(&observed, &retained));
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
        assert!(Arc::ptr_eq(&selected, &retained));
        assert!(Arc::ptr_eq(
            engine.session.prepared.read()["shared"]
                .plan
                .as_ref()
                .unwrap(),
            &retained
        ));
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
    assert!(!Arc::ptr_eq(&replacement, &retained));
    assert_eq!(weak.strong_count(), 1);
    drop(retained);
    assert!(weak.upgrade().is_none());
    let weak = Arc::downgrade(&replacement);
    engine.sql("DEALLOCATE ALL", &[]).unwrap();
    assert_eq!(weak.strong_count(), 1);
    engine.close().unwrap();
    drop(engine);
    drop(replacement);
    assert!(weak.upgrade().is_none());
}
