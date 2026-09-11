//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use uqa_core::Value;
use uqa_execution::query::relational::build_relational_operator;
use uqa_planner::{AccessPathPlan, ComputePlan, OrderPlan, ProjectionPlan, QueryBlockPlan};
use uqa_sql::ScalarExpr;

#[test]
fn bound_projection_order_and_limit_build_with_explicit_runtime_view() {
    let engine = Engine::new();
    engine.set_variable("work_mem", "64kB").unwrap();
    let ctes = crate::capabilities::query_scope::new_for_current_routine(&engine);
    let schema = uqa_execution::RowSchema::with_types(
        vec!["id".into(), "payload".into()],
        vec![
            Some(uqa_sql::ast::ColumnType::BigInteger),
            Some(uqa_sql::ast::ColumnType::Text),
        ],
    );
    let rows = vec![
        uqa_execution::PhysicalRow::from_values(vec![Value::Int(1), Value::Str("a".into())]),
        uqa_execution::PhysicalRow::from_values(vec![Value::Int(3), Value::Str("c".into())]),
        uqa_execution::PhysicalRow::from_values(vec![Value::Int(2), Value::Str("b".into())]),
    ];
    let operator: Box<dyn uqa_execution::PhysicalOperator> = Box::new(
        uqa_execution::scan::TableScan::from_physical_rows(schema, rows),
    );
    let statement = QueryBlockPlan {
        projections: vec![ProjectionPlan {
            expr: ScalarExpr::Column("id".into()),
            alias: None,
        }],
        from: None,
        r#where: None,
        compute: ComputePlan::Project,
        group_by: Vec::new(),
        grouping_sets: Vec::new(),
        group_distinct: false,
        having: None,
        order_by: vec![OrderPlan {
            expr: ScalarExpr::Column("id".into()),
            descending: true,
            nulls: None,
        }],
        limit: Some(ScalarExpr::Literal(Value::Int(2))),
        with_ties: false,
        offset: None,
        distinct: false,
        distinct_on: Vec::new(),
        subqueries: Vec::new(),
        access: AccessPathPlan::Row,
        locking: Vec::new(),
    };

    let (mut operator, resjunk) = build_relational_operator(
        engine.relational_context(),
        operator,
        None,
        &statement,
        &[],
        &ctes,
        engine.query_runtime_view(),
    )
    .unwrap();
    assert!(resjunk.is_empty());
    let ids = uqa_execution::physical::run_to_batches(operator.as_mut())
        .unwrap()
        .into_iter()
        .flat_map(uqa_execution::Batch::into_result_rows)
        .map(|row| row.get("id").cloned().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec![Value::Int(3), Value::Int(2)]);
}
