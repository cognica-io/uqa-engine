//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::FunctionOrderSyntax, ir::ScalarOrder};

fn case(else_branch: Option<ScalarExpr>) -> ScalarExpr {
    ScalarExpr::Case {
        base: Some(Box::new(text(31))),
        when: vec![
            (text(101), text(103)),
            (text(211), text(223)),
            (text(307), text(311)),
        ],
        else_branch: else_branch.map(Box::new),
    }
}

fn call() -> ScalarExpr {
    ScalarExpr::Func {
        order_syntax: FunctionOrderSyntax::Ordinary,
        name: "coalesce".into(),
        binding: None,
        args: vec![text(101), text(211), text(307)],
        distinct: false,
        order_by: vec![
            ScalarOrder {
                expr: text(409),
                descending: false,
                nulls: None,
            },
            ScalarOrder {
                expr: text(503),
                descending: true,
                nulls: None,
            },
        ],
        filter: Some(Box::new(text(4096))),
    }
}

#[test]
fn swapping_every_scalar_shape_moves_exact_child_owners_without_allocation() {
    for (source, children) in fixtures::child_shapes() {
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let output = source.copy_for_rewrite(&control).unwrap();
        let before = budget.used();
        let mut swapped = None;
        let measured = allocation_counter::measure(|| {
            swapped = Some(
                output
                    .swap_children(0, children.len() - 1, &control)
                    .unwrap(),
            );
        });
        assert_eq!(measured.count_total, 0);
        assert_eq!(measured.bytes_current, 0);
        assert_eq!(budget.used(), before);
        let selected = swapped.unwrap().select_child(0);
        assert_eq!(&*selected, children.last().unwrap());
        assert_eq!(selected.reserved_bytes(), budget.used());
        let reference_budget = MemoryBudget::new(1 << 20);
        let expected = children
            .last()
            .unwrap()
            .copy_for_rewrite(&ProductionControl::new(&reference_budget, &token, &token))
            .unwrap();
        assert_eq!(selected.reserved_bytes(), expected.reserved_bytes());
        drop((selected, expected));
        assert_eq!(budget.used(), 0);
        assert_eq!(reference_budget.used(), 0);
    }
}

#[test]
fn case_truncation_releases_discarded_values_and_keeps_base_and_else_owners() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let output = case(Some(text(4096))).copy_for_rewrite(&control).unwrap();
    let ScalarExpr::Case {
        base,
        when,
        else_branch,
    } = &*output
    else {
        unreachable!()
    };
    let base_buffer = std::ptr::from_ref(base.as_deref().unwrap());
    let arms_buffer = when.as_ptr();
    let else_buffer = std::ptr::from_ref(else_branch.as_deref().unwrap());
    let before = budget.used();
    let mut truncated = None;
    let measured = allocation_counter::measure(|| {
        truncated = Some(output.truncate_case_arms(1, &control).unwrap());
    });
    let output = truncated.unwrap();
    assert_eq!(measured.count_total, 0);
    assert_eq!(before - budget.used(), 211 + 223 + 307 + 311);
    assert_eq!(
        measured.bytes_current,
        -i64::try_from(before - budget.used()).unwrap()
    );
    assert_eq!(output.reserved_bytes(), budget.used());
    let ScalarExpr::Case {
        base,
        when,
        else_branch,
    } = &*output
    else {
        unreachable!()
    };
    assert_eq!(std::ptr::from_ref(base.as_deref().unwrap()), base_buffer);
    assert_eq!(when.as_ptr(), arms_buffer);
    assert_eq!(when, &[(text(101), text(103))]);
    assert_eq!(
        std::ptr::from_ref(else_branch.as_deref().unwrap()),
        else_buffer
    );
    assert_eq!(output.child_count(), 4);
    let selected = output
        .truncate_case_arms(usize::MAX, &control)
        .unwrap()
        .truncate_case_arms(0, &control)
        .unwrap()
        .select_child(1);
    assert_eq!(*selected, text(4096));
    assert_eq!(budget.used(), 4096);
    drop(selected);
    assert_eq!(budget.used(), 0);
}

