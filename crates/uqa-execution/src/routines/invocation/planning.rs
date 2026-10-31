//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transfer a routine's execution result into a separately owned planning constant.

use super::{context::RoutineInvocationContext, handlers::call_bound_user_scalar_function};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};
use uqa_sql::{ast::FunctionBinding, SQLError};

/// Keep authorization, body caches, activation state and transaction behavior at the ordinary execution boundary. Arguments are borrowed from the admitted scalar evaluator. The returned planning destination is admitted before copying; the executor's temporary value retains its existing execution lifetime and is not reported as planning storage.
pub fn evaluate_bound_scalar_for_planning(
    context: &RoutineInvocationContext<'_>,
    binding: &FunctionBinding,
    arguments: &[(Option<String>, Value)],
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>, SQLError>> {
    if let Err(error) = control.check() {
        return Some(Err(error.into()));
    }
    let nested = RoutineInvocationContext {
        runtime: crate::routines::RoutineContext {
            planning: Some(*control),
            ..context.runtime
        },
        session: context.session,
        lookup: context.lookup,
        overloads: context.overloads,
        types: context.types,
        authority: context.authority,
    };
    call_bound_user_scalar_function(&nested, binding, arguments)
        .map(|result| transfer_result(result, control))
}

fn transfer_result(
    result: Result<Value, SQLError>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let execution_value = result?;
    control.check()?;
    if control.budget().is_none() {
        Ok(control.finish(execution_value, None)?)
    } else {
        Ok(control.copy_value(&execution_value)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{memory::MemoryBudget, CancellationToken};

    #[test]
    fn planning_result_admits_its_own_destination_before_large_copy() {
        let memory = MemoryBudget::new(8192);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let sibling = memory.reserve(1024).unwrap();
        // This buffer belongs to the completed executor, outside planning's allocation observation.
        let value = Value::Str("x".repeat(4 * 1024 * 1024));
        let mut result = None;
        let allocations = allocation_counter::measure(|| {
            result = Some(transfer_result(Ok(value), &control));
        });
        assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
        assert!(allocations.bytes_total < 4096, "{allocations:?}");
        assert_eq!(memory.used(), 1024);
        drop(sibling);
        assert_eq!(memory.used(), 0);
    }

    #[test]
    fn planning_result_keeps_only_selected_output_and_checks_both_cancellations() {
        let memory = MemoryBudget::new(65536);
        for cancelled in [None, Some(false), Some(true)] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&memory, &original, &invoking);
            let value = Value::Str("returned result".repeat(128));
            if let Some(original_cancelled) = cancelled {
                if original_cancelled {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
            }
            let result = transfer_result(Ok(value), &control);
            if cancelled.is_some() {
                assert_eq!(result.unwrap_err().sqlstate(), Some("57014"));
            } else {
                let output = result.unwrap();
                assert_eq!(*output, Value::Str("returned result".repeat(128)));
                assert_eq!(memory.used(), output.reserved_bytes());
                assert!(memory.used() > 0);
                drop(output);
            }
            assert_eq!(memory.used(), 0);
        }
    }
}
