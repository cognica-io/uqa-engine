//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Remove unobserved folded CTE outputs before their constant expressions are evaluated.

use crate::{AggregateClassifier, QueryPlan, RelationalPlan, SourcePlan};
use std::collections::BTreeSet;
use uqa_sql::{semantics::volatility::VolatilityCatalog, SQLError, ScalarExpr};

pub(super) fn prune(
    query: &mut QueryPlan,
    aggregates: &dyn AggregateClassifier,
    volatility: &dyn VolatilityCatalog,
    routines: Option<&dyn uqa_sql::routines::RoutineResolution>,
) -> Result<(), SQLError> {
    let folded = uqa_sql::semantics::cte_strategy::schedule_plan_ctes(volatility, query)?
        .into_iter()
        .filter(|cte| cte.deferred)
        .map(|cte| cte.plan.name.clone())
        .collect::<BTreeSet<_>>();
    for name in folded {
        let cte = query
            .ctes
            .iter()
            .find(|cte| cte.name == name)
            .expect("scheduled CTE");
        let Some(body) = cte.body.query() else {
            continue;
        };
        let RelationalPlan::QueryBlock(block) = &body.root else {
            continue;
        };
        let names = block
            .projections
            .iter()
            .enumerate()
            .map(|(position, projection)| {
                cte.columns
                    .get(position)
                    .cloned()
                    .unwrap_or_else(|| uqa_sql::semantics::projection_label_at(projection))
            })
            .collect::<Vec<_>>();
        let mut required = BTreeSet::new();
        let mut all = false;
        collect_root(query, &name, &names, &mut required, &mut all);
        for other in query.ctes.iter().filter(|cte| cte.name != name) {
            if let Some(body) = other.body.query() {
                collect_query(body, &name, &names, &mut required, &mut all);
            } else {
                all = true;
            }
        }
        if !all {
            let index = query
                .ctes
                .iter()
                .position(|cte| cte.name == name)
                .expect("scheduled CTE");
            let (before, remaining) = query.ctes.split_at_mut(index);
            let (cte, after) = remaining.split_first_mut().expect("scheduled CTE");
            let visible = |name: &str| {
                before.iter().any(|cte| cte.name == name)
                    || (cte.recursive
                        && (cte.name == name || after.iter().any(|cte| cte.name == name)))
            };
            super::derived_projection::prune_outputs(
                cte.body.query_mut().expect("query CTE"),
                &cte.columns,
                &|column| required.contains(column),
                None,
                super::OutputPruningContext {
                    aggregates,
                    routines,
                    volatility: Some(volatility),
                    visible_ctes: &visible,
                },
            )?;
        }
    }
    Ok(())
}

fn collect_query(
    query: &QueryPlan,
    name: &str,
    names: &[String],
    required: &mut BTreeSet<String>,
    all: &mut bool,
) {
    if query.ctes.iter().any(|cte| cte.name == name) {
        return;
    }
    collect_root(query, name, names, required, all);
    for cte in &query.ctes {
        if let Some(query) = cte.body.query() {
            collect_query(query, name, names, required, all);
        } else {
            *all = true;
        }
    }
}

fn collect_root(
    query: &QueryPlan,
    name: &str,
    names: &[String],
    required: &mut BTreeSet<String>,
    all: &mut bool,
) {
    match &query.root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &block.from {
                collect_source(source, block, name, names, required, all);
            }
            for query in &block.subqueries {
                collect_query(query, name, names, required, all);
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            collect_query(left, name, names, required, all);
            collect_query(right, name, names, required, all);
            for query in subqueries {
                collect_query(query, name, names, required, all);
            }
        }
        RelationalPlan::Values { subqueries, .. } => {
            for query in subqueries {
                collect_query(query, name, names, required, all);
            }
        }
    }
}

