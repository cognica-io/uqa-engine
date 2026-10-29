//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::{ast::FunctionOrderSyntax, ir::ScalarOrder};

fn text(bytes: usize) -> ScalarExpr {
    ScalarExpr::Literal(Value::Str("x".repeat(bytes)))
}

fn boolean(value: bool) -> ScalarExpr {
    ScalarExpr::Literal(Value::Bool(value))
}

fn config(control: ProductionControl<'_>, coerced: bool) -> OptimizerConfig<'_> {
    let mut config = OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control);
    config.production = control;
    config.coerced_conditionals = coerced;
    config
}

fn searched_case() -> ScalarExpr {
    ScalarExpr::Case {
        base: None,
        when: vec![
            (boolean(false), text(8192)),
            (ScalarExpr::Param(1), text(101)),
            (boolean(false), text(4096)),
            (boolean(true), text(203)),
            (ScalarExpr::Param(99), text(16384)),
        ],
        else_branch: Some(Box::new(text(32768))),
    }
}

fn visit(child: OwnedScalarExpr, visits: &mut Vec<usize>) -> Result<OwnedScalarExpr, SQLError> {
    let marker = match &*child {
        ScalarExpr::Param(99) => return Err(SQLError::Internal("unreachable condition".into())),
        ScalarExpr::Param(index) => 100 + index,
        ScalarExpr::Literal(Value::Bool(value)) => usize::from(*value),
        ScalarExpr::Literal(Value::Str(text)) if text.len() >= 4096 => {
            return Err(SQLError::Internal("unreachable result".into()));
        }
        ScalarExpr::Literal(Value::Str(text)) => 1000 + text.len(),
        ScalarExpr::Literal(Value::Null) => 2,
        _ => return Err(SQLError::Internal("unexpected reached child".into())),
    };
    visits.push(marker);
    Ok(child)
}

#[test]
fn admitted_case_compacts_selected_arms_and_reuses_the_else_owner_without_copying() {
    let source = searched_case();
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let input = source.copy_for_rewrite(&control).unwrap();
    let ScalarExpr::Case {
        when, else_branch, ..
    } = &*input
    else {
        unreachable!()
    };
    let arms = when.as_ptr();
    let fallback = std::ptr::from_ref(else_branch.as_deref().unwrap());
    let sibling = budget.reserve(37).unwrap();
    let before = budget.used();
    let mut visits = Vec::with_capacity(16);
    let mut output = None;
    let measured = allocation_counter::measure(|| {
        output = Some(
            optimize_case(input, &config(control, true), &control, |child| {
                visit(child, &mut visits)
            })
            .unwrap(),
        );
    });
    let output = output.unwrap();
    assert_eq!(visits, [0, 101, 1101, 0, 1, 1203]);
    assert_eq!(measured.count_total, 0);
    assert_eq!(
        measured.bytes_current,
        -i64::try_from(before - budget.used()).unwrap()
    );
    assert_eq!(output.reserved_bytes() + sibling.bytes(), budget.used());
    let ScalarExpr::Case {
        when, else_branch, ..
    } = &*output
    else {
        panic!("CASE prefix")
    };
    assert_eq!(when.as_ptr(), arms);
    assert_eq!(when, &[(ScalarExpr::Param(1), text(101))]);
    assert_eq!(
        std::ptr::from_ref(else_branch.as_deref().unwrap()),
        fallback
    );
    assert_eq!(else_branch.as_deref(), Some(&text(203)));
    let selected = output.select_child(2);
    assert_eq!(*selected, text(203));
    assert_eq!(budget.used(), sibling.bytes() + 203);
    drop((selected, sibling));
    assert_eq!(budget.used(), 0);
}

