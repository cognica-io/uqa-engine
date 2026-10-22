//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Eliminate unused scalar outputs of a simple derived source before constant evaluation.

use crate::{AggregateClassifier, QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan};
use std::collections::BTreeSet;
use uqa_core::Value;
use uqa_sql::{ast::FunctionVolatility, RowSchema, ScalarExpr};

pub(super) fn prune(
    block: &mut QueryBlockPlan,
    aggregates: &dyn AggregateClassifier,
    routines: Option<&dyn uqa_sql::routines::RoutineResolution>,
    volatility: Option<&dyn uqa_sql::semantics::volatility::VolatilityCatalog>,
) {
    let Some(SourcePlan::Subquery {
        body,
        alias,
        column_aliases,
        ..
    }) = &block.from
    else {
        return;
    };
    if !block.subqueries.is_empty() {
        return;
    }
    let mut required = BTreeSet::new();
    let mut all = false;
    for expression in block.expressions() {
        super::output_demand::visit(expression, &mut |part| match part {
            ScalarExpr::Column(name) => {
                if alias.as_deref() == Some(name.as_str())
                    && !has_named_output(body, column_aliases, name.as_str())
                {
                    all = true;
                } else {
                    required.insert(name.clone());
                }
            }
            ScalarExpr::QualifiedColumn { qualifier, column }
                if alias.as_deref() == Some(qualifier.as_str()) =>
            {
                required.insert(column.clone());
            }
            ScalarExpr::QualifiedStar(qualifier)
                if alias.as_deref() == Some(qualifier.as_str()) =>
            {
                all = true;
            }
            ScalarExpr::Star | ScalarExpr::Position(_) => all = true,
            _ => {}
        });
    }
    if all {
        return;
    }
    let aliases = column_aliases.clone();
    let Some(SourcePlan::Subquery { body, .. }) = &mut block.from else {
        unreachable!()
    };
    prune_outputs(
        body,
        &aliases,
        &|name| required.contains(name),
        None,
        aggregates,
        routines,
        volatility,
    );
}

fn has_named_output(query: &QueryPlan, aliases: &[String], name: &str) -> bool {
    if let Some(schema) = &query.analyzed_output {
        return schema
            .columns()
            .iter()
            .enumerate()
            .any(|(index, column)| aliases.get(index).unwrap_or(column) == name);
    }
    let RelationalPlan::QueryBlock(block) = &query.root else {
        // Other relational roots are not pruned by this direct-source path.
        return true;
    };
    block
        .projections
        .iter()
        .enumerate()
        .any(|(index, projection)| {
            aliases.get(index).map_or_else(
                || uqa_sql::semantics::projection_label_at(projection) == name,
                |alias| alias == name,
            )
        })
}