fn collect_source(
    source: &SourcePlan,
    block: &crate::QueryBlockPlan,
    name: &str,
    names: &[String],
    required: &mut BTreeSet<String>,
    all: &mut bool,
) {
    match source {
        SourcePlan::Table {
            name: reference,
            qualifier,
            alias,
            column_aliases,
            ..
        } if uqa_sql::semantics::cte_reference_name(reference).as_deref() == Some(name) => {
            let qualifier = alias.as_deref().unwrap_or(qualifier);
            if !block.subqueries.is_empty() {
                *all = true;
            }
            for expression in block.expressions() {
                super::output_demand::visit(expression, &mut |part| {
                    let column = match part {
                        ScalarExpr::Column(column) => Some(column),
                        ScalarExpr::QualifiedColumn {
                            qualifier: selected,
                            column,
                        } if selected == qualifier => Some(column),
                        ScalarExpr::QualifiedStar(selected) if selected == qualifier => {
                            *all = true;
                            None
                        }
                        ScalarExpr::Star | ScalarExpr::Position(_) => {
                            *all = true;
                            None
                        }
                        _ => None,
                    };
                    if let Some(column) = column {
                        if matches!(part, ScalarExpr::Column(_))
                            && column == qualifier
                            && !names.iter().enumerate().any(|(position, name)| {
                                column_aliases.get(position).unwrap_or(name) == column
                            })
                        {
                            *all = true;
                        }
                        for (position, original) in names.iter().enumerate() {
                            if column_aliases.get(position).unwrap_or(original) == column {
                                required.insert(original.clone());
                            }
                        }
                    }
                });
            }
        }
        SourcePlan::Join {
            left,
            right,
            alias,
            on,
            using,
            natural,
            ..
        } => {
            if alias.is_some() || on.is_some() || using.is_some() || *natural {
                *all = true;
            }
            collect_source(left, block, name, names, required, all);
            collect_source(right, block, name, names, required, all);
        }
        SourcePlan::Subquery { body, .. } => collect_query(body, name, names, required, all),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Catalog;
    impl AggregateClassifier for Catalog {
        fn is_registered_aggregate(&self, _: &str) -> bool {
            false
        }
    }
    impl VolatilityCatalog for Catalog {
        fn host_function_volatility(&self, _: &str) -> Option<uqa_sql::ast::FunctionVolatility> {
            None
        }
        fn routine_volatilities(
            &self,
            _: &str,
            _: Option<&uqa_sql::ast::FunctionBinding>,
        ) -> Option<Vec<uqa_sql::ast::FunctionVolatility>> {
            None
        }
        fn view_query(&self, _: &str) -> Result<Option<QueryPlan>, SQLError> {
            Ok(None)
        }
    }

    #[test]
    fn folded_cte_demand_precedes_constant_evaluation_and_preserves_materialization() {
        for (sql, removed) in [
            ("WITH q AS (SELECT lower('a') l,upper('a') u) SELECT l FROM q", true),
            ("WITH q(a,b) AS (SELECT lower('a'),upper('a')) SELECT x FROM q AS r(x,y)", true),
            ("WITH q AS MATERIALIZED (SELECT lower('a') l,upper('a') u) SELECT l FROM q", false),
            ("WITH q AS (SELECT lower('a') l,upper('a') u) SELECT a.l FROM q a CROSS JOIN q b", false),
            ("WITH q AS NOT MATERIALIZED (SELECT lower('a') l,upper('a') u) SELECT a.l FROM q a CROSS JOIN q b", true),
            ("WITH q AS (SELECT lower('a') l,upper('a') u) SELECT l FROM q WHERE u='A'", false),
            ("WITH q AS (SELECT lower('a') l,upper('a') u,random() n) SELECT l FROM q", false),
        ] {
            let crate::UnifiedPlan::Query(mut query) = crate::UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0)) else { panic!("query"); };
            query.rewrite_scalar_expressions(&mut |expression| {
                *expression = uqa_sql::bind_type_introspection(expression.clone(), &uqa_sql::RowSchema::default(), &[]);
            });
            prune(&mut query, &Catalog, &Catalog, None).unwrap();
            let RelationalPlan::QueryBlock(block) = &query.ctes[0].body.query().unwrap().root else { panic!("block"); };
            assert_eq!(matches!(block.projections[1].expr, ScalarExpr::TypedLiteral { value: uqa_core::Value::Null, .. }), removed, "{sql}");
        }
    }
    #[test]
    fn folded_cte_row_count_keeps_filters_and_column_shadowing() {
        for (sql, removed) in [
            (
                "WITH q AS(SELECT 1 AS id,1/0 AS z) SELECT count(*) FROM q",
                true,
            ),
            (
                "WITH q AS(SELECT 1 AS id,1/0 AS z) SELECT count(*) FILTER(WHERE id>0) FROM q",
                true,
            ),
            (
                "WITH q AS(SELECT 1 AS id,1/0 AS z) SELECT count(q) FROM q",
                false,
            ),
            (
                "WITH q AS(SELECT 1 AS id,1/0 AS z) SELECT count(q.*) FROM q",
                false,
            ),
            (
                "WITH q AS(SELECT 1 AS q,1/0 AS z) SELECT count(q) FROM q",
                true,
            ),
            (
                "WITH r AS(SELECT 1 AS id,1/0 AS z) SELECT count(q) FROM r AS q(q,z)",
                true,
            ),
            (
                "WITH q AS MATERIALIZED(SELECT 1 AS id,1/0 AS z) SELECT count(*) FROM q",
                false,
            ),
        ] {
            let crate::UnifiedPlan::Query(mut query) =
                crate::UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
            else {
                panic!("query");
            };
            prune(&mut query, &Catalog, &Catalog, None).unwrap();
            let RelationalPlan::QueryBlock(inner) = &query.ctes[0].body.query().unwrap().root
            else {
                panic!("CTE block");
            };
            assert_eq!(
                matches!(
                    inner.projections[1].expr,
                    ScalarExpr::TypedLiteral {
                        value: uqa_core::Value::Null,
                        ..
                    }
                ),
                removed,
                "{sql}"
            );
        }
    }
}
