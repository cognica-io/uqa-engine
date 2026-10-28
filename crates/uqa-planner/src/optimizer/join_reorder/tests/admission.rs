//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{optimizer::OptimizerConfig, UnifiedPlan};
use uqa_core::{memory::MemoryBudget, memory::ProductionControl, CancellationToken};

#[test]
fn optimizer_routes_nested_join_search_through_request_admission() {
    for sql in [
        "SELECT a.id FROM a JOIN b ON a.id = b.id",
        "WITH joined AS MATERIALIZED (SELECT a.id FROM a JOIN b ON a.id = b.id) SELECT * FROM joined",
        "SELECT (SELECT a.id FROM a JOIN b ON a.id = b.id) FROM outer_table",
        "INSERT INTO target SELECT a.id FROM a JOIN b ON a.id = b.id",
        "UPDATE target SET id = a.id FROM a JOIN b ON a.id = b.id",
        "DELETE FROM target USING a JOIN b ON a.id = b.id",
        "MERGE INTO target USING (SELECT a.id FROM a JOIN b ON a.id = b.id) AS source ON target.id = source.id WHEN MATCHED THEN DELETE",
        "DECLARE c CURSOR FOR SELECT a.id FROM a JOIN b ON a.id = b.id",
    ] {
        let input = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0));
        let expected = crate::optimize(
            input.clone(),
            &OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control),
        ).unwrap();
        for failure in 0..4 {
            let budget = MemoryBudget::new(if failure == 0 { 0 } else { 128 * 1024 });
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            if failure == 1 {
                original.cancel();
            } else if failure == 2 {
                invoking.cancel();
            }
            let mut config = OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control);
            config.production = ProductionControl::new(&budget, &original, &invoking);
            let result = crate::optimize(input.clone(), &config);
            if failure == 3 {
                let actual = result.unwrap();
                assert_eq!(format!("{actual:?}"), format!("{expected:?}"), "{sql}");
                assert!(budget.peak() > 0, "join search bypassed admission: {sql}");
                drop(actual);
            } else {
                let error = result.unwrap_err();
                let crate::optimizer::OptimizerError::Expression(error) = error else {
                    panic!("lost typed production failure: {error}");
                };
                assert_eq!(error.sqlstate(), Some(if failure == 0 { "53200" } else { "57014" }), "{sql}: {error}");
            }
            assert_eq!(budget.used(), 0, "{sql}");
        }
    }
}
