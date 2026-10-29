//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

mod fixtures;

fn text(length: usize) -> ScalarExpr {
    ScalarExpr::Literal(Value::Str("x".repeat(length)))
}

fn nested() -> ScalarExpr {
    ScalarExpr::Row(vec![
        text(4096),
        ScalarExpr::Not(Box::new(ScalarExpr::Array(vec![text(257), text(8192)]))),
        text(16384),
    ])
}

#[test]
fn mutable_copy_charges_exact_live_allocations_and_freezing_releases_only_metadata() {
    let source = nested();
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut output = None;
    let allocated = allocation_counter::measure(|| {
        output = Some(source.copy_for_rewrite(&control).unwrap());
    });
    let output = output.unwrap();
    assert_eq!(*output, source);
    assert_eq!(output.reserved_bytes(), budget.used());
    assert_eq!(
        allocated.bytes_current,
        i64::try_from(budget.used()).unwrap()
    );
    let before = budget.used();
    let mut frozen = None;
    let released = allocation_counter::measure(|| {
        frozen = Some(output.finish(&control).unwrap());
    });
    let frozen = frozen.unwrap();
    assert_eq!(released.count_total, 0);
    assert!(budget.used() < before);
    assert_eq!(
        released.bytes_current,
        -i64::try_from(before - budget.used()).unwrap()
    );
    assert_eq!(*frozen, source);
    let immutable = source.clone_with_control(&control).unwrap();
    assert_eq!(immutable.reserved_bytes(), frozen.reserved_bytes());
    drop(immutable);
    drop(frozen);
    assert_eq!(budget.used(), 0);
}

#[test]
fn selecting_nested_children_frees_parent_boxes_vectors_and_discarded_payloads() {
    let source = nested();
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let output = source.copy_for_rewrite(&control).unwrap();
    let before = budget.used();
    let mut selected = None;
    let released = allocation_counter::measure(|| {
        selected = Some(output.select_child(1).select_child(0).select_child(0));
    });
    let selected = selected.unwrap();
    assert_eq!(*selected, text(257));
    assert_eq!(selected.reserved_bytes(), 257);
    assert_eq!(budget.used(), 257);
    assert_eq!(released.count_total, 0);
    assert_eq!(
        released.bytes_current,
        -i64::try_from(before - 257).unwrap()
    );
    drop(selected);
    assert_eq!(budget.used(), 0);
    assert_eq!(source, nested());
}

