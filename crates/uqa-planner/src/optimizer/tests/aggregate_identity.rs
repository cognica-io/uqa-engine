//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn caller_owned_compute_classification_charges_registered_aggregate_observation() {
    use std::cell::Cell;
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken,
    };

    #[derive(Default)]
    struct Aggregates {
        ordinary: Cell<usize>,
        admitted: Cell<usize>,
    }

    impl AggregateClassifier for Aggregates {
        fn is_registered_aggregate(&self, name: &str) -> bool {
            self.ordinary.set(self.ordinary.get() + 1);
            name == "custom_agg"
        }

        fn is_registered_aggregate_with_control(
            &self,
            name: &str,
            control: &ProductionControl<'_>,
        ) -> Result<bool, uqa_sql::SQLError> {
            self.admitted.set(self.admitted.get() + 1);
            let lookup = control.copy_text(name)?;
            Ok(lookup.as_str() == "custom_agg")
        }
    }

    for limit in [0, 256 * 1024] {
        let plan = UnifiedPlan::lower(
            compile("SELECT custom_agg(n) AS value FROM t")
                .unwrap()
                .remove(0),
        );
        let budget = MemoryBudget::new(limit);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let mut config =
            OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control);
        config.production = ProductionControl::new(&budget, &original, &invoking);
        let aggregates = Aggregates::default();
        let result = optimize_with_aggregates(plan, &config, &aggregates);
        assert_eq!(aggregates.ordinary.get(), 0);
        assert!(aggregates.admitted.get() > 0);
        if limit == 0 {
            assert!(
                matches!(result, Err(OptimizerError::Expression(error)) if error.sqlstate() == Some("53200"))
            );
        } else {
            assert!(matches!(
                query_block(&result.unwrap()).compute,
                ComputePlan::Aggregate
            ));
            assert!(budget.peak() > 0);
        }
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn recomputed_aggregate_kind_preserves_qualified_builtin_and_selected_scalar() {
    for sql in [
        "SELECT pg_catalog.count(*) FROM t",
        "SELECT pg_catalog.sum(n) FROM t",
        "SELECT coalesce(pg_catalog.count(*), 0) FROM t",
    ] {
        assert!(
            matches!(query_block(&optimized(sql)).compute, ComputePlan::Aggregate),
            "{sql}"
        );
    }
    let mut plan = UnifiedPlan::lower(compile("SELECT count() FROM t").unwrap().remove(0));
    let UnifiedPlan::Query(query) = &mut plan else {
        unreachable!()
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        unreachable!()
    };
    let ScalarExpr::Func { binding, .. } = &mut block.projections[0].expr else {
        unreachable!()
    };
    let mut selected = uqa_sql::ast::FunctionBinding::builtin_by_name("app.count", None);
    selected.builtin = false;
    selected.object_id = Some([42; 16]);
    *binding = Some(selected);
    let plan = optimize(
        plan,
        &OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control),
    )
    .unwrap();
    assert!(matches!(query_block(&plan).compute, ComputePlan::Project));
}
