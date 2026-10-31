//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{CommandPlan, CommandScalarSlot, MutableExecutablePlan, OwnedScalarExpr};

fn command(sql: &str) -> CommandPlan {
    let UnifiedPlan::Command(command) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        unreachable!()
    };
    *command
}

#[test]
fn utility_body_rewrites_preserve_nested_query_and_command_constructor_ownership() {
    for sql in [
        "EXPLAIN SELECT 'original'",
        "PREPARE p AS UPDATE t SET n = 'original'",
    ] {
        let source = command(sql);
        let budget = MemoryBudget::new(4 * 1024 * 1024);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let input = source.copy_for_rewrite(&control).unwrap();
        let before = budget.used();
        let mut output = None;
        let allocations = allocation_counter::measure(|| {
            output = Some(
                input
                    .map_executable_body(&control, |body| {
                        let replacement = OwnedScalarExpr::literal(
                            control.copy_value(&Value::Str("replacement".repeat(1024)))?,
                            &control,
                        )?;
                        Ok(match body {
                            MutableExecutablePlan::Query(query) => {
                                MutableExecutablePlan::Query(query.map_scalar(
                                    QueryScalarSlot::Projection(0),
                                    &control,
                                    |_| Ok(replacement),
                                )?)
                            }
                            MutableExecutablePlan::Command(command) => {
                                MutableExecutablePlan::Command(command.map_scalar(
                                    CommandScalarSlot::Assignment(0),
                                    &control,
                                    |_| Ok(replacement),
                                )?)
                            }
                        })
                    })
                    .unwrap(),
            );
        });
        let output = output.unwrap();
        assert_eq!(
            before as i64 + allocations.bytes_current,
            budget.used() as i64,
            "{sql}"
        );
        assert_eq!(output.reserved_bytes(), budget.used());
        drop(output.finish(&control).unwrap());
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn utility_body_callback_cancellation_and_unwind_drop_all_private_wrappers() {
    for sql in [
        "EXPLAIN SELECT 'original'",
        "PREPARE p AS UPDATE t SET n = 'original'",
    ] {
        let source = command(sql);
        for mode in 0..4 {
            let budget = MemoryBudget::new(4 * 1024 * 1024);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&budget, &original, &invoking);
            let input = source.copy_for_rewrite(&control).unwrap();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                input.map_executable_body(&control, |body| {
                    match mode {
                        0 => return Err(SQLError::Internal("body callback failure".into())),
                        1 => original.cancel(),
                        2 => invoking.cancel(),
                        _ => panic!("injected body callback unwind"),
                    }
                    Ok(body)
                })
            }));
            if mode == 3 {
                assert!(result.is_err());
            } else {
                assert!(result.unwrap().is_err());
            }
            assert_eq!(budget.used(), 0, "{sql}, mode {mode}");
        }
    }
}

#[test]
fn lowered_explain_keeps_independent_parameter_constructor_ownership() {
    let syntax = crate::compile("EXPLAIN SELECT $1").unwrap().remove(0);
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let MutableExecutablePlan::Command(command) = UnifiedPlan::lower_borrowed_for_rewrite(
        &syntax,
        &crate::plan::NoRegisteredAggregates,
        &control,
    )
    .unwrap() else {
        panic!("EXPLAIN")
    };
    let before = budget.used();
    let mut changed = Some(command);
    let allocations = allocation_counter::measure(|| {
        let mut parameters = uqa_core::memory::ProductionVec::new(control);
        parameters
            .push_produced(
                control
                    .finish(
                        crate::SQLParam::Scalar(Value::Int(1)),
                        control.empty_reservation(),
                    )
                    .unwrap(),
            )
            .unwrap();
        let command = changed
            .take()
            .unwrap()
            .set_explain_parameters(Some(parameters.finish().unwrap()), &control)
            .unwrap();
        changed = Some(command.set_explain_parameters(None, &control).unwrap());
    });
    assert_eq!(allocations.bytes_current, 0);
    assert_eq!(budget.used(), before);
    drop(changed);
    assert_eq!(budget.used(), 0);
}
