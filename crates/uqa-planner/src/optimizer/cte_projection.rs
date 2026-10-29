//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Remove unobserved folded CTE outputs before their constant expressions are evaluated.

use crate::{AggregateClassifier, QueryPlan, RelationalPlan};
use uqa_core::memory::{ProductionControl, ProductionMap, ProductionVec};
use uqa_sql::{semantics::volatility::VolatilityCatalog, SQLError};

mod demand;

pub(super) fn prune(
    query: &mut QueryPlan,
    aggregates: &dyn AggregateClassifier,
    volatility: &dyn VolatilityCatalog,
    routines: Option<&dyn uqa_sql::routines::RoutineResolution>,
    production: ProductionControl<'_>,
) -> Result<(), SQLError> {
    let folded = folded_ctes(query, volatility, production)?;
    for index in folded.iter().copied() {
        production.check()?;
        let cte = &query.ctes[index];
        let Some(body) = cte.body.query() else {
            continue;
        };
        let RelationalPlan::QueryBlock(block) = &body.root else {
            continue;
        };
        let names = uqa_sql::semantics::projection_labels_with_control(
            &block.projections,
            &cte.columns,
            &production,
        )?;
        let mut demand = demand::Demand::new(&cte.name, &names, production);
        demand.collect_root(query)?;
        for other in &query.ctes {
            production.check()?;
            if other.name == cte.name {
                continue;
            }
            if let Some(body) = other.body.query() {
                demand.collect_query(body)?;
            } else {
                demand.all = true;
            }
        }
        if demand.all {
            continue;
        }
        let demanded = super::derived_projection::output_demand(
            body,
            &cte.columns,
            &|column| demand.required(column),
            None,
            production,
        )?
        .expect("query CTE output descriptor");
        drop(demand);
        drop(names);
        let (before, remaining) = query.ctes.split_at_mut(index);
        let (cte, after) = remaining.split_first_mut().expect("scheduled CTE");
        let visible = |name: &str| {
            before.iter().any(|cte| cte.name == name)
                || (cte.recursive && (cte.name == name || after.iter().any(|cte| cte.name == name)))
        };
        super::derived_projection::prune_positions(
            cte.body.query_mut().expect("query CTE"),
            &|position| demanded.get(position).copied().unwrap_or(true),
            None,
            super::OutputPruningContext {
                production,
                aggregates,
                routines,
                volatility: Some(volatility),
                visible_ctes: &visible,
            },
        )?;
    }
    production.check()?;
    Ok(())
}

fn folded_ctes<'c>(
    query: &QueryPlan,
    volatility: &dyn VolatilityCatalog,
    production: ProductionControl<'c>,
) -> Result<ProductionVec<'c, usize>, SQLError> {
    let scheduled = uqa_sql::semantics::cte_strategy::schedule_plan_ctes_with_control(
        volatility, query, production,
    )?;
    let mut names = ProductionMap::new(production);
    for cte in scheduled.iter().filter(|cte| cte.deferred) {
        names.insert_copy(cte.plan.name.as_str(), None)?;
    }
    drop(scheduled);
    for (index, cte) in query.ctes.iter().enumerate() {
        production.check()?;
        if names.get(cte.name.as_str()) == Some(&None) {
            names.insert_copy(cte.name.as_str(), Some(index))?;
        }
    }
    let mut indices = ProductionVec::new(production);
    indices.reserve(names.len())?;
    for (_, index) in &names {
        indices.push_copy(index.expect("scheduled CTE"))?;
    }
    Ok(indices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SourcePlan;
    use uqa_sql::ScalarExpr;
    mod control;
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
            prune(&mut query, &Catalog, &Catalog, None, uqa_core::memory::ProductionControl::uncontrolled()).unwrap();
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
            prune(
                &mut query,
                &Catalog,
                &Catalog,
                None,
                uqa_core::memory::ProductionControl::uncontrolled(),
            )
            .unwrap();
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
