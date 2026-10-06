//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Eliminate unused scalar outputs of a simple derived source before constant evaluation.

use crate::{AggregateClassifier, ComputePlan, QueryBlockPlan, RelationalPlan, SourcePlan};
use std::collections::BTreeSet;
use uqa_core::Value;
use uqa_sql::{ast::FunctionVolatility, RowSchema, ScalarExpr};

pub(super) fn prune(block: &mut QueryBlockPlan, aggregates: &dyn AggregateClassifier) {
    let Some(SourcePlan::Subquery {
        body,
        column_aliases,
        ..
    }) = &block.from
    else {
        return;
    };
    let RelationalPlan::QueryBlock(inner) = &body.root else {
        return;
    };
    if inner.distinct
        || !inner.distinct_on.is_empty()
        || !matches!(inner.compute, ComputePlan::Project)
        || !inner.group_by.is_empty()
        || !inner.grouping_sets.is_empty()
        || inner.having.is_some()
        || !inner.order_by.is_empty()
        || !inner.windows.is_empty()
        || !block.subqueries.is_empty()
        || inner
            .projections
            .iter()
            .any(|p| matches!(p.expr, ScalarExpr::Star | ScalarExpr::QualifiedStar(_)))
    {
        return;
    }
    let mut required = BTreeSet::new();
    let mut all = false;
    for expression in block.expressions() {
        expression.visit(&mut |part| match part {
            ScalarExpr::Column(name) | ScalarExpr::QualifiedColumn { column: name, .. } => {
                required.insert(name.clone());
            }
            ScalarExpr::Star | ScalarExpr::QualifiedStar(_) | ScalarExpr::Position(_) => all = true,
            _ => {}
        });
    }
    if all {
        return;
    }
    let names = inner
        .projections
        .iter()
        .enumerate()
        .map(|(i, p)| {
            column_aliases
                .get(i)
                .cloned()
                .unwrap_or_else(|| uqa_sql::semantics::projection_label_at(p))
        })
        .collect::<Vec<_>>();
    let mut replacements = Vec::new();
    for (index, projection) in inner.projections.iter().enumerate() {
        if required.contains(&names[index]) || !removable(&projection.expr, aggregates) {
            continue;
        }
        // Retain the schema even though the enclosing query never reads this slot.
        if let Ok(Some(ty)) = uqa_sql::scalar_type(&projection.expr, &RowSchema::default(), &[]) {
            replacements.push((
                index,
                ScalarExpr::TypedLiteral {
                    value: Value::Null,
                    ty: ty.sql_name(),
                    bound_type: Some(ty),
                    parameter_index: None,
                },
            ));
        }
    }
    let Some(SourcePlan::Subquery { body, .. }) = &mut block.from else {
        unreachable!()
    };
    let RelationalPlan::QueryBlock(inner) = &mut body.root else {
        unreachable!()
    };
    for (index, expression) in replacements {
        let projection = &mut inner.projections[index];
        projection.expr.visit(&mut |part| match part {
            ScalarExpr::Column(column) => {
                inner
                    .privilege_columns
                    .insert(uqa_sql::ColumnIdentity::unqualified(column.clone()));
            }
            ScalarExpr::QualifiedColumn { qualifier, column } => {
                inner
                    .privilege_columns
                    .insert(uqa_sql::ColumnIdentity::qualified(
                        qualifier.clone(),
                        column.clone(),
                    ));
            }
            _ => {}
        });
        if projection.alias.is_none() {
            projection.alias = Some(uqa_sql::semantics::projection_label_at(projection));
        }
        projection.expr = expression;
    }
}

fn removable(expression: &ScalarExpr, aggregates: &dyn AggregateClassifier) -> bool {
    let mut removable = true;
    expression.visit(&mut |part| match part {
        ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } => {
            let name = uqa_sql::semantics::builtin_function_dispatch_name(name);
            if crate::unified_plan::is_builtin_aggregate(&name)
                || aggregates.is_registered_aggregate(&name)
                || uqa_sql::semantics::sets::validation::builtin_returns_set(&name)
                || binding.as_ref().is_none_or(|binding| !binding.builtin)
                || uqa_sql::semantics::volatility::builtin_function_volatility(
                    &name,
                    binding.as_ref(),
                    args.len(),
                ) == FunctionVolatility::Volatile
            {
                removable = false;
            }
        }
        ScalarExpr::Cast { ty, .. }
            if !uqa_sql::ColumnType::from_sql_name(ty)
                .is_ok_and(|ty| super::scalar::constants::immutable_cast_type(&ty)) =>
        {
            removable = false;
        }
        ScalarExpr::WindowCall { .. }
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => removable = false,
        _ => {}
    });
    removable
}

#[cfg(test)]
mod tests {
    use super::*;
    struct NoAggregates;
    impl AggregateClassifier for NoAggregates {
        fn is_registered_aggregate(&self, _: &str) -> bool {
            false
        }
    }

    #[test]
    fn unused_derived_values_are_removed_without_losing_column_authority_or_schema() {
        for (sql, pruned) in [
            ("SELECT 1 FROM (SELECT lower(v) AS x FROM data) q", true),
            ("SELECT x FROM (SELECT lower(v) AS x FROM data) q", false),
            (
                "SELECT 1 FROM (SELECT DISTINCT lower(v) AS x FROM data) q",
                false,
            ),
            (
                "SELECT 1 FROM (SELECT lower(v) AS x FROM data ORDER BY x) q",
                false,
            ),
            ("SELECT 1 FROM (SELECT sum(v) AS x FROM data) q", false),
            (
                "SELECT 1 FROM (SELECT generate_series(1,2) AS x FROM data) q",
                false,
            ),
            ("SELECT 1 FROM (SELECT random() AS x FROM data) q", false),
        ] {
            let crate::UnifiedPlan::Query(mut query) =
                crate::UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
            else {
                panic!("query")
            };
            let RelationalPlan::QueryBlock(block) = &mut query.root else {
                panic!("block")
            };
            let Some(SourcePlan::Subquery { body, .. }) = &mut block.from else {
                panic!("derived")
            };
            let RelationalPlan::QueryBlock(inner) = &mut body.root else {
                panic!("inner")
            };
            let schema =
                RowSchema::with_types(vec!["v".into()], vec![Some(uqa_sql::ColumnType::Text)]);
            inner.projections[0].expr =
                uqa_sql::bind_type_introspection(inner.projections[0].expr.clone(), &schema, &[]);
            prune(block, &NoAggregates);
            let Some(SourcePlan::Subquery { body, .. }) = &block.from else {
                unreachable!()
            };
            let RelationalPlan::QueryBlock(inner) = &body.root else {
                unreachable!()
            };
            assert_eq!(
                matches!(
                    &inner.projections[0].expr,
                    ScalarExpr::TypedLiteral {
                        value: Value::Null,
                        ..
                    }
                ),
                pruned,
                "{sql}"
            );
            if pruned {
                assert_eq!(
                    inner.privilege_columns,
                    BTreeSet::from([uqa_sql::ColumnIdentity::unqualified("v")])
                );
                assert_eq!(inner.projections[0].alias.as_deref(), Some("x"));
                assert_eq!(
                    uqa_sql::scalar_type(&inner.projections[0].expr, &schema, &[]).unwrap(),
                    Some(uqa_sql::ColumnType::Text)
                );
            }
        }
    }
}