/// Remove unused scalar output expressions while retaining the relation's complete positional descriptor and analyzed column authority.
pub fn prune_outputs(
    query: &mut QueryPlan,
    aliases: &[String],
    required: &dyn Fn(&str) -> bool,
    schema: Option<&RowSchema>,
    aggregates: &dyn AggregateClassifier,
    routines: Option<&dyn uqa_sql::routines::RoutineResolution>,
    volatility: Option<&dyn uqa_sql::semantics::volatility::VolatilityCatalog>,
) {
    if query.analyzed_output.is_none() {
        if let Some(schema) = schema {
            query.analyzed_output = Some(schema.clone());
            if let RelationalPlan::QueryBlock(block) = &mut query.root {
                block.compute = block.compute_kind(aggregates);
            }
        }
    }
    if matches!(query.root, RelationalPlan::SetOp { .. }) {
        super::union_projection::prune(query, aliases, required, aggregates, routines, volatility);
        return;
    }
    let schema = schema.or(query.analyzed_output.as_ref());
    let RelationalPlan::QueryBlock(inner) = &mut query.root else {
        return;
    };
    if inner.distinct
        || !inner.distinct_on.is_empty()
        || !inner.windows.is_empty()
        || inner
            .projections
            .iter()
            .any(|p| matches!(p.expr, ScalarExpr::Star | ScalarExpr::QualifiedStar(_)))
    {
        return;
    }
    let names = inner
        .projections
        .iter()
        .enumerate()
        .map(|(i, p)| {
            aliases
                .get(i)
                .cloned()
                .unwrap_or_else(|| uqa_sql::semantics::projection_label_at(p))
        })
        .collect::<Vec<_>>();
    let mut replacements = Vec::new();
    for (index, projection) in inner.projections.iter().enumerate() {
        if required(&names[index])
            || needed_by_ordering(inner, index)
            || !removable(&projection.expr, aggregates)
        {
            continue;
        }
        // Retain the schema even though the enclosing query never reads this slot.
        let ty = schema
            .and_then(|schema| schema.column_types().get(index))
            .cloned()
            .flatten()
            .or_else(|| {
                uqa_sql::scalar_type(&projection.expr, &RowSchema::default(), &[])
                    .ok()
                    .flatten()
            });
        if let Some(ty) = ty {
            replacements.push((
                index,
                ScalarExpr::TypedLiteral {
                    collation_oid: schema.and_then(|schema| schema.column_collation(index)),
                    composite_source: None,
                    value: Value::Null,
                    ty: ty.catalog_name(),
                    bound_type: Some(ty),
                    parameter_index: None,
                },
            ));
        }
    }
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

fn needed_by_ordering(block: &QueryBlockPlan, index: usize) -> bool {
    let projection = &block.projections[index];
    let name = uqa_sql::semantics::projection_label_at(projection);
    block.order_by.iter().any(|order| {
        matches!(&order.expr, ScalarExpr::Column(column) if *column == name)
            || matches!(order.expr, ScalarExpr::Position(position) if position == index)
            || matches!(order.expr, ScalarExpr::Literal(Value::Int(position)) if uqa_sql::semantics::query_output_position(position, block.projections.len(), "ORDER BY").is_ok_and(|position| position == index))
            || uqa_sql::semantics::aggregates::exprs_match(&order.expr, &projection.expr)
    })
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
            if uqa_sql::semantics::aggregates::is_builtin_aggregate(part) {
                // The aggregate's arguments are still visited for volatile effects; its row cardinality belongs to the block's compute phase.
                return;
            }
            let name = uqa_sql::semantics::builtin_function_dispatch_name(name);
            if aggregates.is_registered_aggregate(&name)
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
                .is_ok_and(|ty| uqa_sql::type_resolution::immutable_cast_type(&ty)) =>
        {
            removable = false;
        }
        ScalarExpr::WindowCall { .. }
        | ScalarExpr::CompositeRow { .. }
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
    fn named_output_pruning_preserves_bound_types_aliases_and_column_authority() {
        let crate::UnifiedPlan::Query(mut query) = crate::UnifiedPlan::lower(
            uqa_sql::compile("SELECT a AS first, b AS second FROM data")
                .unwrap()
                .remove(0),
        ) else {
            panic!("query");
        };
        let schema = RowSchema::with_types(
            vec!["first".into(), "second".into()],
            vec![
                Some(uqa_sql::ColumnType::Text),
                Some(uqa_sql::ColumnType::Integer),
            ],
        );
        prune_outputs(
            &mut query,
            &["selected".into(), "unused".into()],
            &|name| name == "selected",
            Some(&schema),
            &NoAggregates,
            None,
            None,
        );
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("block");
        };
        assert_eq!(block.projections.len(), 2);
        assert!(matches!(&block.projections[0].expr, ScalarExpr::Column(name) if name == "a"));
        assert_eq!(
            uqa_sql::scalar_type(&block.projections[1].expr, &RowSchema::default(), &[]).unwrap(),
            Some(uqa_sql::ColumnType::Integer)
        );
        assert!(matches!(
            block.projections[1].expr,
            ScalarExpr::TypedLiteral {
                value: Value::Null,
                ..
            }
        ));
        assert_eq!(block.projections[1].alias.as_deref(), Some("second"));
        assert_eq!(
            block.privilege_columns,
            BTreeSet::from([uqa_sql::ColumnIdentity::unqualified("b")])
        );
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
            (
                "SELECT 1 FROM (SELECT lower(v) AS x FROM data ORDER BY 1) q",
                false,
            ),
            (
                "SELECT 1 FROM (SELECT lower(v) AS x FROM data ORDER BY 0+1) q",
                true,
            ),
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
            prune(block, &NoAggregates, None, None);
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
    #[test]
    fn count_rows_and_whole_row_references_have_different_output_demand() {
        for (sql, removed) in [
            ("SELECT count(*) FROM (SELECT 1 AS id,1/0 AS z) q", true),
            (
                "SELECT 2+pg_catalog.count(*) FROM (SELECT 1 AS id,1/0 AS z) q",
                true,
            ),
            (
                "SELECT count(*) FILTER(WHERE id>0) FROM (SELECT 1 AS id,1/0 AS z) q",
                true,
            ),
            ("SELECT count(q) FROM (SELECT 1 AS id,1/0 AS z) q", false),
            ("SELECT count(q.*) FROM (SELECT 1 AS id,1/0 AS z) q", false),
            ("SELECT count(q) FROM (SELECT 1 AS q,1/0 AS z) q", true),
            (
                "SELECT count(q) FROM (SELECT 1 AS id,1/0 AS z) q(q,z)",
                true,
            ),
        ] {
            let crate::UnifiedPlan::Query(mut query) =
                crate::UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
            else {
                panic!("query");
            };
            let RelationalPlan::QueryBlock(outer) = &mut query.root else {
                panic!("outer block");
            };
            prune(outer, &NoAggregates, None, None);
            let Some(SourcePlan::Subquery { body, .. }) = &outer.from else {
                panic!("derived source");
            };
            let RelationalPlan::QueryBlock(inner) = &body.root else {
                panic!("inner block");
            };
            assert_eq!(
                matches!(
                    inner.projections[1].expr,
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
