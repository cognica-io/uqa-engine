//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::SQLError;

#[test]
fn shared_scalar_edits_preserve_original_analysis_identities_and_replacement_order() {
    let crate::Statement::Select(mut query) = crate::compile("SELECT sum(a + b) FILTER (WHERE c) OVER (PARTITION BY d ORDER BY e ROWS BETWEEN 2 PRECEDING AND 3 FOLLOWING)").unwrap().remove(0) else { panic!("SELECT") };
    let source = crate::plan::ExpressionPlan::lower(query.projections.remove(0).expr).scalar;
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut value = source.copy_for_rewrite(&control).unwrap();
    let mut expected = Vec::new();
    crate::plan::rewrite_scalar_expression(&mut value.expression, &mut |node| {
        expected.push(std::ptr::NonNull::from(node));
    });
    let mut identities = Vec::new();
    value
        .transform_retained_nodes(&control, &mut |identity, value| {
            identities.push(identity);
            Ok(value)
        })
        .unwrap();
    assert_eq!(identities, expected);
    assert_eq!(*value, source);

    let mut original_slots = Vec::new();
    value
        .expression
        .visit(&mut |node| original_slots.push(std::ptr::NonNull::from(node)));
    let mut selected_slots = Vec::new();
    OwnedScalarExpr::replace_nodes_in_place(
        &mut value.expression,
        value.memory.as_mut(),
        &control,
        &mut |node| {
            selected_slots.push(std::ptr::NonNull::from(node));
            Ok(None)
        },
    )
    .unwrap();
    assert_eq!(selected_slots, original_slots);
    drop(value);

    let source = ScalarExpr::Array(vec![text(1), text(2)]);
    let mut expected = source.clone();
    let mut original_order = Vec::new();
    expected.visit_mut(&mut |node| {
        if matches!(node, ScalarExpr::Literal(Value::Str(text)) if text.len() == 1) {
            *node = ScalarExpr::Row(vec![text(3), text(4)]);
        }
        if let ScalarExpr::Literal(Value::Str(text)) = node {
            original_order.push(text.len());
        }
    });
    let mut actual_order = Vec::new();
    let actual = source
        .copy_for_rewrite(&control)
        .unwrap()
        .replace_nodes(&control, &mut |node| {
            if matches!(node, ScalarExpr::Literal(Value::Str(text)) if text.len() == 1) {
                return Ok(Some(
                    ScalarExpr::Row(vec![text(3), text(4)]).copy_for_rewrite(&control)?,
                ));
            }
            if let ScalarExpr::Literal(Value::Str(text)) = node {
                actual_order.push(text.len());
            }
            Ok(None)
        })
        .unwrap();
    assert_eq!(*actual, expected);
    assert_eq!(actual_order, original_order);
    assert_eq!(actual.reserved_bytes(), budget.used());
    drop(actual);
    assert_eq!(budget.used(), 0);
}

#[test]
fn shared_scalar_edits_keep_deep_traversal_and_failure_cleanup_on_small_stack() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let budget = MemoryBudget::new(32 << 20);
            let sibling = budget.reserve(37).unwrap();
            let token = CancellationToken::new();
            let control = ProductionControl::new(&budget, &token, &token);
            for preorder in [false, true] {
                for fail in [false, true] {
                    let value = super::retirement::deep(&control);
                    let mut count = 0;
                    let mut callback = || {
                        count += 1;
                        if fail && count == 1024 {
                            Err(SQLError::Internal("selected edit failed".into()))
                        } else {
                            Ok(())
                        }
                    };
                    let result = if preorder {
                        value.replace_nodes(&control, &mut |_| {
                            callback()?;
                            Ok(None)
                        })
                    } else {
                        value.transform_nodes(&control, &mut |_, value| {
                            callback()?;
                            Ok(value)
                        })
                    };
                    assert_eq!(result.is_err(), fail);
                    assert_eq!(count, if fail { 1024 } else { 8193 });
                    let released = allocation_counter::measure(|| drop(result));
                    assert_eq!(released.count_total, 0);
                    assert_eq!(budget.used(), 37);
                }
            }
            drop(sibling);
        })
        .unwrap()
        .join()
        .unwrap();
}