#[test]
fn uncoerced_case_retains_unvisited_type_arms_and_their_exact_owners() {
    let source = searched_case();
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let input = source.copy_for_rewrite(&control).unwrap();
    let before = budget.used();
    let mut visits = Vec::with_capacity(16);
    let output = optimize_case(input, &config(control, false), &control, |child| {
        visit(child, &mut visits)
    })
    .unwrap();
    assert_eq!(visits, [0, 101, 1101, 0, 1, 1203]);
    assert_eq!(*output, source);
    assert_eq!(budget.used(), before);
    assert_eq!(output.reserved_bytes(), before);
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn simple_case_rewrites_the_base_first_and_moves_only_the_matched_result() {
    let source = ScalarExpr::Case {
        base: Some(Box::new(ScalarExpr::Param(1))),
        when: vec![
            (ScalarExpr::Literal(Value::Null), ScalarExpr::Param(99)),
            (ScalarExpr::Param(2), text(257)),
            (ScalarExpr::Param(99), text(4096)),
        ],
        else_branch: Some(Box::new(text(8192))),
    };
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let input = source.copy_for_rewrite(&control).unwrap();
    let mut visits = Vec::new();
    let output = optimize_case(
        input,
        &config(control, true),
        &control,
        |child| match &*child {
            ScalarExpr::Param(index @ (1 | 2)) => {
                visits.push(*index);
                drop(child);
                Ok(OwnedScalarExpr::literal(
                    control.finish(Value::Int(7), control.empty_reservation())?,
                    &control,
                )?)
            }
            ScalarExpr::Literal(Value::Null) => {
                visits.push(0);
                Ok(child)
            }
            ScalarExpr::Literal(Value::Str(value)) if value.len() == 257 => {
                visits.push(3);
                Ok(child)
            }
            _ => Err(SQLError::Internal("unreachable simple CASE child".into())),
        },
    )
    .unwrap();
    assert_eq!(visits, [1, 0, 2, 3]);
    assert_eq!(*output, text(257));
    assert_eq!(output.reserved_bytes(), 257);
    assert_eq!(budget.used(), 257);
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn exhausted_case_selects_else_or_null_and_releases_all_parent_storage() {
    for has_else in [false, true] {
        let source = ScalarExpr::Case {
            base: None,
            when: vec![
                (boolean(false), text(8192)),
                (ScalarExpr::Literal(Value::Null), text(4096)),
            ],
            else_branch: has_else.then(|| Box::new(text(257))),
        };
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let input = source.copy_for_rewrite(&control).unwrap();
        let sibling = budget.reserve(budget.available()).unwrap();
        let mut visits = Vec::with_capacity(8);
        let output = optimize_case(input, &config(control, true), &control, |child| {
            visit(child, &mut visits)
        })
        .unwrap();
        assert_eq!(
            visits,
            if has_else {
                vec![0, 2, 1257]
            } else {
                vec![0, 2]
            }
        );
        assert_eq!(
            *output,
            if has_else {
                text(257)
            } else {
                ScalarExpr::Literal(Value::Null)
            }
        );
        assert_eq!(output.reserved_bytes(), if has_else { 257 } else { 0 });
        assert_eq!(budget.used(), sibling.bytes() + output.reserved_bytes());
        drop((output, sibling));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn selected_case_fallback_rejects_unadmitted_box_and_recovers_without_copying_results() {
    let source = ScalarExpr::Case {
        base: None,
        when: vec![
            (ScalarExpr::Param(1), ScalarExpr::Param(2)),
            (boolean(true), ScalarExpr::Param(3)),
        ],
        else_branch: None,
    };
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let input = source.copy_for_rewrite(&control).unwrap();
    let sibling = budget.reserve(budget.available()).unwrap();
    let error = optimize_case(input, &config(control, true), &control, Ok).unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(budget.used(), sibling.bytes());
    drop(sibling);
    let input = source.copy_for_rewrite(&control).unwrap();
    let before = budget.used();
    let mut output = None;
    let measured = allocation_counter::measure(|| {
        output = Some(optimize_case(input, &config(control, true), &control, Ok).unwrap());
    });
    let output = output.unwrap();
    assert_eq!(
        measured.bytes_current,
        i64::try_from(budget.used() - before).unwrap()
    );
    assert_eq!(
        *output,
        ScalarExpr::Case {
            base: None,
            when: vec![(ScalarExpr::Param(1), ScalarExpr::Param(2))],
            else_branch: Some(Box::new(ScalarExpr::Param(3))),
        }
    );
    assert_eq!(output.reserved_bytes(), budget.used());
    drop(output);
    assert_eq!(budget.used(), 0);
}

fn coalesce() -> ScalarExpr {
    ScalarExpr::Func {
        order_syntax: FunctionOrderSyntax::Ordinary,
        name: "coalesce".into(),
        binding: None,
        args: vec![
            ScalarExpr::Param(1),
            ScalarExpr::Literal(Value::Null),
            text(257),
            text(8192),
        ],
        distinct: false,
        order_by: vec![ScalarOrder {
            expr: ScalarExpr::Param(99),
            descending: false,
            nulls: None,
        }],
        filter: Some(Box::new(text(4096))),
    }
}

#[test]
fn coalesce_keeps_metadata_owners_and_retains_unbound_type_arms_without_evaluation() {
    for coerced in [false, true] {
        let source = coalesce();
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let input = source.copy_for_rewrite(&control).unwrap();
        let ScalarExpr::Func { args, filter, .. } = &*input else {
            unreachable!()
        };
        let buffer = args.as_ptr();
        let filter_buffer = std::ptr::from_ref(filter.as_deref().unwrap());
        let before = budget.used();
        let mut visits = Vec::with_capacity(8);
        let mut output = None;
        let measured = allocation_counter::measure(|| {
            output = Some(
                optimize_coalesce(input, &config(control, coerced), &control, |child| {
                    visit(child, &mut visits)
                })
                .unwrap(),
            );
        });
        let output = output.unwrap();
        assert_eq!(visits, [101, 2, 1257]);
        assert_eq!(measured.count_total, 0);
        assert_eq!(before - budget.used(), if coerced { 8192 } else { 0 });
        assert_eq!(
            measured.bytes_current,
            -i64::try_from(before - budget.used()).unwrap()
        );
        let ScalarExpr::Func { args, filter, .. } = &*output else {
            unreachable!()
        };
        assert_eq!(args.as_ptr(), buffer);
        assert_eq!(args.len(), if coerced { 3 } else { 4 });
        assert_eq!(
            std::ptr::from_ref(filter.as_deref().unwrap()),
            filter_buffer
        );
        let index = output.child_count() - 1;
        let filter = output.select_child(index);
        assert_eq!(*filter, text(4096));
        assert_eq!(budget.used(), 4096);
        drop(filter);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn conditional_callback_error_unwind_and_cancellation_release_every_private_owner() {
    for call in [false, true] {
        for failure in 0..4 {
            let budget = MemoryBudget::new(1 << 20);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&budget, &original, &invoking);
            let source = if call { coalesce() } else { searched_case() };
            let input = source.copy_for_rewrite(&control).unwrap();
            let sibling = budget.reserve(37).unwrap();
            let mut visits = 0;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let callback = |child| {
                    visits += 1;
                    if visits == 2 {
                        match failure {
                            0 => return Err(SQLError::Internal("callback failed".into())),
                            1 => panic!("callback unwind"),
                            2 => original.cancel(),
                            _ => invoking.cancel(),
                        }
                    }
                    Ok(child)
                };
                if call {
                    optimize_coalesce(input, &config(control, true), &control, callback)
                } else {
                    optimize_case(input, &config(control, true), &control, callback)
                }
            }));
            if failure == 1 {
                assert!(result.is_err());
            } else {
                let error = result.unwrap().unwrap_err();
                if failure == 0 {
                    assert!(error.to_string().contains("callback failed"));
                } else {
                    assert_eq!(error.sqlstate(), Some("57014"));
                }
            }
            assert_eq!(visits, 2);
            assert_eq!(budget.used(), sibling.bytes());
            drop(sibling);
            assert_eq!(budget.used(), 0);
        }
    }
}
