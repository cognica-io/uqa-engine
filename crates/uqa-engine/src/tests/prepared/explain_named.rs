//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::tests::providers::Provider;
use uqa_core::Value;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn named_explain_borrows_the_selected_generic_executable(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("explain-selected.db"));
    let rows = std::iter::repeat_n("(1)", 8192)
        .collect::<Vec<_>>()
        .join(",");
    engine
        .sql(
            &format!("PREPARE large_explanation AS VALUES {rows}; SET plan_cache_mode = force_generic_plan"),
            &[],
        )
        .unwrap();
    check_selected_borrow(&engine);
    engine.sql("DEALLOCATE large_explanation", &[]).unwrap();
    engine.close().unwrap();
}

fn check_selected_borrow(engine: &crate::Engine) {
    let statement = "EXPLAIN (COSTS FALSE, MEMORY, FORMAT JSON) EXECUTE large_explanation";
    engine.sql(statement, &[]).unwrap();
    let selected = engine.session.prepared.read()["large_explanation"]
        .plan
        .clone()
        .unwrap();
    let retained = engine.query_retention_control().unwrap();
    let baseline = retained.memory().used();
    let occupied = retained
        .memory()
        .reserve(retained.memory().available() - 64 * 1024)
        .unwrap();
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        output = Some(engine.sql(statement, &[]).unwrap());
    });
    let output = output.unwrap();
    let Some(Value::Json(text)) = output.value_at(0, 0) else {
        panic!("JSON explanation");
    };
    let document: serde_json::Value = serde_json::from_str(text).unwrap();
    assert!(document[0]["Planning"]["Memory Used"].as_u64().unwrap() >= 1024);
    assert!(allocation.bytes_total < 256 * 1024, "{allocation:?}");
    drop(output);
    drop(occupied);
    assert_eq!(retained.memory().used(), baseline);
    let statements = engine.session.prepared.read();
    let cached = &statements["large_explanation"];
    assert!(std::sync::Arc::ptr_eq(
        cached.plan.as_ref().unwrap(),
        &selected
    ));
    assert_eq!((cached.generic_plans, cached.custom_plans), (2, 0));
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn utility_explain_borrows_its_query_until_optimization(#[case] provider: Provider) {
    use uqa_sql::plan::{ExecutablePlanOptimizer, UnifiedPlan};
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("explain-utility-owner.db"));
    engine.sql("BEGIN", &[]).unwrap();
    let rows = std::iter::repeat_n("(1)", 8192)
        .collect::<Vec<_>>()
        .join(",");
    let mut baseline = None;
    for prefix in [
        "",
        "DECLARE explained_cursor CURSOR FOR ",
        "CREATE TABLE explained_table AS ",
        "CREATE MATERIALIZED VIEW explained_view AS ",
    ] {
        let sql = format!("EXPLAIN (COSTS FALSE, FORMAT JSON) {prefix}VALUES {rows}");
        let plan = engine
            .plan_for_execution(
                UnifiedPlan::lower(uqa_sql::compile(&sql).unwrap().remove(0)),
                &[],
            )
            .unwrap();
        let allocation = allocation_counter::measure(|| {
            let output = uqa_execution::statement::compiled::execute_selected_plan(
                &engine.compiled_statement_context(),
                &plan,
                &[],
            )
            .unwrap();
            assert!(!output.rows.is_empty());
        });
        if let Some(baseline) = baseline {
            assert!(
                allocation.bytes_total < baseline + 128 * 1024,
                "{prefix}: {allocation:?}, query baseline={baseline}"
            );
        } else {
            baseline = Some(allocation.bytes_total);
        }
    }
    engine.sql("ROLLBACK", &[]).unwrap();
    engine.close().unwrap();
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn named_explain_prepares_private_sources_and_preserves_cache_recovery(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("explain-private-sources.db");
    let engine = provider.open(&path);
    engine.sql("CREATE SEQUENCE explanation_ticks; CREATE TABLE explanation_writes (n bigint); CREATE VIEW explanation_input AS SELECT nextval('explanation_ticks') AS n; PREPARE read_explanation AS WITH q AS MATERIALIZED (SELECT n FROM explanation_input) SELECT n FROM q; PREPARE write_explanation AS INSERT INTO explanation_writes SELECT n FROM explanation_input RETURNING n; SET plan_cache_mode = force_generic_plan", &[]).unwrap();
    let retained = engine.query_retention_control().unwrap();
    for (index, name) in ["read_explanation", "write_explanation"]
        .into_iter()
        .enumerate()
    {
        engine
            .sql(
                &format!("EXPLAIN (MEMORY, FORMAT JSON) EXECUTE {name}"),
                &[],
            )
            .unwrap();
        let selected = engine.session.prepared.read()[name].plan.clone().unwrap();
        let original = serde_json::to_value(selected.as_ref()).unwrap();
        engine
            .sql("BEGIN; SAVEPOINT retry_explanation", &[])
            .unwrap();
        let baseline = retained.memory().used();
        let occupied = retained
            .memory()
            .reserve(retained.memory().available())
            .unwrap();
        let analyze = format!("EXPLAIN (MEMORY, ANALYZE, FORMAT JSON) EXECUTE {name}");
        let error = engine.sql(&analyze, &[]).unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some("53200"),
            "{provider:?}, {name}: {error}"
        );
        drop(occupied);
        assert_eq!(retained.memory().used(), baseline);
        assert!(engine.transaction_failed());
        engine.sql("ROLLBACK TO retry_explanation", &[]).unwrap();
        let result = engine.sql(&analyze, &[]).unwrap();
        let Some(Value::Json(text)) = result.value_at(0, 0) else {
            panic!("JSON explanation");
        };
        let document: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(document[0]["Plan"]["Actual Rows"], 1);
        assert_eq!(
            engine
                .sql("SELECT currval('explanation_ticks')", &[])
                .unwrap()
                .value_at(0, 0),
            Some(&Value::Int(index as i64 + 1))
        );
        drop(result);
        engine.sql("COMMIT", &[]).unwrap();
        assert_eq!(serde_json::to_value(selected.as_ref()).unwrap(), original);
    }
    engine
        .sql("EXPLAIN EXECUTE write_explanation", &[])
        .unwrap();
    let original = engine.session.prepared.read()["write_explanation"]
        .plan
        .clone()
        .unwrap();
    engine.sql("CREATE OR REPLACE VIEW explanation_input AS SELECT nextval('explanation_ticks') + 100 AS n; EXPLAIN (ANALYZE, MEMORY, FORMAT JSON) EXECUTE write_explanation", &[]).unwrap();
    assert!(engine.session.prepared.read()["write_explanation"]
        .plan
        .as_ref()
        .is_none_or(|current| !std::sync::Arc::ptr_eq(current, &original)));
    assert_eq!(
        engine
            .sql("SELECT max(n) FROM explanation_writes", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(103))
    );
    drop(original);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(retained.memory().used(), 0);
    if !matches!(provider, Provider::Memory) {
        let engine = provider.open(&path);
        assert_eq!(
            engine
                .sql("SELECT count(*), max(n) FROM explanation_writes", &[])
                .unwrap()
                .value_at(0, 0),
            Some(&Value::Int(2))
        );
        engine.close().unwrap();
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn named_explanations_retain_their_prepared_inputs_and_native_effects(#[case] provider: Provider) {
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/explain_named_inputs.expected.json"
    ))
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    for case in reference["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let engine = provider.open(&directory.path().join(format!("{name}.db")));
        engine
            .sql(&format!("BEGIN; {}", case["setup"].as_str().unwrap()), &[])
            .unwrap();
        let result = engine
            .sql(case["sql"].as_str().unwrap(), &[])
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let Some(Value::Json(text)) = result.value_at(0, 0) else {
            panic!("{name}: EXPLAIN JSON carrier");
        };
        let document: serde_json::Value = serde_json::from_str(text).unwrap();
        let plan = &document[0]["Plan"];
        assert_eq!(
            plan["Plan Rows"].as_f64(),
            case["rows"].as_f64(),
            "{name}: {plan}"
        );
        assert_eq!(plan["Plan Width"], case["width"], "{name}: {plan}");
        assert_eq!(
            plan.get("Actual Rows").and_then(serde_json::Value::as_f64),
            case["actual_rows"].as_f64(),
            "{name}: {plan}"
        );
        if let Some(sql) = case["after"].as_str() {
            assert_eq!(
                engine.sql(sql, &[]).unwrap().value_at(0, 0),
                Some(&Value::Int(case["next_value"].as_i64().unwrap())),
                "{name}"
            );
        }
        engine.sql("ROLLBACK", &[]).unwrap();
        engine.close().unwrap();
    }
}
