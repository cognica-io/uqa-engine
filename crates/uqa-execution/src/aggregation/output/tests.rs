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
            None,
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
        None,
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

#[test]
fn enclosing_group_outputs_survive_empty_input_and_appended_local_slots() {
    let local = RowSchema::with_qualified_types(
        "t",
        vec!["n".into()],
        vec![Some(uqa_sql::ast::ColumnType::Integer)],
    );
    for value in [3, 4] {
        let outer = OwnedPhysicalRow::new(
            RowSchema::with_qualified_types(
                "o",
                vec!["n".into()],
                vec![Some(uqa_sql::ast::ColumnType::Integer)],
            ),
            PhysicalRow::from_values(vec![Value::Int(value)]),
        );
        let input = RowSchema::with_outer_schema(&local, &outer.schema);
        let scope = outer::AggregateOuterRow::new(&outer, input.physical_width()).unwrap();
        let input = RowSchema::append_typed(
            &input,
            &[("computed".into(), Some(uqa_sql::ast::ColumnType::Integer))],
        );
        for (sql, grouped_outer) in [
            (
                "SELECT o.n, count(*) FROM t GROUP BY GROUPING SETS ((t.n), ())",
                false,
            ),
            (
                "SELECT o.n, count(*) FROM t GROUP BY GROUPING SETS ((o.n), ())",
                true,
            ),
        ] {
            let statement = block(sql);
            for (selected, empty_input) in [
                (&statement.grouping_sets[0], false),
                (&statement.grouping_sets[1], true),
            ] {
                let active = uqa_sql::semantics::aggregates::select_grouping_set(
                    &Services, &statement, selected,
                )
                .unwrap();
                let target = active.projections[1].expr.clone();
                let output = AggregateOutputPlan::compile(
                    &Services,
                    &active,
                    &[target],
                    true,
                    &input,
                    &[],
                    Some(&scope),
                )
                .unwrap();
                let mut count = AggregateAccumulator::builtin("count");
                if !empty_input {
                    count.observe(&Value::Int(1)).unwrap();
                }
                let keys = if empty_input {
                    Vec::new()
                } else {
                    vec![Value::Int(if grouped_outer { value } else { 1 })]
                };
                let expected = if grouped_outer && empty_input {
                    Value::Null
                } else {
                    Value::Int(value)
                };
                assert_eq!(
                    finish_group(
                        &Services,
                        &active,
                        &output,
                        vec![count],
                        &keys,
                        &["n".into(), "count".into()],
                        &[]
                    )
                    .unwrap(),
                    Some(vec![expected, Value::Int(i64::from(!empty_input))]),
                    "{sql}"
                );
            }
        }
        let statement = block("SELECT computed FROM t");
        let output = AggregateOutputPlan::compile(
            &Services,
            &statement,
            &[],
            true,
            &input,
            &[],
            Some(&scope),
        )
        .unwrap();
        assert!(
            matches!(output.projections[0], AggregateProjectionPlan::Null),
            "an appended local value must not become an enclosing input"
        );
    }
}