#[test]
fn call_argument_truncation_preserves_ordering_and_filter_owners() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let output = call().copy_for_rewrite(&control).unwrap();
    let ScalarExpr::Func {
        args,
        order_by,
        filter,
        ..
    } = &*output
    else {
        unreachable!()
    };
    let args_buffer = args.as_ptr();
    let order_buffer = order_by.as_ptr();
    let filter_buffer = std::ptr::from_ref(filter.as_deref().unwrap());
    let before = budget.used();
    let mut truncated = None;
    let measured = allocation_counter::measure(|| {
        truncated = Some(output.truncate_call_arguments(1, &control).unwrap());
    });
    let output = truncated.unwrap();
    assert_eq!(measured.count_total, 0);
    assert_eq!(before - budget.used(), 211 + 307);
    assert_eq!(
        measured.bytes_current,
        -i64::try_from(before - budget.used()).unwrap()
    );
    let ScalarExpr::Func {
        args,
        order_by,
        filter,
        ..
    } = &*output
    else {
        unreachable!()
    };
    assert_eq!(args.as_ptr(), args_buffer);
    assert_eq!(order_by.as_ptr(), order_buffer);
    assert_eq!(
        std::ptr::from_ref(filter.as_deref().unwrap()),
        filter_buffer
    );
    assert_eq!(args, &[text(101)]);
    assert_eq!(output.child_count(), 4);
    let selected = output
        .truncate_call_arguments(0, &control)
        .unwrap()
        .select_child(2);
    assert_eq!(*selected, text(4096));
    assert_eq!(budget.used(), 4096);
    drop(selected);
    assert_eq!(budget.used(), 0);
}

#[test]
fn boolean_permutation_and_truncation_preserve_surviving_payload_owners() {
    for conjunction in [false, true] {
        let items = vec![text(101), text(211), text(307), text(4096)];
        let source = if conjunction {
            ScalarExpr::And(items)
        } else {
            ScalarExpr::Or(items)
        };
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let output = source.copy_for_rewrite(&control).unwrap();
        let (ScalarExpr::And(items) | ScalarExpr::Or(items)) = &*output else {
            unreachable!()
        };
        let buffer = items.as_ptr();
        let before = budget.used();
        let mut reordered = None;
        let measured = allocation_counter::measure(|| {
            reordered = Some(
                output
                    .swap_children(0, 3, &control)
                    .unwrap()
                    .swap_children(1, 2, &control)
                    .unwrap()
                    .truncate_boolean_children(2, &control)
                    .unwrap(),
            );
        });
        let output = reordered.unwrap();
        let (ScalarExpr::And(items) | ScalarExpr::Or(items)) = &*output else {
            unreachable!()
        };
        assert_eq!(items.as_ptr(), buffer);
        assert_eq!(items, &[text(4096), text(307)]);
        assert_eq!(measured.count_total, 0);
        assert_eq!(before - budget.used(), 101 + 211);
        assert_eq!(
            measured.bytes_current,
            -i64::try_from(before - budget.used()).unwrap()
        );
        let selected = output.select_child(1);
        assert_eq!(*selected, text(307));
        assert_eq!(budget.used(), 307);
        drop(selected);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn replacing_case_else_reuses_its_box_and_releases_previous_payload_immediately() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let output = case(Some(text(8192))).copy_for_rewrite(&control).unwrap();
    let replacement = text(257).copy_for_rewrite(&control).unwrap();
    let ScalarExpr::Case { else_branch, .. } = &*output else {
        unreachable!()
    };
    let buffer = std::ptr::from_ref(else_branch.as_deref().unwrap());
    let before = budget.used();
    let mut replaced = None;
    let measured = allocation_counter::measure(|| {
        replaced = Some(output.set_case_else(replacement, &control).unwrap());
    });
    let output = replaced.unwrap();
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_current, -8192);
    assert_eq!(before - budget.used(), 8192);
    assert_eq!(output.reserved_bytes(), budget.used());
    let ScalarExpr::Case { else_branch, .. } = &*output else {
        unreachable!()
    };
    assert_eq!(std::ptr::from_ref(else_branch.as_deref().unwrap()), buffer);
    let index = output.child_count() - 1;
    let selected = output.select_child(index);
    assert_eq!(*selected, text(257));
    assert_eq!(budget.used(), 257);
    drop(selected);
    assert_eq!(budget.used(), 0);
}

