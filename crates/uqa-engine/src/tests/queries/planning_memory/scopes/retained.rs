//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::{plan::QueryPlan, SQLParam};

fn query(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    *query
}

#[rstest::rstest]
#[case::memory_plan(Provider::Memory, "plan")]
#[case::memory_parameter(Provider::Memory, "parameter")]
#[case::memory_scope(Provider::Memory, "scope")]
#[case::sqlite_native_plan(Provider::SQLiteNative, "plan")]
#[case::sqlite_native_parameter(Provider::SQLiteNative, "parameter")]
#[case::sqlite_native_scope(Provider::SQLiteNative, "scope")]
#[case::sqlite_kv_plan(Provider::SQLiteKeyValue, "plan")]
#[case::sqlite_kv_parameter(Provider::SQLiteKeyValue, "parameter")]
#[case::sqlite_kv_scope(Provider::SQLiteKeyValue, "scope")]
#[case::redb_plan(Provider::Redb, "plan")]
#[case::redb_parameter(Provider::Redb, "parameter")]
#[case::redb_scope(Provider::Redb, "scope")]
fn native_retained_cte_inputs_admit_before_copying_and_keep_original_allowance(
    #[case] provider: Provider,
    #[case] input: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("retained-cte-inputs.db"));
    let payload = "x".repeat(4 << 20);
    let plan = query(match input {
        "parameter" => "SELECT $1::text AS value",
        _ => "SELECT 42 AS value",
    });
    let mut plan = plan;
    if input == "plan" {
        let uqa_sql::plan::RelationalPlan::QueryBlock(block) = &mut plan.root else {
            panic!("block")
        };
        block.projections[0].expr = uqa_sql::ScalarExpr::Literal(Value::Str(payload.clone()));
    }
    let params = if input == "parameter" {
        vec![SQLParam::Scalar(Value::Str(payload))]
    } else {
        Vec::new()
    };
    let mut scope = crate::capabilities::query_scope::new_for_statement(&engine, None).unwrap();
    if input == "scope" {
        let mut definition = query("WITH unused AS (SELECT 1) SELECT 1").ctes.remove(0);
        definition.name = "x".repeat(4 << 20);
        scope.insert_deferred(definition.into());
    }
    let context = engine.cte_execution_context();
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let before = memory.used();
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let mut result = None;
    let allocated = allocation_counter::measure(|| {
        result = Some(
            context
                .queries
                .retained_query(&plan, &params, &scope)
                .map(drop),
        );
    });
    assert!(
        allocated.bytes_total < 256 << 10,
        "{provider:?} {input}: {allocated:?}"
    );
    assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
    assert_eq!(memory.used(), before + occupied.bytes());
    drop(occupied);
    let mut pipeline = context
        .queries
        .retained_query(&plan, &params, &scope)
        .unwrap()
        .unwrap();
    assert!(
        memory.used() >= before + (4 << 20),
        "{provider:?} {input}: retained input lost its lease"
    );
    pipeline.open().unwrap();
    let batch = pipeline.next().unwrap().unwrap();
    assert_eq!(batch.rows.len(), 1);
    drop(batch);
    pipeline.close().unwrap();
    drop(pipeline);
    assert_eq!(memory.used(), before);
    for token in [resources.cancellation(), context.runtime.cancellation] {
        token.cancel();
        let error = context
            .queries
            .retained_query(&plan, &params, &scope)
            .err()
            .unwrap();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(memory.used(), before);
        token.reset();
    }
    drop(scope);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}
