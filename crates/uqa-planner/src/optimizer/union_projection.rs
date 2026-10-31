//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Positional demand through UNION ALL, retaining PostgreSQL append-member simplification boundaries.

use super::OutputPruningContext;
use crate::{ComputePlan, QueryPlan, RelationalPlan, SourcePlan};
use uqa_sql::{
    ast::{FunctionVolatility, SetOpKind},
    routines::RoutineResolution,
    semantics::volatility::VolatilityCatalog,
    ColumnType, RowSchema, SQLError, ScalarExpr,
};

pub(super) fn prune(
    query: &mut QueryPlan,
    required: &dyn Fn(usize) -> bool,
    context: OutputPruningContext<'_>,
) -> Result<(), SQLError> {
    let Some(output) = query.analyzed_output.clone() else {
        return Ok(());
    };
    if !simple_union(query, output.column_types()) {
        return Ok(());
    }
    prune_members(query, required, context)
}

fn simple_union(query: &QueryPlan, types: &[Option<ColumnType>]) -> bool {
    match &query.root {
        RelationalPlan::SetOp {
            kind: SetOpKind::Union,
            all: true,
            left,
            right,
            order_by,
            limit: None,
            offset: None,
            ..
        } if query.ctes.is_empty() && order_by.is_empty() => {
            simple_union(left, types) && simple_union(right, types)
        }
        RelationalPlan::QueryBlock(_) | RelationalPlan::Values { .. } => query
            .analyzed_output
            .as_ref()
            .is_some_and(|schema| same_types(schema, types)),
        RelationalPlan::SetOp { .. } => false,
    }
}

fn same_types(schema: &RowSchema, types: &[Option<ColumnType>]) -> bool {
    schema.column_types().len() == types.len()
        && schema
            .column_types()
            .iter()
            .zip(types)
            .all(|(source, target)| match (source, target) {
                (Some(source), Some(target)) => {
                    uqa_sql::catalog::type_metadata::pg_type_oid(source)
                        == uqa_sql::catalog::type_metadata::pg_type_oid(target)
                }
                _ => false,
            })
}

fn prune_members(
    query: &mut QueryPlan,
    required: &dyn Fn(usize) -> bool,
    context: OutputPruningContext<'_>,
) -> Result<(), SQLError> {
    let pulled_up = observation::pulled_up_member(
        query,
        context.routines,
        context.volatility,
        &context.production,
    )?;
    match &mut query.root {
        RelationalPlan::SetOp { left, right, .. } => {
            prune_members(left, required, context)?;
            prune_members(right, required, context)?;
        }
        RelationalPlan::QueryBlock(_) if pulled_up == Some(false) => {
            super::derived_projection::prune_positions(query, required, None, context)?;
        }
        _ => {}
    }
    Ok(())
}

mod observation;
pub(super) mod owned;

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::Value;
    use uqa_sql::ScalarExpr;

    struct NoAggregates;
    impl crate::AggregateClassifier for NoAggregates {
        fn is_registered_aggregate(&self, _: &str) -> bool {
            false
        }
    }

    #[test]
    fn discarded_aggregate_values_keep_the_implicit_group() {
        let mut plan = crate::UnifiedPlan::lower(
            uqa_sql::compile("SELECT count(*) FROM (SELECT sum(1/0) AS z) q")
                .unwrap()
                .remove(0),
        );
        plan.visit_queries_mut(&mut |query| {
            query.analyzed_output = Some(RowSchema::with_types(
                vec!["z".into()],
                vec![Some(ColumnType::BigInteger)],
            ));
        });
        let crate::UnifiedPlan::Query(query) = &mut plan else {
            panic!("query");
        };
        query.analyzed_output = Some(RowSchema::with_types(
            vec!["count".into()],
            vec![Some(ColumnType::BigInteger)],
        ));
        query.analyzed_source = Some(
            std::sync::Arc::new(uqa_sql::plan::AnalyzedSource {
                schema: RowSchema::with_relation_qualifier(
                    &RowSchema::with_types(vec!["z".into()], vec![Some(ColumnType::BigInteger)]),
                    "q",
                ),
                children: None,
            })
            .into(),
        );
        let crate::UnifiedPlan::Query(query) = super::super::optimize(
            plan,
            &super::super::OptimizerConfig::new(
                uqa_execution::scalar::eval_constant_scalar_with_control,
            ),
        )
        .map(|plan| {
            let (plan, memory) = plan.into_parts();
            assert_eq!(memory.reserved_bytes(), 0);
            plan
        })
        .unwrap() else {
            panic!("query");
        };
        let crate::UnifiedPlan::Query(query) = super::super::optimize(
            crate::UnifiedPlan::Query(query),
            &super::super::OptimizerConfig::new(
                uqa_execution::scalar::eval_constant_scalar_with_control,
            ),
        )
        .map(|plan| {
            let (plan, memory) = plan.into_parts();
            assert_eq!(memory.reserved_bytes(), 0);
            plan
        })
        .unwrap() else {
            panic!("reoptimized query");
        };
        let RelationalPlan::QueryBlock(outer) = &query.root else {
            panic!("outer");
        };
        let Some(SourcePlan::Subquery { body, .. }) = &outer.from else {
            panic!("source");
        };
        let RelationalPlan::QueryBlock(inner) = &body.root else {
            panic!("inner");
        };
        assert!(matches!(inner.compute, ComputePlan::Aggregate));
        assert!(matches!(
            inner.projections[0].expr,
            ScalarExpr::TypedLiteral {
                value: Value::Null,
                ..
            }
        ));
    }

    #[test]
    fn unused_append_outputs_preserve_postgresql_member_boundaries() {
        // PostgreSQL 18.4 observations in union_output_demand_oracle: each branch exposes one integer slot.
        for (sql, removed) in [
            ("SELECT 1/0 AS z UNION ALL SELECT 2", false),
            ("SELECT 1/0 AS z WHERE true UNION ALL SELECT 2", true),
            ("SELECT 1/0 AS z WHERE false UNION ALL SELECT 2", true),
            ("(SELECT 1/0 AS z OFFSET 0) UNION ALL SELECT 2", true),
            ("(SELECT 1/0 AS z LIMIT 0) UNION ALL SELECT 2", true),
            (
                "SELECT DISTINCT 1/0 AS z WHERE false UNION ALL SELECT 2",
                false,
            ),
            ("SELECT 1/0 AS z WHERE false UNION SELECT 2", false),
            (
                "SELECT 1/0 AS z WHERE false UNION ALL SELECT 2 LIMIT 1",
                false,
            ),
        ] {
            let crate::UnifiedPlan::Query(mut query) =
                crate::UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
            else {
                panic!("query");
            };
            let schema = RowSchema::with_types(vec!["z".into()], vec![Some(ColumnType::Integer)]);
            query.visit_queries_mut(&mut |query| query.analyzed_output = Some(schema.clone()));
            prune(
                &mut query,
                &|_| false,
                OutputPruningContext::new(&NoAggregates),
            )
            .unwrap();
            let RelationalPlan::SetOp { left, .. } = &query.root else {
                panic!("union");
            };
            let RelationalPlan::QueryBlock(block) = &left.root else {
                panic!("member");
            };
            assert_eq!(
                matches!(
                    block.projections[0].expr,
                    ScalarExpr::TypedLiteral {
                        value: Value::Null,
                        ..
                    }
                ),
                removed,
                "{sql}"
            );
        }
    }
}