#[test]
fn child_rewrites_preserve_container_addresses_and_return_discarded_allowances_immediately() {
    let source = nested();
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let output = source.copy_for_rewrite(&control).unwrap();
    let ScalarExpr::Row(items) = &*output else {
        panic!("row")
    };
    let buffer = items.as_ptr();
    let capacity = items.capacity();
    let ScalarExpr::Not(inner) = &items[1] else {
        panic!("not")
    };
    let boxed = std::ptr::from_ref(inner.as_ref());
    let before = budget.used();
    let mut rewritten = None;
    let measured = allocation_counter::measure(|| {
        rewritten = Some(
            output
                .map_child::<CatalogRetentionError>(1, &control, |child| {
                    child.map_child(0, &control, |array| Ok(array.select_child(0)))
                })
                .unwrap(),
        );
    });
    let output = rewritten.unwrap();
    let ScalarExpr::Row(items) = &*output else {
        panic!("row")
    };
    assert_eq!(items.as_ptr(), buffer);
    assert_eq!(items.capacity(), capacity);
    let ScalarExpr::Not(inner) = &items[1] else {
        panic!("not")
    };
    assert_eq!(std::ptr::from_ref(inner.as_ref()), boxed);
    assert_eq!(inner.as_ref(), &text(257));
    assert_eq!(measured.count_total, 0);
    assert_eq!(
        measured.bytes_current,
        -i64::try_from(before - budget.used()).unwrap()
    );
    assert_eq!(output.reserved_bytes(), budget.used());
    assert!(before - budget.used() > 8192);
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn replacement_constructor_can_reuse_the_discarded_child_allowance() {
    let source = ScalarExpr::Row(vec![text(8192), text(1024)]);
    let token = CancellationToken::new();
    let baseline = MemoryBudget::new(1 << 20);
    let initial = source
        .copy_for_rewrite(&ProductionControl::new(&baseline, &token, &token))
        .unwrap();
    let required = initial.reserved_bytes();
    drop(initial);
    let budget = MemoryBudget::new(required);
    let control = ProductionControl::new(&budget, &token, &token);
    let output = source.copy_for_rewrite(&control).unwrap();
    assert!(control.copy_value(&Value::Str("y".repeat(8192))).is_err());
    let replacement = Value::Str("z".repeat(8192));
    let output = output
        .map_child::<CatalogRetentionError>(0, &control, |child| {
            drop(child);
            OwnedScalarExpr::literal(control.copy_value(&replacement)?, &control)
        })
        .unwrap();
    assert_eq!(budget.used(), required);
    let ScalarExpr::Row(items) = &*output else {
        panic!("row")
    };
    assert_eq!(items[0], ScalarExpr::Literal(replacement));
    assert_eq!(items[1], text(1024));
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn failed_copy_rejects_large_payload_before_allocation_and_releases_partial_siblings() {
    let source = ScalarExpr::Row(vec![text(97), text(4 << 20)]);
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let measured = allocation_counter::measure(|| {
        assert!(matches!(
            source.copy_for_rewrite(&control),
            Err(CatalogRetentionError::Memory(_))
        ));
    });
    assert!(measured.bytes_max <= 4096, "{measured:?}");
    assert_eq!(measured.bytes_current, 0);
    assert_eq!(budget.used(), 0);
    assert_eq!(source, ScalarExpr::Row(vec![text(97), text(4 << 20)]));
}

#[test]
fn failed_replacement_drops_visited_and_unvisited_children_without_mutating_borrowed_source() {
    let source = nested();
    let budget = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let oversized = Value::Str("x".repeat(1 << 20));
    let output = source.copy_for_rewrite(&control).unwrap();
    let mut visited = 0;
    let result = output.map_children::<CatalogRetentionError>(&control, |index, child| {
        visited += 1;
        if index == 1 {
            drop(child);
            OwnedScalarExpr::literal(control.copy_value(&oversized)?, &control)
        } else {
            Ok(child)
        }
    });
    assert!(matches!(result, Err(CatalogRetentionError::Memory(_))));
    assert_eq!(visited, 2);
    assert_eq!(budget.used(), 0);
    assert_eq!(source, nested());
}

#[test]
fn both_cancellation_scopes_stop_before_copy_and_after_child_callback() {
    let source = nested();
    for cancel_original in [false, true] {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let output = source.copy_for_rewrite(&control).unwrap();
        let token = if cancel_original {
            &original
        } else {
            &invoking
        };
        let mut visited = 0;
        let result = output.map_children::<CatalogRetentionError>(&control, |_, child| {
            visited += 1;
            token.cancel();
            Ok(child)
        });
        assert!(matches!(result, Err(CatalogRetentionError::Cancelled(_))));
        assert_eq!(visited, 1);
        assert_eq!(budget.used(), 0);
        let measured = allocation_counter::measure(|| {
            assert!(matches!(
                source.copy_for_rewrite(&control),
                Err(CatalogRetentionError::Cancelled(_))
            ));
        });
        assert_eq!(measured.count_total, 0);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn callback_error_and_unwind_destroy_the_complete_private_rewrite() {
    let source = nested();
    for unwind in [false, true] {
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let output = source.copy_for_rewrite(&control).unwrap();
        let mut visited = 0;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            output.map_children::<CatalogRetentionError>(&control, |index, child| {
                visited += 1;
                if index == 1 {
                    assert!(!unwind, "rewrite callback");
                    return Err(CatalogRetentionError::UnexpectedSubquery);
                }
                Ok(child)
            })
        }));
        if unwind {
            assert!(result.is_err());
        } else {
            assert!(result.unwrap().is_err());
        }
        assert_eq!(visited, 2);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn selected_child_keeps_ancestor_allowance_and_blocks_sibling_until_final_drop() {
    let parent = MemoryBudget::new(65536);
    let child = parent.child(65536);
    let sibling = parent.child(65536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&child, &token, &token);
    let output = nested().copy_for_rewrite(&control).unwrap().select_child(2);
    assert_eq!(child.used(), 16384);
    assert_eq!(parent.used(), 16384);
    let occupied = sibling.reserve(65536 - 16384).unwrap();
    assert!(sibling.reserve(1).is_err());
    drop(output);
    assert_eq!(child.used(), 0);
    let available = sibling.reserve(16384).unwrap();
    drop(occupied);
    drop(available);
    assert_eq!(parent.used(), 0);
}

#[test]
fn ordinary_child_rewrites_move_payloads_and_freezing_keeps_the_explicit_ordinary_mode() {
    let source = nested();
    let control = ProductionControl::uncontrolled();
    let owned = source.copy_for_rewrite(&control).unwrap();
    assert_eq!(owned.reserved_bytes(), 0);
    let output = owned
        .map_child::<CatalogRetentionError>(1, &control, |child| {
            Ok(child.select_child(0).select_child(1))
        })
        .unwrap()
        .select_child(1)
        .finish(&control)
        .unwrap()
        .into_uncontrolled()
        .unwrap();
    assert_eq!(output, text(8192));
}

#[test]
fn mismatched_replacement_ownership_is_rejected_without_leaking_either_allowance() {
    for ordinary in [false, true] {
        let budget = MemoryBudget::new(65536);
        let foreign = MemoryBudget::new(65536);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let other = if ordinary {
            ProductionControl::uncontrolled()
        } else {
            ProductionControl::new(&foreign, &token, &token)
        };
        let output = nested().copy_for_rewrite(&control).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            output.map_child::<CatalogRetentionError>(0, &control, |child| {
                drop(child);
                text(4096).copy_for_rewrite(&other)
            })
        }));
        assert!(result.is_err());
        assert_eq!(budget.used(), 0);
        assert_eq!(foreign.used(), 0);
    }
}

