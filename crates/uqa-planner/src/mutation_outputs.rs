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
    ScalarExpr,
};

pub fn prune_unused_query_outputs(
    volatility: &dyn uqa_sql::semantics::volatility::VolatilityCatalog,
    query: &mut QueryPlan,
    required_positions: &BTreeSet<usize>,
    expected_width: usize,
) {
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        return;
    };
    let projection_can_be_pruned = matches!(block.compute, ComputePlan::Project)
        && !block.distinct
        && block.distinct_on.is_empty()
        && block.order_by.is_empty()
        && block.projections.len() == expected_width;
    if !projection_can_be_pruned {
        return;
    }
    for (position, projection) in block.projections.iter_mut().enumerate() {
        if !required_positions.contains(&position)
            && !uqa_sql::semantics::volatility::expr_contains_volatile_function(
                volatility,
                &projection.expr,
            )
        {
            projection.expr = ScalarExpr::Literal(Value::Null);
        }
    }
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
    impl uqa_sql::semantics::volatility::VolatilityCatalog for Catalog {
        fn host_function_volatility(&self, name: &str) -> Option<FunctionVolatility> {
            (name == "host_effect").then_some(FunctionVolatility::Volatile)
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
    fn discarded_mutation_outputs_preserve_volatile_effects_and_erase_unused_errors() {
        let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(
            uqa_sql::compile("SELECT 7, 1/0, nextval('effect'), host_effect()")
                .unwrap()
                .remove(0),
        ) else {
            panic!("query")
        };
        let before = query.clone();
        prune_unused_query_outputs(&Catalog, &mut query, &BTreeSet::from([0]), 4);
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
            prune_unused_query_outputs(&Catalog, &mut query, &BTreeSet::from([0]), 2);
            assert_eq!(serde_json::to_value(&query).unwrap(), before);
        }
    }
}
