//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

fn planned(
    sql: &str,
    admitted: bool,
    cancel_original: bool,
    cancel_invoking: bool,
) -> Result<serde_json::Value, String> {
    let memory = MemoryBudget::new(8 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let config = OptimizerConfig {
        production: control,
        ..OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control)
    };
    let input = UnifiedPlan::lower(compile(sql).unwrap().remove(0));
    let native = admitted.then(|| input.copy_for_rewrite(&control).unwrap());
    if cancel_original {
        original.cancel();
    }
    if cancel_invoking {
        invoking.cancel();
    }
    let output = match native {
        Some(native) => optimize_owned_with_aggregates_and_statistics(
            native,
            &config,
            &api::NoRegisteredAggregates,
            &|_: &str| None,
        )
        .and_then(|output| Ok(output.finish(&control)?)),
        None => optimize(input, &config),
    };
    let result = match output {
        Ok(output) => {
            let (plan, reservation) = output.into_parts();
            let plan = serde_json::to_value(plan).unwrap();
            drop(reservation);
            Ok(plan)
        }
        Err(OptimizerError::Expression(error)) => Err(error.sqlstate().unwrap().to_string()),
        Err(error) => panic!("unexpected plan error for {sql}: {error}"),
    };
    assert_eq!(memory.used(), 0, "{sql}, admitted {admitted}");
    result
}

#[test]
fn caller_and_admitted_traversal_preserve_statement_shape_and_cancellation() {
    for (sql, failure) in [
        ("WITH unused AS (VALUES (1/0)), live AS (VALUES (2+3)) SELECT * FROM live UNION ALL SELECT 6", None),
        ("INSERT INTO target(id) VALUES(1+2), (4) ON CONFLICT(id) DO UPDATE SET id=excluded.id+1 WHERE false OR true RETURNING id+2", None),
        ("UPDATE target SET id=s.n+1 FROM (VALUES (2)) s(n) WHERE target.id=s.n AND EXISTS (SELECT 7) RETURNING target.id", None),
        ("DELETE FROM target USING (VALUES (2)) s(n) WHERE target.id=s.n AND true RETURNING target.id+1", None),
        ("MERGE INTO target USING (VALUES (1+1)) s(n) ON target.id=s.n WHEN MATCHED AND true THEN UPDATE SET id=s.n+3 WHEN NOT MATCHED THEN INSERT(id) VALUES(s.n+4) RETURNING merge_action()", None),
        ("DECLARE rows_cursor CURSOR FOR SELECT * FROM (VALUES (3+4)) s(n)", None),
        ("CALL example((SELECT 1+2))", None),
        ("EXPLAIN SELECT 1/0", None),
        ("SELECT 1/0", Some("22012")),
    ] {
        let caller = planned(sql, false, false, false);
        let native = planned(sql, true, false, false);
        assert_eq!(caller.as_ref().err().map(String::as_str), failure, "{sql}");
        assert_eq!(caller, native, "{sql}");
        for admitted in [false, true] {
            for (original, invoking) in [(true, false), (false, true)] {
                assert_eq!(planned(sql, admitted, original, invoking).unwrap_err(), "57014", "{sql}");
            }
        }
    }
}
