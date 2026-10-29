//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn node(and: bool, children: Vec<ScalarExpr>) -> ScalarExpr {
    if and {
        ScalarExpr::And(children)
    } else {
        ScalarExpr::Or(children)
    }
}

fn boolean(value: bool) -> ScalarExpr {
    ScalarExpr::Literal(Value::Bool(value))
}

fn config(control: ProductionControl<'_>, simplify: bool) -> OptimizerConfig<'_> {
    let mut config = OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control);
    config.production = control;
    config.enable_boolean_simplify = simplify;
    config
}

fn lazy(and: bool) -> ScalarExpr {
    node(
        and,
        vec![
            ScalarExpr::Param(1),
            node(
                and,
                vec![
                    boolean(and),
                    node(
                        and,
                        vec![ScalarExpr::Param(2), boolean(!and), ScalarExpr::Param(99)],
                    ),
                    node(!and, vec![ScalarExpr::Param(98)]),
                ],
            ),
            ScalarExpr::Param(97),
        ],
    )
}

#[test]
fn boolean_flattening_preserves_short_circuit_order_and_unvisited_structure() {
    for and in [false, true] {
        for simplify in [false, true] {
            let source = lazy(and);
            let budget = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&budget, &token, &token);
            let input = source.copy_for_rewrite(&control).unwrap();
            let sibling = budget.reserve(37).unwrap();
            let mut visits = Vec::new();
            let output = optimize(input, and, &config(control, simplify), &control, |child| {
                visits.push(match &*child {
                    ScalarExpr::Param(index @ (1 | 2)) => *index,
                    ScalarExpr::Literal(Value::Bool(value)) => 10 + usize::from(*value),
                    _ => return Err(SQLError::Internal("unreachable child".into())),
                });
                Ok(child)
            })
            .unwrap();
            assert_eq!(
                visits,
                [1, 10 + usize::from(and), 2, 10 + usize::from(!and)]
            );
            let expected = if simplify {
                boolean(!and)
            } else {
                node(
                    and,
                    vec![
                        ScalarExpr::Param(1),
                        boolean(and),
                        ScalarExpr::Param(2),
                        boolean(!and),
                        ScalarExpr::Param(99),
                        node(!and, vec![ScalarExpr::Param(98)]),
                        ScalarExpr::Param(97),
                    ],
                )
            };
            assert_eq!(*output, expected);
            assert_eq!(budget.used(), sibling.bytes() + output.reserved_bytes());
            if simplify {
                assert_eq!(output.reserved_bytes(), 0);
            }
            drop((output, sibling));
            assert_eq!(budget.used(), 0);
        }
    }
}

fn truth(expression: &ScalarExpr) -> Option<bool> {
    match expression {
        ScalarExpr::Literal(Value::Bool(value)) => Some(*value),
        ScalarExpr::Literal(Value::Null)
        | ScalarExpr::TypedLiteral {
            value: Value::Null, ..
        } => None,
        ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            let and = matches!(expression, ScalarExpr::And(_));
            let values: Vec<_> = items.iter().map(truth).collect();
            if values.contains(&Some(!and)) {
                Some(!and)
            } else if values.contains(&None) {
                None
            } else {
                Some(and)
            }
        }
        _ => panic!("truth-table fixture"),
    }
}

#[test]
fn boolean_rewriting_preserves_three_valued_results_and_typed_null_ownership() {
    let values = [
        boolean(false),
        boolean(true),
        ScalarExpr::Literal(Value::Null),
    ];
    for and in [false, true] {
        for simplify in [false, true] {
            for a in &values {
                for b in &values {
                    for c in &values {
                        let source =
                            node(and, vec![a.clone(), node(and, vec![b.clone(), c.clone()])]);
                        let expected = truth(&source);
                        let budget = MemoryBudget::new(1 << 20);
                        let token = CancellationToken::new();
                        let control = ProductionControl::new(&budget, &token, &token);
                        let input = source.copy_for_rewrite(&control).unwrap();
                        let output =
                            optimize(input, and, &config(control, simplify), &control, Ok).unwrap();
                        assert_eq!(truth(&output), expected);
                        if simplify {
                            if expected.is_none() {
                                let ScalarExpr::TypedLiteral { ty, .. } = &*output else {
                                    panic!("typed boolean NULL")
                                };
                                assert_eq!(ty, "boolean");
                            }
                            assert_eq!(
                                output.reserved_bytes(),
                                if expected.is_none() { 7 } else { 0 }
                            );
                        }
                        assert_eq!(budget.used(), output.reserved_bytes());
                        drop(output);
                        assert_eq!(budget.used(), 0);
                    }
                }
            }
        }
    }
}