#[test]
fn adding_case_else_admits_box_and_metadata_and_freezes_exact_surviving_storage() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let output = case(None).copy_for_rewrite(&control).unwrap();
    let replacement = text(257).copy_for_rewrite(&control).unwrap();
    let before = budget.used();
    let mut replaced = None;
    let measured = allocation_counter::measure(|| {
        replaced = Some(output.set_case_else(replacement, &control).unwrap());
    });
    let output = replaced.unwrap();
    assert!(budget.used() - before >= size_of::<ScalarExpr>());
    assert_eq!(
        measured.bytes_current,
        i64::try_from(budget.used() - before).unwrap()
    );
    assert_eq!(output.reserved_bytes(), budget.used());
    let frozen = output.finish(&control).unwrap();
    assert_eq!(*frozen, case(Some(text(257))));
    let reference = case(Some(text(257))).clone_with_control(&control).unwrap();
    assert_eq!(frozen.reserved_bytes(), reference.reserved_bytes());
    drop((frozen, reference));
    assert_eq!(budget.used(), 0);
}

#[test]
fn case_else_rejects_before_either_new_metadata_or_box_allocation() {
    for reject_metadata in [true, false] {
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let source = if reject_metadata {
            ScalarExpr::Case {
                base: None,
                when: Vec::new(),
                else_branch: None,
            }
        } else {
            case(None)
        };
        let output = source.copy_for_rewrite(&control).unwrap();
        let output = output.truncate_case_arms(0, &control).unwrap();
        if !reject_metadata {
            let memory = output.memory.as_ref().unwrap();
            assert!(memory.children.capacity() > memory.children.len());
        }
        let replacement = text(257).copy_for_rewrite(&control).unwrap();
        let available = if reject_metadata {
            0
        } else {
            size_of::<ScalarExpr>() - 1
        };
        let sibling = budget.reserve(budget.available() - available).unwrap();
        let mut result = None;
        let measured = allocation_counter::measure(|| {
            result = Some(output.set_case_else(replacement, &control));
        });
        let error: crate::SQLError = result.unwrap().unwrap_err().into();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!(measured.count_total, 0);
        assert_eq!(budget.used(), sibling.bytes());
        drop(sibling);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn structural_edits_honor_both_cancellation_scopes_and_drop_private_inputs() {
    for original_owner in [true, false] {
        for operation in 0..5 {
            let budget = MemoryBudget::new(1 << 20);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&budget, &original, &invoking);
            let source = match operation {
                1 => call(),
                3 => ScalarExpr::And(vec![text(1024), text(2048)]),
                _ => case(None),
            };
            let output = source.copy_for_rewrite(&control).unwrap();
            let mut replacement = Some(text(257).copy_for_rewrite(&control).unwrap());
            let sibling = budget.reserve(37).unwrap();
            if original_owner {
                original.cancel();
            } else {
                invoking.cancel();
            }
            let result = match operation {
                0 => output.swap_children(0, 1, &control),
                1 => output.truncate_call_arguments(1, &control),
                2 => output.truncate_case_arms(1, &control),
                3 => output.truncate_boolean_children(1, &control),
                _ => output.set_case_else(replacement.take().unwrap(), &control),
            };
            let error: crate::SQLError = result.unwrap_err().into();
            assert_eq!(error.sqlstate(), Some("57014"));
            // Other operations do not consume the separately prepared fallback.
            assert_eq!(
                budget.used(),
                sibling.bytes() + if operation == 4 { 0 } else { 257 }
            );
            drop((sibling, replacement));
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn ordinary_structural_edits_preserve_explicit_uncontrolled_ownership() {
    let control = ProductionControl::uncontrolled();
    let output = OwnedScalarExpr::uncontrolled(case(None))
        .swap_children(1, 3, &control)
        .unwrap()
        .swap_children(2, 4, &control)
        .unwrap()
        .truncate_case_arms(1, &control)
        .unwrap()
        .set_case_else(OwnedScalarExpr::uncontrolled(text(4096)), &control)
        .unwrap();
    assert_eq!(output.reserved_bytes(), 0);
    assert_eq!(output.child_count(), 4);
    let expected = ScalarExpr::Case {
        base: Some(Box::new(text(31))),
        when: vec![(text(211), text(223))],
        else_branch: Some(Box::new(text(4096))),
    };
    assert_eq!(*output.finish(&control).unwrap(), expected);
    let output = OwnedScalarExpr::uncontrolled(call())
        .truncate_call_arguments(0, &control)
        .unwrap()
        .select_child(2);
    assert_eq!(*output, text(4096));
    assert_eq!(output.reserved_bytes(), 0);
}

#[test]
fn case_fallback_rejects_foreign_or_uncontrolled_replacements_without_leaking() {
    for ordinary in [false, true] {
        let parent = MemoryBudget::new(1 << 20);
        let foreign = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&parent, &token, &token);
        let output = case(Some(text(4096))).copy_for_rewrite(&control).unwrap();
        let replacement = if ordinary {
            OwnedScalarExpr::uncontrolled(text(1024))
        } else {
            text(1024)
                .copy_for_rewrite(&ProductionControl::new(&foreign, &token, &token))
                .unwrap()
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            output.set_case_else(replacement, &control)
        }));
        assert!(result.is_err());
        assert_eq!(parent.used(), 0);
        assert_eq!(foreign.used(), 0);
    }
}

#[test]
fn child_replacement_returns_both_exact_owners_without_allocating_capacity() {
    for (source, children) in fixtures::child_shapes() {
        let budget = MemoryBudget::new(1 << 20);
        let reference_budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let index = children.len() / 2;
        let expected = children[index]
            .copy_for_rewrite(&ProductionControl::new(&reference_budget, &token, &token))
            .unwrap();
        let input = source.copy_for_rewrite(&control).unwrap();
        let replacement = text(257).copy_for_rewrite(&control).unwrap();
        let sibling = budget.reserve(budget.available()).unwrap();
        let before = budget.used();
        let mut result = None;
        let measured = allocation_counter::measure(|| {
            result = Some(input.replace_child(index, replacement, &control).unwrap());
        });
        let (parent, previous) = result.unwrap();
        assert_eq!(measured.count_total, 0);
        assert_eq!(measured.bytes_current, 0);
        assert_eq!(budget.used(), before);
        assert_eq!(*previous, children[index]);
        assert_eq!(previous.reserved_bytes(), expected.reserved_bytes());
        assert_eq!(
            parent.reserved_bytes() + previous.reserved_bytes() + sibling.bytes(),
            before
        );
        let selected = parent.select_child(index);
        assert_eq!(*selected, text(257));
        assert_eq!(
            budget.used(),
            sibling.bytes() + 257 + previous.reserved_bytes()
        );
        drop(previous);
        assert_eq!(budget.used(), sibling.bytes() + 257);
        drop((selected, sibling, expected));
        assert_eq!(budget.used(), 0);
        assert_eq!(reference_budget.used(), 0);
    }
}

#[test]
fn child_replacement_rejects_cancellation_and_mixed_ownership_without_leaking() {
    for mode in 0..4 {
        let budget = MemoryBudget::new(1 << 20);
        let foreign = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let input = case(Some(text(8192))).copy_for_rewrite(&control).unwrap();
        let replacement = match mode {
            2 => text(1024)
                .copy_for_rewrite(&ProductionControl::new(&foreign, &original, &invoking))
                .unwrap(),
            3 => OwnedScalarExpr::uncontrolled(text(1024)),
            _ => text(1024).copy_for_rewrite(&control).unwrap(),
        };
        let sibling = budget.reserve(37).unwrap();
        let foreign_sibling = foreign.reserve(17).unwrap();
        if mode == 0 {
            original.cancel();
        }
        if mode == 1 {
            invoking.cancel();
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            input.replace_child(0, replacement, &control)
        }));
        if mode < 2 {
            let error: crate::SQLError = result.unwrap().unwrap_err().into();
            assert_eq!(error.sqlstate(), Some("57014"));
        } else {
            assert!(result.is_err());
        }
        assert_eq!(budget.used(), sibling.bytes());
        assert_eq!(foreign.used(), foreign_sibling.bytes());
        drop((sibling, foreign_sibling));
        assert_eq!(budget.used(), 0);
        assert_eq!(foreign.used(), 0);
    }
}
