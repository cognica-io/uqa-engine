//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::query::relational::values::test_support::Services;
use uqa_sql::plan::{RelationalPlan, UnifiedPlan};

fn block(sql: &str) -> QueryBlockPlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("query");
    };
    let RelationalPlan::QueryBlock(block) = query.root else {
        panic!("query block");
    };
    *block
}

#[test]
fn constant_group_outputs_read_finalized_slots_without_an_input_row() {
    for (sql, count) in [
        ("SELECT true, false, NOT true", false),
        ("SELECT count(*), true, false, NOT true", true),
    ] {
        let mut statement = block(sql);
        statement.group_by = vec![ScalarExpr::Literal(Value::Bool(true))];
        let mut targets = Vec::new();
        let mut accumulators = Vec::new();
        let mut expected = Vec::new();
        if count {
            targets.push(statement.projections[0].expr.clone());
            let mut accumulator = AggregateAccumulator::builtin("count");
            accumulator.observe(&Value::Int(1)).unwrap();
            accumulators.push(accumulator);
            expected.push(Value::Int(1));
        }
        expected.extend([Value::Bool(true), Value::Bool(false), Value::Bool(false)]);
        let output = AggregateOutputPlan::compile(
            &Services,
            &statement,
            &targets,
            false,
            &RowSchema::default(),
            &[],
        )
        .unwrap();
        let labels = vec!["output".into(); expected.len()];
        assert_eq!(
            finish_group(
                &Services,
                &statement,
                &output,
                accumulators,
                &[Value::Bool(true)],
                &labels,
                &[],
            )
            .unwrap(),
            Some(expected),
            "{sql}"
        );
    }
}

#[test]
fn grouped_expression_outputs_keep_the_finalized_null_value() {
    let mut statement = block("SELECT true, NOT true, true IS NULL");
    statement.group_by = vec![ScalarExpr::Literal(Value::Bool(true))];
    let output = AggregateOutputPlan::compile(
        &Services,
        &statement,
        &[],
        false,
        &RowSchema::default(),
        &[],
    )
    .unwrap();
    assert_eq!(
        finish_group(
            &Services,
            &statement,
            &output,
            Vec::new(),
            &[Value::Null],
            &["value".into(), "negated".into(), "is_null".into()],
            &[],
        )
        .unwrap(),
        Some(vec![Value::Null, Value::Null, Value::Bool(true)])
    );
}
