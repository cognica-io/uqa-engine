//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_core::Value;
use uqa_sql::{ast::InternalRelationId, ColumnType, ScalarExpr};
mod control;

fn query(sql: &str) -> Box<QueryPlan> {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    query
}

#[test]
fn hidden_captured_outputs_follow_physical_demand() {
    let relation = InternalRelationId::allocate();
    let schema =
        RowSchema::with_internal_relation_types(relation, vec![Some(ColumnType::Integer); 2]);
    assert_eq!(schema.len(), 0);
    assert_eq!(schema.physical_width(), 2);
    let mut plan = query("SELECT 7, 1/0");
    let config = OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control);
    let context = Context {
        config: &config,
        aggregates: &|_: &str| false,
        ctes: None,
    };
    let mut demand = Demand::new(config.production);
    demand.extend([0]).unwrap();
    context
        .outputs(
            &mut plan,
            &AnalyzedSource {
                schema,
                children: None,
            },
            &demand,
            Some(relation),
        )
        .unwrap();
    let RelationalPlan::QueryBlock(block) = plan.root else {
        panic!("block")
    };
    assert_eq!(
        block.projections[0].expr,
        ScalarExpr::Literal(Value::Int(7))
    );
    assert!(matches!(
        block.projections[1].expr,
        ScalarExpr::TypedLiteral {
            value: Value::Null,
            ..
        }
    ));
}

#[test]
fn join_lateral_scope_preserves_captured_input_used_only_by_its_right_child() {
    let relation = InternalRelationId::allocate();
    let left_schema =
        RowSchema::with_internal_relation_types(relation, vec![Some(ColumnType::Integer); 2]);
    let right_schema =
        RowSchema::with_qualified_types("r", vec!["n".into()], vec![Some(ColumnType::Integer)]);
    let mut plan = query("SELECT r.n FROM (SELECT 7, 1/0) l CROSS JOIN (SELECT 1 AS n) r");
    let RelationalPlan::QueryBlock(block) = &mut plan.root else {
        panic!("block")
    };
    let Some(SourcePlan::Join {
        left,
        right,
        lateral,
        ..
    }) = &mut block.from
    else {
        panic!("join")
    };
    *lateral = true;
    let SourcePlan::Subquery {
        captured_output, ..
    } = left.as_mut()
    else {
        panic!("left")
    };
    *captured_output = Some(relation);
    let SourcePlan::Subquery { body, lateral, .. } = right.as_mut() else {
        panic!("right")
    };
    // The join supplies LATERAL scope even when the child itself has no LATERAL marker.
    *lateral = false;
    let RelationalPlan::QueryBlock(right) = &mut body.root else {
        panic!("right block")
    };
    right.projections[0].expr = ScalarExpr::InternalColumn(relation.column(0));
    plan.analyzed_source = Some(Arc::new(AnalyzedSource {
        schema: RowSchema::join(&left_schema, &right_schema, std::iter::empty()),
        children: Some(Box::new((
            AnalyzedSource {
                schema: left_schema,
                children: None,
            },
            AnalyzedSource {
                schema: right_schema,
                children: None,
            },
        ))),
    }));
    let mut plan = UnifiedPlan::Query(plan);
    prune(
        &mut plan,
        &OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control),
        &|_: &str| false,
    )
    .unwrap();
    let UnifiedPlan::Query(plan) = plan else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = plan.root else {
        panic!("block")
    };
    let Some(SourcePlan::Join { left, .. }) = block.from else {
        panic!("join")
    };
    let SourcePlan::Subquery { body, .. } = *left else {
        panic!("left")
    };
    let RelationalPlan::QueryBlock(block) = body.root else {
        panic!("block")
    };
    assert_eq!(
        block.projections[0].expr,
        ScalarExpr::Literal(Value::Int(7))
    );
    assert!(matches!(
        block.projections[1].expr,
        ScalarExpr::TypedLiteral {
            value: Value::Null,
            ..
        }
    ));
}