#[test]
fn boolean_flattening_moves_large_payloads_and_freezes_exact_survivors() {
    let source = node(
        true,
        vec![
            ScalarExpr::Column("x".repeat(4 << 20).into()),
            node(
                true,
                vec![
                    boolean(true),
                    ScalarExpr::Column("tail".into()),
                    ScalarExpr::Literal(Value::Null),
                ],
            ),
        ],
    );
    let budget = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let input = source.copy_for_rewrite(&control).unwrap();
    let ScalarExpr::And(items) = &*input else {
        unreachable!()
    };
    let ScalarExpr::Column(name) = &items[0] else {
        unreachable!()
    };
    let pointer = name.as_ptr();
    let sibling = budget.reserve(37).unwrap();
    let before = budget.used();
    let mut result = None;
    let allocated = allocation_counter::measure(|| {
        result = Some(optimize(input, true, &config(control, true), &control, Ok).unwrap());
    });
    let output = result.unwrap();
    assert!(allocated.bytes_total < 16 * 1024, "{allocated:?}");
    assert_eq!(
        allocated.bytes_current,
        i64::try_from(budget.used()).unwrap() - i64::try_from(before).unwrap()
    );
    assert_eq!(budget.used(), sibling.bytes() + output.reserved_bytes());
    let output = output.finish(&control).unwrap();
    let ScalarExpr::And(items) = &*output else {
        panic!("surviving conjunction")
    };
    assert_eq!(items.len(), 3);
    let ScalarExpr::Column(name) = &items[0] else {
        unreachable!()
    };
    assert_eq!(name.as_ptr(), pointer);
    assert_eq!(name.len(), 4 << 20);
    assert_eq!(items[1], ScalarExpr::Column("tail".into()));
    assert!(
        matches!(&items[2], ScalarExpr::TypedLiteral { value: Value::Null, ty, .. } if ty == "boolean")
    );
    assert_eq!(budget.used(), sibling.bytes() + output.reserved_bytes());
    drop((output, sibling));
    assert_eq!(budget.used(), 0);
}

fn deep() -> ScalarExpr {
    let mut result = ScalarExpr::Param(3);
    for _ in 0..16 {
        result = node(
            true,
            vec![ScalarExpr::Param(2), result, ScalarExpr::Param(99)],
        );
    }
    node(true, vec![ScalarExpr::Param(1), result])
}

#[test]
fn boolean_partial_scratch_rejection_releases_prefix_and_unvisited_ancestors() {
    let source = deep();
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let input = source.copy_for_rewrite(&control).unwrap();
    let sibling = budget.reserve(37).unwrap();
    let mut occupied = None;
    let mut visits = 0;
    let result = optimize(input, true, &config(control, true), &control, |child| {
        visits += 1;
        if visits == 2 {
            occupied = Some(budget.reserve(budget.available()).unwrap());
        }
        Ok(child)
    });
    assert_eq!(result.unwrap_err().sqlstate(), Some("53200"));
    assert!((2..10).contains(&visits), "{visits}");
    assert_eq!(
        budget.used(),
        sibling.bytes() + occupied.as_ref().unwrap().bytes()
    );
    drop(occupied);
    let input = source.copy_for_rewrite(&control).unwrap();
    let output = optimize(input, true, &config(control, true), &control, Ok).unwrap();
    let ScalarExpr::And(items) = &*output else {
        unreachable!()
    };
    assert_eq!(items.len(), 34);
    assert_eq!(budget.used(), sibling.bytes() + output.reserved_bytes());
    drop((output, sibling));
    assert_eq!(budget.used(), 0);
}

