//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::Expr, plan::ExpressionPlan};
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn source() -> Expr {
    Expr::Func {
        name: "jsonb_strip_nulls".into(),
        binding: Some(FunctionBinding {
            object_id: None,
            name: "pg_catalog.jsonb_strip_nulls".into(),
            argument_types: vec!["jsonb".into(), "boolean".into()],
            builtin: true,
            dispatch: None,
            invocation: None,
            resolution_error: None,
        }),
        args: vec![Expr::Literal(Value::Str("{\"field\":null}".into()))],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    }
}

fn input(source: &Expr, budget: &MemoryBudget) -> Produced<SelectedCall> {
    let cancellation = CancellationToken::new();
    let lowered =
        ExpressionPlan::lower_column_budgeted(source, budget, &cancellation, &cancellation)
            .unwrap();
    let (scalar, memory) = lowered.into_parts();
    let ScalarExpr::Func {
        binding: Some(binding),
        args,
        ..
    } = scalar
    else {
        panic!("fixture has a selected call");
    };
    ProductionControl::new(budget, &cancellation, &cancellation)
        .finish(
            SelectedCall {
                binding,
                arguments: args,
            },
            Some(memory),
        )
        .unwrap()
}

#[test]
fn selected_constructors_keep_input_payload_and_admit_cast_defaults_and_metadata() {
    let budget = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let input = input(&source(), &budget);
    let ScalarExpr::Literal(Value::Str(text)) = &input.arguments[0] else {
        panic!()
    };
    let pointer = text.as_ptr();
    let original_bytes = input.reserved_bytes();
    let (matched, output) =
        bind_call_with_control(input, &[None], &[Some(ColumnType::Text)], &[None], &control)
            .unwrap();
    assert!(matched);
    assert_eq!(output.arguments.len(), 2);
    assert_eq!(output.arguments[1], ScalarExpr::Literal(Value::Bool(false)));
    let ScalarExpr::Cast { expr, ty } = &output.arguments[0] else {
        panic!()
    };
    let ScalarExpr::Literal(Value::Str(text)) = expr.as_ref() else {
        panic!()
    };
    assert_eq!(text.as_ptr(), pointer);
    assert_eq!(ty, "jsonb");
    assert_eq!(output.binding.argument_types, ["jsonb", "boolean"]);
    assert!(output.reserved_bytes() > original_bytes);
    assert_eq!(budget.used(), output.reserved_bytes());
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn invalid_selected_match_preserves_input_and_releases_matching_scratch() {
    let budget = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let input = input(&source(), &budget);
    let original_bytes = input.reserved_bytes();
    let original_binding = input.binding.clone();
    let original_arguments = input.arguments.clone();
    let (matched, output) = bind_call_with_control(
        input,
        &[None],
        &[Some(ColumnType::Boolean)],
        &[Some(ColumnType::Boolean)],
        &control,
    )
    .unwrap();
    assert!(!matched);
    assert_eq!(output.binding, original_binding);
    assert_eq!(output.arguments, original_arguments);
    assert_eq!(output.reserved_bytes(), original_bytes);
    assert_eq!(budget.used(), original_bytes);
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn selected_constructor_failure_drops_partial_outputs_and_preserves_other_owners() {
    for available in [0, 32, 128, 512, 1024, 4096] {
        let budget = MemoryBudget::new(1 << 20);
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        let input = input(&source(), &budget);
        let other = budget
            .reserve(budget.limit() - budget.used() - available)
            .unwrap();
        let other_bytes = other.bytes();
        let result =
            bind_call_with_control(input, &[None], &[Some(ColumnType::Text)], &[None], &control);
        match result {
            Ok((matched, output)) => {
                assert!(matched);
                assert_eq!(budget.used(), other_bytes + output.reserved_bytes());
                drop(output);
            }
            Err(error) => assert_eq!(error.sqlstate(), Some("53200")),
        }
        assert_eq!(budget.used(), other_bytes);
        drop(other);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn either_caller_cancellation_releases_selected_input() {
    for cancel_original in [true, false] {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let input = input(&source(), &budget);
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let error = bind_call_with_control(
            input,
            &[None],
            &[Some(ColumnType::Text)],
            &[None],
            &ProductionControl::new(&budget, &original, &invoking),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn selected_input_rejects_foreign_allowance_and_mode_before_no_match_return() {
    for controlled_target in [true, false] {
        let source_budget = MemoryBudget::new(1 << 20);
        let target_budget = MemoryBudget::new(1 << 20);
        let cancellation = CancellationToken::new();
        let input = input(&source(), &source_budget);
        let control = if controlled_target {
            ProductionControl::new(&target_budget, &cancellation, &cancellation)
        } else {
            ProductionControl::uncontrolled()
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            bind_call_with_control(
                input,
                &[None],
                &[Some(ColumnType::Boolean)],
                &[Some(ColumnType::Boolean)],
                &control,
            )
        }));
        assert!(result.is_err());
        assert_eq!(source_budget.used(), 0);
        assert_eq!(target_budget.used(), 0);
    }
    let budget = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let input = ProductionControl::uncontrolled()
        .finish(
            SelectedCall {
                binding: FunctionBinding {
                    object_id: None,
                    name: "absent".into(),
                    argument_types: Vec::new(),
                    builtin: true,
                    dispatch: None,
                    invocation: None,
                    resolution_error: None,
                },
                arguments: Vec::new(),
            },
            None,
        )
        .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        bind_call_with_control(
            input,
            &[],
            &[],
            &[],
            &ProductionControl::new(&budget, &cancellation, &cancellation),
        )
    }));
    assert!(result.is_err());
    assert_eq!(budget.used(), 0);
}

struct SharedRoot {
    call: SelectedCall,
    sibling: String,
    memory: Option<MemoryReservation>,
}

#[test]
fn selected_in_place_construction_keeps_siblings_charged_through_partial_failure() {
    for available in [0, 32, 128, 512, 1024, 8192] {
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let call = input(&source(), &budget);
        let sibling = control.copy_text("sibling outside selected call").unwrap();
        let (call, memory) = call.into_parts();
        let (sibling, extra) = sibling.into_parts();
        let mut root = SharedRoot {
            call,
            sibling,
            memory: control.combine(memory, extra),
        };
        let initial = budget.used();
        let other = budget
            .reserve(budget.limit() - initial - available)
            .unwrap();
        let result = bind_call_in_place_with_control(
            &mut root.call,
            &mut root.memory,
            &[None],
            &[Some(ColumnType::Text)],
            &[None],
            &control,
        );
        match result {
            Ok(matched) => {
                assert!(matched);
                assert_eq!(root.call.arguments.len(), 2);
            }
            Err(error) => assert_eq!(error.sqlstate(), Some("53200")),
        }
        assert_eq!(root.sibling, "sibling outside selected call");
        let retained = root.memory.as_ref().unwrap().bytes();
        assert!(retained >= initial);
        assert_eq!(budget.used(), other.bytes() + retained);
        drop(root);
        assert_eq!(budget.used(), other.bytes());
        drop(other);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn selected_in_place_cancellation_leaves_the_shared_lease_with_its_root() {
    for cancel_original in [false, true] {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let call = input(&source(), &budget);
        let sibling = control.copy_text("sibling outside selected call").unwrap();
        let (call, memory) = call.into_parts();
        let (sibling, extra) = sibling.into_parts();
        let mut root = SharedRoot {
            call,
            sibling,
            memory: control.combine(memory, extra),
        };
        let initial = budget.used();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let error = bind_call_in_place_with_control(
            &mut root.call,
            &mut root.memory,
            &[None],
            &[Some(ColumnType::Text)],
            &[None],
            &control,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), initial);
        assert_eq!(root.sibling, "sibling outside selected call");
        drop(root);
        assert_eq!(budget.used(), 0);
    }
}
