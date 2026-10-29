//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{MutableExecutablePlan, OwnedScalarExpr};

const COMMANDS: &[&str] = &[
    "INSERT INTO t (a[1:2]) VALUES (ARRAY[3, 4]) ON CONFLICT (a) WHERE a = 5 DO UPDATE SET a[6] = 7 WHERE a = 8 RETURNING a + 9",
    "INSERT INTO t SELECT 10 FROM (SELECT 11 AS n) AS s RETURNING (SELECT 12)",
    "WITH q AS (SELECT 13 AS n) UPDATE t SET a[14:15] = ARRAY[16] FROM q WHERE a = 17 RETURNING a + 18",
    "DELETE FROM t USING (SELECT 19 AS n) AS s WHERE a = 20 RETURNING (SELECT 21)",
    "MERGE INTO t USING (SELECT 22 AS n) AS s ON a = 23 WHEN MATCHED AND a = 24 THEN UPDATE SET a[25] = 26 WHEN NOT MATCHED AND s.n = 27 THEN INSERT (a[28]) VALUES (29) RETURNING a + 30",
    "EXPLAIN SELECT 31, (SELECT 32)",
    "PREPARE p AS UPDATE t SET a = 33 RETURNING (SELECT 34)",
    "CREATE VIEW v AS SELECT 35",
    "CREATE MATERIALIZED VIEW mv AS SELECT 36",
    "CREATE TABLE created AS SELECT 37",
    "DECLARE c CURSOR FOR SELECT 38",
    "EXECUTE p(39, (SELECT 40))",
    "CALL f(41, (SELECT 42))",
];

#[test]
fn command_scalar_transforms_follow_original_postorder_and_owner_wrappers() {
    for sql in COMMANDS {
        let mut reference = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let source = reference.clone();
        let mut expected = Vec::new();
        reference.rewrite_scalar_expressions(&mut |node| {
            expected.push(serde_json::to_value(node).unwrap())
        });
        let budget = MemoryBudget::new(8 * 1024 * 1024);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let input = source.copy_for_rewrite(&control).unwrap();
        let mut actual = Vec::new();
        let output = input
            .transform_scalar_nodes(&control, &mut |_, node| {
                actual.push(serde_json::to_value(&*node).unwrap());
                Ok(node)
            })
            .unwrap();
        assert_eq!(actual, expected, "{sql}");
        assert_eq!(output.reserved_bytes(), budget.used(), "{sql}");
        let output = output.finish(&control).unwrap();
        assert_eq!(
            serde_json::to_value(&*output).unwrap(),
            serde_json::to_value(&source).unwrap(),
            "{sql}"
        );
        drop(output);
        assert_eq!(budget.used(), 0, "{sql}");
    }
}

#[test]
fn command_scalar_replacements_release_retired_payloads_and_all_failures() {
    let source = UnifiedPlan::lower(
        crate::compile("EXPLAIN UPDATE t SET a = 'old' WHERE a = 'filter' RETURNING 'result'")
            .unwrap()
            .remove(0),
    );
    for mode in 0..5 {
        let budget = MemoryBudget::new(4 * 1024 * 1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let input = source.copy_for_rewrite(&control).unwrap();
        let replacement = Value::Str("replacement".repeat(4096));
        let before = budget.used();
        let mut output = None;
        let allocation = allocation_counter::measure(|| {
            output = Some(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                || {
                    input.transform_scalar_nodes(&control, &mut |_, node| {
                        if !matches!(&*node, ScalarExpr::Literal(Value::Str(_))) {
                            return Ok(node);
                        }
                        match mode {
                            1 => {
                                return Err(SQLError::Internal(
                                    "injected scalar transform failure".into(),
                                ))
                            }
                            2 => original.cancel(),
                            3 => invoking.cancel(),
                            4 => panic!("injected scalar transform unwind"),
                            _ => {}
                        }
                        Ok(OwnedScalarExpr::literal(
                            control.copy_value(&replacement)?,
                            &control,
                        )?)
                    })
                },
            )));
        });
        let output = output.unwrap();
        if mode == 0 {
            let plan: MutableExecutablePlan = output.unwrap().unwrap();
            assert_eq!(
                before as i64 + allocation.bytes_current,
                budget.used() as i64
            );
            assert_eq!(plan.reserved_bytes(), budget.used());
            drop(plan);
        } else if mode == 4 {
            assert!(output.is_err());
        } else {
            assert!(output.unwrap().is_err());
        }
        assert_eq!(budget.used(), 0, "failure mode {mode}");
    }
}

#[test]
fn scalar_arena_edits_keep_mutation_roots_separate_from_detached_query_owners() {
    for sql in COMMANDS {
        let syntax = crate::compile(sql).unwrap().remove(0);
        let reference = UnifiedPlan::lower(syntax.clone());
        let expected = serde_json::to_value(&reference).unwrap();
        for lower in [false, true] {
            let budget = MemoryBudget::new(8 * 1024 * 1024);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&budget, &token, &token);
            let input = if lower {
                UnifiedPlan::lower_borrowed_for_rewrite(
                    &syntax,
                    &crate::plan::NoRegisteredAggregates,
                    &control,
                )
                .unwrap()
            } else {
                reference.copy_for_rewrite(&control).unwrap()
            };
            let before = budget.used();
            let mut output = None;
            let allocation = allocation_counter::measure(|| {
                output = Some(input.transform_scalar_arenas(&control, &mut |_, node, _| Ok(node)));
            });
            let output = output.unwrap().unwrap();
            assert_eq!(allocation.count_total, 0, "{sql}, lower={lower}");
            assert_eq!(budget.used(), before, "{sql}, lower={lower}");
            assert_eq!(output.reserved_bytes(), before);
            let output = output.finish(&control).unwrap();
            assert_eq!(serde_json::to_value(&*output).unwrap(), expected, "{sql}");
            drop(output);
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn failed_mutation_arena_edits_release_detached_queries_and_remaining_command_storage() {
    let source = UnifiedPlan::lower(
        crate::compile("DELETE FROM t WHERE a IN ((SELECT 1), 2) RETURNING (SELECT 3)")
            .unwrap()
            .remove(0),
    );
    for failure in 0..4 {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let sibling = budget.reserve(37).unwrap();
        let input = source.copy_for_rewrite(&control).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            input.transform_scalar_arenas(&control, &mut |_, node, _| {
                if !matches!(&*node, ScalarExpr::InList { .. }) {
                    return Ok(node);
                }
                match failure {
                    0 => return Err(SQLError::Internal("injected arena failure".into())),
                    1 => original.cancel(),
                    2 => invoking.cancel(),
                    _ => panic!("injected arena unwind"),
                }
                control.check()?;
                Ok(node)
            })
        }));
        if failure == 3 {
            assert!(result.is_err());
        } else {
            assert!(result.unwrap().is_err());
        }
        assert_eq!(budget.used(), 37, "failure={failure}");
        drop(sibling);
        assert_eq!(budget.used(), 0);
    }
}
