//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::ast::{InternalRelationId, RetainedRowSource};
use uqa_sql::{ColumnType, RowSchema};

#[test]
fn retained_row_plans_preserve_cardinality_identity_and_internal_predicates() {
    let statistics = |_: &str| None;
    for count in [0, 1, 2048] {
        let relation = InternalRelationId::allocate();
        let source = RetainedRowSource::new(
            relation,
            RowSchema::with_internal_relation_types(relation, vec![Some(ColumnType::Integer)]),
            count,
        )
        .unwrap();
        let UnifiedPlan::Query(mut query) =
            UnifiedPlan::lower(compile("SELECT value FROM input").unwrap().remove(0))
        else {
            panic!("query")
        };
        let RelationalPlan::QueryBlock(block) = &mut query.root else {
            panic!("block")
        };
        block.from = Some(SourcePlan::RetainedRows(source.clone()));
        query.rewrite_scalar_expressions(&mut |expression| {
            if matches!(expression, ScalarExpr::Column(name) if name == "value") {
                *expression = ScalarExpr::InternalColumn(relation.column(0));
            }
        });
        let plan = optimize(
            UnifiedPlan::Query(query),
            &OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control),
        )
        .unwrap();
        assert_eq!(
            crate::plan_cost::PlanCostEstimator::new(&statistics)
                .estimate(&plan)
                .rows,
            count as f64
        );
        let Some(SourcePlan::RetainedRows(retained)) = &query_block(&plan).from else {
            panic!("source was removed")
        };
        assert_eq!(retained, &source);

        let UnifiedPlan::Query(mut query) = plan else {
            panic!("query")
        };
        let RelationalPlan::QueryBlock(block) = &mut query.root else {
            panic!("block")
        };
        block.r#where = Some(ScalarExpr::Binary {
            lhs: Box::new(ScalarExpr::InternalColumn(relation.column(0))),
            op: uqa_sql::ast::BinaryOp::Greater,
            rhs: Box::new(ScalarExpr::Literal(Value::Int(1))),
        });
        let plan = optimize(
            UnifiedPlan::Query(query),
            &OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control),
        )
        .unwrap();
        assert!(
            query_block(&plan).r#where.is_some(),
            "internal predicate cannot be elided into a SQL qualifier"
        );
        let Some(SourcePlan::RetainedRows(retained)) = &query_block(&plan).from else {
            panic!("source was removed")
        };
        assert_eq!(retained, &source);
    }
}