#[test]
fn boolean_destination_rejection_releases_completed_scratch_and_selected_children() {
    let source = node(true, (1..=4).map(ScalarExpr::Param).collect());
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let input = source.copy_for_rewrite(&control).unwrap();
    let mut occupied = None;
    let mut visits = 0;
    let result = optimize(input, true, &config(control, true), &control, |child| {
        visits += 1;
        if visits == 4 {
            occupied = Some(budget.reserve(budget.available()).unwrap());
        }
        Ok(child)
    });
    assert_eq!(visits, 4);
    assert_eq!(result.unwrap_err().sqlstate(), Some("53200"));
    assert_eq!(budget.used(), occupied.as_ref().unwrap().bytes());
    drop(occupied);
    assert_eq!(budget.used(), 0);
}

#[test]
fn boolean_callback_failure_unwind_and_cancellation_release_every_private_owner() {
    for failure in 0..4 {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let input = deep().copy_for_rewrite(&control).unwrap();
        let sibling = budget.reserve(37).unwrap();
        let mut visits = 0;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            optimize(input, true, &config(control, true), &control, |child| {
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
            })
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

#[test]
fn ordinary_boolean_input_keeps_payloads_while_request_scratch_is_admitted() {
    let source = node(
        true,
        vec![
            ScalarExpr::Column("x".repeat(4 << 20).into()),
            node(true, vec![ScalarExpr::Param(1), ScalarExpr::Param(2)]),
        ],
    );
    let budget = MemoryBudget::new(16 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut result = None;
    let allocated = allocation_counter::measure(|| {
        result = Some(
            optimize(
                OwnedScalarExpr::uncontrolled(source),
                true,
                &config(control, true),
                &ProductionControl::uncontrolled(),
                Ok,
            )
            .unwrap(),
        );
    });
    let output = result.unwrap();
    assert!(allocated.bytes_total < 16 * 1024, "{allocated:?}");
    assert_eq!(output.reserved_bytes(), 0);
    assert_eq!(budget.used(), 0);
    let ScalarExpr::And(items) = &*output else {
        unreachable!()
    };
    assert_eq!(items.len(), 3);
}

#[test]
fn boolean_dispatch_preserves_opposite_nodes_and_callback_created_conjunctions() {
    for and in [false, true] {
        let source = node(
            and,
            vec![
                node(!and, vec![ScalarExpr::Param(99)]),
                ScalarExpr::Param(1),
            ],
        );
        let replacement = node(and, vec![ScalarExpr::Param(2), ScalarExpr::Param(3)]);
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let input = source.copy_for_rewrite(&control).unwrap();
        let mut visits = 0;
        let output = optimize(input, and, &config(control, true), &control, |child| {
            visits += 1;
            if matches!(*child, ScalarExpr::Param(1)) {
                Ok(replacement.copy_for_rewrite(&control)?)
            } else {
                Ok(child)
            }
        })
        .unwrap();
        assert_eq!(visits, 2);
        assert_eq!(
            *output,
            node(
                and,
                vec![node(!and, vec![ScalarExpr::Param(99)]), replacement]
            )
        );
        assert_eq!(budget.used(), output.reserved_bytes());
        drop(output);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn narrow_boolean_flattening_retires_parent_owners_without_heap_scratch() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut input = literal(true, &control).unwrap();
    for _ in 0..128 {
        input = OwnedScalarExpr::boolean(std::iter::once(input), true, &control).unwrap();
    }
    let sibling = budget.reserve(budget.available()).unwrap();
    let mut result = None;
    let allocated = allocation_counter::measure(|| {
        result = Some(optimize(input, true, &config(control, true), &control, Ok).unwrap());
    });
    let output = result.unwrap();
    assert_eq!(*output, boolean(true));
    assert_eq!(allocated.bytes_total, 0);
    assert_eq!(budget.used(), sibling.bytes());
    drop((output, sibling));
    assert_eq!(budget.used(), 0);
}