#[test]
fn every_scalar_child_shape_transfers_the_matching_payload_and_retains_semantics() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (source, expected) in fixtures::child_shapes() {
        let copied = source.copy_for_rewrite(&control).unwrap();
        assert_eq!(*copied, source);
        assert_eq!(copied.child_count(), expected.len());
        let mut ordinal = 0;
        let copied = copied
            .map_children::<CatalogRetentionError>(&control, |index, child| {
                assert_eq!(index, ordinal);
                assert_eq!(*child, expected[index]);
                let ScalarExpr::Literal(Value::Str(value)) = &*child else {
                    panic!("unique child payload")
                };
                assert_eq!(child.reserved_bytes(), value.capacity());
                ordinal += 1;
                Ok(child)
            })
            .unwrap();
        assert_eq!(ordinal, expected.len());
        assert_eq!(*copied.finish(&control).unwrap(), source);
        assert_eq!(budget.used(), 0);
        for (index, expected) in expected.iter().enumerate() {
            let child = source
                .copy_for_rewrite(&control)
                .unwrap()
                .select_child(index);
            assert_eq!(&*child, expected);
            let ScalarExpr::Literal(Value::Str(value)) = &*child else {
                panic!("unique child payload")
            };
            assert_eq!(budget.used(), value.capacity());
            drop(child);
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn copied_shared_literal_keeps_its_original_owner_until_the_final_reader() {
    use crate::ir::{RetainedConstantValue, RetainedScalarLiteral};
    let original = MemoryBudget::new(65536);
    let rewrite = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&original, &token, &token);
    let value = RetainedConstantValue::from_produced(
        control.copy_value(&Value::Str("x".repeat(8192))).unwrap(),
        &control,
    )
    .unwrap();
    let literal = RetainedScalarLiteral::with_control(value, None, None, None, &control).unwrap();
    let source = ScalarExpr::Array(vec![ScalarExpr::RetainedLiteral(literal)]);
    let retained = original.used();
    let control = ProductionControl::new(&rewrite, &token, &token);
    let first = source.copy_for_rewrite(&control).unwrap().select_child(0);
    let last = source.copy_for_rewrite(&control).unwrap().select_child(0);
    assert_eq!(rewrite.used(), 0);
    assert_eq!(original.used(), retained);
    drop(source);
    drop(first);
    assert_eq!(original.used(), retained);
    drop(last);
    assert_eq!(original.used(), 0);
}

#[test]
fn child_iterator_releases_parent_on_exhaustion_and_unvisited_children_on_drop() {
    let source = ScalarExpr::Row(vec![text(101), text(203), text(307)]);
    let budget = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for stop_early in [false, true] {
        let output = source.copy_for_rewrite(&control).unwrap();
        let retained = budget.used();
        let mut children = output.into_children();
        assert_eq!(children.len(), 3);
        let first = children.next().unwrap();
        assert_eq!(*first, text(101));
        assert_eq!(children.len(), 2);
        assert_eq!(budget.used(), retained);
        if stop_early {
            drop(children);
            assert_eq!(budget.used(), 101);
            drop(first);
        } else {
            drop(first);
            assert_eq!(budget.used(), retained - 101);
            let second = children.next().unwrap();
            let last = children.next().unwrap();
            assert_eq!(children.len(), 0);
            assert!(children.next().is_none());
            assert_eq!(*second, text(203));
            assert_eq!(*last, text(307));
            assert_eq!(budget.used(), 203 + 307);
            drop(second);
            drop(last);
            assert_eq!(budget.used(), 0);
            drop(children);
        }
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn rebuilding_a_boolean_moves_children_and_admits_only_new_containers() {
    let source = ScalarExpr::Row(vec![text(101), text(203), text(307)]);
    for controlled in [false, true] {
        let budget = MemoryBudget::new(65536);
        let token = CancellationToken::new();
        let control = if controlled {
            ProductionControl::new(&budget, &token, &token)
        } else {
            ProductionControl::uncontrolled()
        };
        for conjunction in [false, true] {
            let input = source.copy_for_rewrite(&control).unwrap();
            let ScalarExpr::Row(items) = &*input else {
                panic!("row")
            };
            let payloads = items
                .iter()
                .map(|item| {
                    let ScalarExpr::Literal(Value::Str(text)) = item else {
                        panic!("text")
                    };
                    text.as_ptr()
                })
                .collect::<Vec<_>>();
            let output =
                OwnedScalarExpr::boolean(input.into_children(), conjunction, &control).unwrap();
            let items = match &*output {
                ScalarExpr::And(items) if conjunction => items,
                ScalarExpr::Or(items) if !conjunction => items,
                _ => panic!("selected boolean operator"),
            };
            for (item, pointer) in items.iter().zip(&payloads) {
                let ScalarExpr::Literal(Value::Str(text)) = item else {
                    panic!("text")
                };
                assert_eq!(text.as_ptr(), *pointer);
            }
            assert_eq!(output.reserved_bytes(), budget.used());
            let output = output.finish(&control).unwrap();
            if controlled {
                assert_eq!(
                    budget.used(),
                    items_capacity(&output) * size_of::<ScalarExpr>() + 101 + 203 + 307
                );
            }
            drop(output);
            assert_eq!(budget.used(), 0);
        }
    }
}

fn items_capacity(expression: &ScalarExpr) -> usize {
    match expression {
        ScalarExpr::And(items) | ScalarExpr::Or(items) => items.capacity(),
        _ => panic!("boolean container"),
    }
}

#[test]
fn boolean_destination_rejection_releases_consumed_and_unvisited_input() {
    let source = ScalarExpr::Row(vec![text(4096), text(8192)]);
    let budget = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let input = source.copy_for_rewrite(&control).unwrap();
    let occupied = budget.reserve(budget.available()).unwrap();
    let measured = allocation_counter::measure(|| {
        assert!(matches!(
            OwnedScalarExpr::boolean(input.into_children(), true, &control),
            Err(CatalogRetentionError::Memory(_))
        ));
    });
    assert_eq!(measured.count_total, 0);
    assert_eq!(budget.used(), occupied.bytes());
    drop(occupied);
    assert_eq!(budget.used(), 0);
}

#[test]
fn cancelled_boolean_iteration_releases_partial_output_and_remaining_input() {
    let source = ScalarExpr::Row(vec![text(101), text(203), text(307)]);
    for cancel_original in [false, true] {
        let budget = MemoryBudget::new(65536);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let input = source.copy_for_rewrite(&control).unwrap();
        let mut visited = 0;
        let children = input.into_children().inspect(|_| {
            visited += 1;
            if visited == 2 {
                if cancel_original {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
            }
        });
        assert!(matches!(
            OwnedScalarExpr::boolean(children, true, &control),
            Err(CatalogRetentionError::Cancelled(_))
        ));
        assert_eq!(visited, 2);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn every_scalar_shape_moves_its_children_without_copying_or_retaining_parent_storage() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (source, expected) in fixtures::child_shapes() {
        let input = source.copy_for_rewrite(&control).unwrap();
        let before = budget.used();
        let measured = allocation_counter::measure(|| {
            let mut children = input.into_children();
            for (index, expected_child) in expected.iter().enumerate() {
                assert_eq!(children.len(), expected.len().saturating_sub(index));
                let child = children.next().unwrap();
                assert_eq!(&*child, expected_child);
            }
            assert!(children.next().is_none());
            assert_eq!(budget.used(), 0);
        });
        assert_eq!(measured.count_total, 0);
        assert_eq!(measured.bytes_current, -i64::try_from(before).unwrap());
    }
}

#[test]
fn ownership_metadata_rejection_drops_partially_constructed_boolean() {
    let source = ScalarExpr::Row(vec![text(101), text(203), text(307)]);
    let budget = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let input = source.copy_for_rewrite(&control).unwrap();
    let destination = 3 * size_of::<ScalarExpr>() + size_of::<Option<ScalarMemory>>();
    let occupied = budget.reserve(budget.available() - destination).unwrap();
    let mut visited = 0;
    let children = input.into_children().inspect(|_| visited += 1);
    assert!(matches!(
        OwnedScalarExpr::boolean(children, true, &control),
        Err(CatalogRetentionError::Memory(_))
    ));
    assert_eq!(visited, 2);
    assert_eq!(budget.used(), occupied.bytes());
    drop(occupied);
    assert_eq!(budget.used(), 0);
}

#[test]
fn iterating_a_leaf_releases_it_immediately_and_empty_boolean_needs_no_headroom() {
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let leaf = text(4096).copy_for_rewrite(&control).unwrap();
    assert_eq!(budget.used(), 4096);
    let mut children = leaf.into_children();
    assert_eq!(budget.used(), 0);
    assert_eq!(children.len(), 0);
    assert!(children.next().is_none());
    let occupied = budget.reserve(4096).unwrap();
    let boolean = OwnedScalarExpr::boolean(children, true, &control).unwrap();
    assert_eq!(*boolean, ScalarExpr::And(Vec::new()));
    assert_eq!(boolean.reserved_bytes(), 0);
    drop(boolean);
    drop(occupied);
    assert_eq!(budget.used(), 0);
}

#[test]
fn wide_call_and_window_slots_preserve_order_without_rewrite_workspace() {
    let budget = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (source, count) in fixtures::wide_keyed_calls() {
        let owned = source.copy_for_rewrite(&control).unwrap();
        let retained = budget.used();
        assert_eq!(owned.child_count(), count);
        let mut output = None;
        let mut visited = 0;
        let measured = allocation_counter::measure(|| {
            output = Some(
                owned
                    .map_children::<CatalogRetentionError>(&control, |index, child| {
                        assert_eq!(index, visited);
                        assert_eq!(*child, ScalarExpr::Param(index));
                        assert_eq!(child.reserved_bytes(), 0);
                        visited += 1;
                        Ok(child)
                    })
                    .unwrap(),
            );
        });
        assert_eq!(visited, count);
        assert_eq!(measured.count_total, 0);
        assert_eq!(measured.bytes_current, 0);
        assert_eq!(budget.used(), retained);
        let output = output.unwrap();
        assert_eq!(*output, source);
        let selected = output.select_child(count - 1);
        assert_eq!(*selected, ScalarExpr::Param(count - 1));
        assert_eq!(budget.used(), 0);
    }
}
