//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prune unused, side-effect-free mutation-source output slots without changing row shape.
use std::collections::BTreeSet;
use uqa_core::Value;
use uqa_sql::{
    plan::{ComputePlan, QueryPlan, RelationalPlan},
    SQLError, ScalarExpr,
};

pub fn prune_unused_query_outputs(
    volatility: &dyn uqa_sql::semantics::volatility::VolatilityCatalog,
    routines: &dyn uqa_sql::routines::RoutineResolution,
    query: &mut QueryPlan,
    required_positions: &BTreeSet<usize>,
    expected_width: usize,
    inherited_ctes: &dyn Fn(&str) -> bool,
) -> Result<(), SQLError> {
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        return Ok(());
    };
    if !crate::optimizer::expand_local_stars(
        block,
        query.analyzed_source.as_deref(),
        query.analyzed_output.as_ref(),
    ) {
        return Ok(());
    }
    let projection_can_be_pruned = matches!(block.compute, ComputePlan::Project)
        && !block.distinct
        && block.distinct_on.is_empty()
        && block.order_by.is_empty()
        && block.projections.len() == expected_width;
    if !projection_can_be_pruned {
        return Ok(());
    }
    let visible_ctes =
        |name: &str| query.ctes.iter().any(|cte| cte.name == name) || inherited_ctes(name);
    for (position, projection) in block.projections.iter_mut().enumerate() {
        if !required_positions.contains(&position)
            && !uqa_sql::semantics::sets::static_setness::expression_may_return_set_statically(
                routines,
                &projection.expr,
            )
            && !uqa_sql::semantics::volatility::scalar_in_scope_contains_volatile_function(
                volatility,
                &projection.expr,
                &block.subqueries,
                &visible_ctes,
            )?
        {
            projection.expr = ScalarExpr::Literal(Value::Null);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_sql::{
        ast::{FunctionBinding, FunctionVolatility},
        plan::UnifiedPlan,
        SQLError,
    };

    struct Catalog;
    impl uqa_sql::FunctionTypeResolver for Catalog {
        fn resolve_function_type(
            &self,
            _: &str,
            _: Option<&FunctionBinding>,
            _: &[Option<String>],
            _: &[Option<uqa_sql::ColumnType>],
            _: bool,
        ) -> Result<Option<uqa_sql::ColumnType>, SQLError> {
            Ok(None)
        }
    }
    impl uqa_sql::routines::RoutineResolution for Catalog {
        fn has_registered_table_function(&self, name: &str) -> bool {
            name == "host_rows"
        }
    }
    impl uqa_sql::semantics::volatility::VolatilityCatalog for Catalog {
        fn host_function_volatility(&self, name: &str) -> Option<FunctionVolatility> {
            match name {
                "host_effect" => Some(FunctionVolatility::Volatile),
                "host_rows" => Some(FunctionVolatility::Immutable),
                _ => None,
            }
        }
        fn routine_volatilities(
            &self,
            _: &str,
            _: Option<&FunctionBinding>,
        ) -> Option<Vec<FunctionVolatility>> {
            None
        }
        fn view_query(&self, _: &str) -> Result<Option<QueryPlan>, SQLError> {
            Ok(None)
        }
    }

    #[test]
    fn star_rule_inputs_use_the_retained_positional_descriptor() {
        let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(
            uqa_sql::compile("SELECT * FROM (SELECT 7 AS used,(SELECT 1/0) AS unused) input")
                .unwrap()
                .remove(0),
        ) else {
            panic!("query");
        };
        let schema = uqa_sql::RowSchema::with_qualified_types(
            "input",
            vec!["used".into(), "unused".into()],
            vec![Some(uqa_sql::ColumnType::Integer); 2],
        );
        query.analyzed_output = Some(schema.clone());
        query.analyzed_source = Some(std::sync::Arc::new(uqa_sql::plan::AnalyzedSource {
            schema,
            children: None,
        }));
        prune_unused_query_outputs(
            &Catalog,
            &Catalog,
            &mut query,
            &BTreeSet::from([0]),
            2,
            &|_| false,
        )
        .unwrap();
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("block");
        };
        assert_eq!(block.projections.len(), 2);
        assert!(matches!(
            block.projections[0].expr,
            ScalarExpr::QualifiedColumn { .. }
        ));
        assert_eq!(block.projections[1].expr, ScalarExpr::Literal(Value::Null));
    }

    #[test]
    fn discarded_mutation_outputs_preserve_volatile_effects_and_erase_unused_errors() {
        let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(
            uqa_sql::compile("SELECT 7, 1/0, nextval('effect'), host_effect()")
                .unwrap()
                .remove(0),
        ) else {
            panic!("query")
        };
        let before = query.clone();
        prune_unused_query_outputs(
            &Catalog,
            &Catalog,
            &mut query,
            &BTreeSet::from([0]),
            4,
            &|_| false,
        )
        .unwrap();
        let RelationalPlan::QueryBlock(block) = query.root else {
            panic!("block")
        };
        let RelationalPlan::QueryBlock(original) = before.root else {
            panic!("block")
        };
        assert_eq!(block.projections[0].expr, original.projections[0].expr);
        assert_eq!(block.projections[1].expr, ScalarExpr::Literal(Value::Null));
        for position in 2..4 {
            assert_eq!(
                block.projections[position].expr,
                original.projections[position].expr
            );
        }
    }

    #[test]
    fn mutation_output_pruning_preserves_order_and_distinct_inputs() {
        for sql in ["SELECT 7, 1/0 ORDER BY 2", "SELECT DISTINCT 7, 1/0"] {
            let UnifiedPlan::Query(mut query) =
                UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
            else {
                panic!("query")
            };
            let before = serde_json::to_value(&query).unwrap();
            prune_unused_query_outputs(
                &Catalog,
                &Catalog,
                &mut query,
                &BTreeSet::from([0]),
                2,
                &|_| false,
            )
            .unwrap();
            assert_eq!(serde_json::to_value(&query).unwrap(), before);
        }
    }

    #[test]
    fn unused_set_outputs_retain_source_cardinality_without_retaining_scalar_errors() {
        for expression in [
            "generate_series(1,3)",
            "unnest(ARRAY[1,2])",
            "generate_series(1,2) + 1",
            "host_rows()",
        ] {
            let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(
                uqa_sql::compile(&format!("SELECT 7, {expression}, 1/0"))
                    .unwrap()
                    .remove(0),
            ) else {
                panic!("query")
            };
            let before = query.clone();
            prune_unused_query_outputs(
                &Catalog,
                &Catalog,
                &mut query,
                &BTreeSet::from([0]),
                3,
                &|_| false,
            )
            .unwrap();
            let (RelationalPlan::QueryBlock(block), RelationalPlan::QueryBlock(original)) =
                (&query.root, &before.root)
            else {
                panic!("block")
            };
            assert_eq!(block.projections[1].expr, original.projections[1].expr);
            assert_eq!(block.projections[2].expr, ScalarExpr::Literal(Value::Null));
        }
    }
}
