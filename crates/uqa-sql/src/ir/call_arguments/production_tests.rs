//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn decoded_arguments_borrow_input_ir_and_hold_exact_temporary_capacity() {
    let args = vec![
        ScalarExpr::Literal(Value::Int(1)),
        ScalarExpr::Column("column".into()),
    ];
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let decoded = scalar_call_arguments_with_control(&args, &control).unwrap();
    assert_eq!(*decoded, scalar_call_arguments(&args).unwrap());
    assert!(std::ptr::eq(decoded[1].value, &raw const args[1]));
    assert_eq!(
        decoded.reserved_bytes(),
        decoded.capacity() * size_of::<ScalarCallArgument<'_>>()
    );
    assert_eq!(budget.used(), decoded.reserved_bytes());
    drop(decoded);
    assert_eq!(budget.used(), 0);
}

#[test]
fn argument_scratch_failure_and_both_cancellation_tokens_release_memory() {
    let args = [ScalarExpr::Literal(Value::Int(1))];
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    assert_eq!(
        scalar_call_arguments_with_control(&args, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert!(scalar_call_arguments_with_control(&[], &control).is_ok());
    for original_cancelled in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        assert_eq!(
            scalar_call_arguments_with_control(&args, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
    }
    assert_eq!(budget.used(), 0);
}

#[test]
fn variadic_validation_keeps_duplicate_error_before_final_position_error() {
    let expression = ScalarExpr::Literal(Value::Null);
    let variadic = ScalarCallArgument {
        name: None,
        value: &expression,
        explicit_variadic: true,
    };
    let ordinary = ScalarCallArgument {
        explicit_variadic: false,
        ..variadic
    };
    assert!(validate_scalar_call_arguments(&[variadic, ordinary])
        .unwrap_err()
        .to_string()
        .contains("final call argument"));
    assert!(
        validate_scalar_call_arguments(&[variadic, variadic, ordinary])
            .unwrap_err()
            .to_string()
            .contains("more than one")
    );
    assert!(validate_scalar_call_arguments(&[ordinary, variadic]).unwrap());
    assert!(!validate_scalar_call_arguments(&[ordinary]).unwrap());
}
