//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_planner::statement_planning::schema_expressions::optimize_rule_condition;
use uqa_sql::{plan::ExpressionPlan, ScalarExpr};

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_subquery_compaction_shares_request_allowance_and_recovers(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("subquery-compaction.db"));
    engine.sql("SELECT 1", &[]).unwrap();
    let mut expression = ExpressionPlan {
        scalar: ScalarExpr::ScalarSubquery(1),
        subqueries: ["SELECT false", "SELECT true"]
            .into_iter()
            .map(|sql| {
                let UnifiedPlan::Query(query) =
                    UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
                else {
                    panic!("query")
                };
                *query
            })
            .collect(),
    };
    let before = serde_json::to_value(&expression).unwrap();
    let buffer = expression.subqueries.as_ptr();
    let expected = serde_json::to_value(&expression.subqueries[1]).unwrap();
    let retention = engine.query_retention_control().unwrap();
    let memory = retention.memory();
    let baseline = memory.used();
    let sibling = memory.reserve(memory.available()).unwrap();
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        result = Some(optimize_rule_condition(
            &engine.statement_planning_context(),
            &mut expression,
        ));
    });
    assert_eq!(
        result.unwrap().unwrap_err().sqlstate(),
        Some("53200"),
        "{provider:?}"
    );
    assert!(
        allocation.bytes_total < 8192,
        "{provider:?}: {allocation:?}"
    );
    assert_eq!(serde_json::to_value(&expression).unwrap(), before);
    assert_eq!(memory.used(), baseline + sibling.bytes());
    drop(sibling);

    // The command discards its private optimization copy on any scalar-planning error.
    let mut cancelled_expression = expression.clone();
    engine.cancel();
    let error = optimize_rule_condition(
        &engine.statement_planning_context(),
        &mut cancelled_expression,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
    drop(cancelled_expression);
    assert_eq!(memory.used(), baseline);
    engine.reset_cancellation();
    optimize_rule_condition(&engine.statement_planning_context(), &mut expression).unwrap();
    assert_eq!(expression.subqueries.as_ptr(), buffer);
    assert_eq!(expression.scalar, ScalarExpr::ScalarSubquery(0));
    assert_eq!(expression.subqueries.len(), 1);
    assert_eq!(
        serde_json::to_value(&expression.subqueries[0]).unwrap(),
        expected
    );
    assert_eq!(memory.used(), baseline);
    drop(expression);
    assert_eq!(
        engine
            .sql("SELECT CASE WHEN false THEN (SELECT 1/0) ELSE 7 END", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(7))
    );
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}
