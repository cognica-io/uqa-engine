//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Move the first threshold call for each field, admitting only the merge's temporary index.

use super::{ScalarExpr, Value};
use uqa_core::memory::{ProductionControl, ProductionMap, ProductionVec};
use uqa_sql::{plan::OwnedScalarExpr, SQLError};

#[derive(Clone, Copy)]
struct Selection {
    first: usize,
    threshold: f64,
}

const REMOVED: usize = usize::MAX;

#[derive(Clone, Copy)]
struct Placement {
    destination: usize,
    threshold: Option<f64>,
}

fn threshold_call(expression: &ScalarExpr) -> Option<(&str, f64)> {
    let ScalarExpr::Func { name, args, .. } = expression else {
        return None;
    };
    if !name.eq_ignore_ascii_case("knn_match") || args.len() < 3 {
        return None;
    }
    match (&args[0], &args[2]) {
        (ScalarExpr::Literal(Value::Str(field)), ScalarExpr::Literal(Value::Float(threshold))) => {
            Some((field, *threshold))
        }
        _ => None,
    }
}

fn selection_layout<'a>(
    items: &[ScalarExpr],
    control: &ProductionControl<'a>,
) -> Result<Option<(ProductionVec<'a, Placement>, usize)>, SQLError> {
    let mut by_field = ProductionMap::<_, Selection>::new(*control);
    for (index, item) in items.iter().enumerate() {
        control.check()?;
        if let Some((field, threshold)) = threshold_call(item) {
            let selection = match by_field.get(field).copied() {
                Some(mut selected) if threshold > selected.threshold => {
                    selected.threshold = threshold;
                    selected
                }
                Some(_) => continue,
                None => Selection {
                    first: index,
                    threshold,
                },
            };
            by_field.insert_copy(field, selection)?;
        }
    }
    if by_field.is_empty() {
        return Ok(None);
    }
    let mut layout = ProductionVec::new(*control);
    layout.reserve(items.len())?;
    let mut survivors = 0;
    for item in items {
        let destination = if threshold_call(item).is_some() {
            REMOVED
        } else {
            let destination = survivors;
            survivors += 1;
            destination
        };
        layout.push_copy(Placement {
            destination,
            threshold: None,
        })?;
    }
    // The ordered map borrows each original field name. Its borrows and reservations end before any input is moved or discarded.
    for (_, selection) in &by_field {
        control.check()?;
        layout.as_mut_slice()[selection.first] = Placement {
            destination: survivors,
            threshold: Some(selection.threshold),
        };
        survivors += 1;
    }
    Ok(Some((layout, survivors)))
}

/// Existing ordinary inputs have no constructor leases; admitted inputs retain their original owners. Scratch always belongs to the invoking request.
#[derive(Clone, Copy)]
struct MergeControl<'a> {
    scratch: ProductionControl<'a>,
    storage: ProductionControl<'a>,
}

fn inline_literal(
    value: Value,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    debug_assert!(matches!(
        value,
        Value::Null | Value::Bool(_) | Value::Float(_)
    ));
    Ok(OwnedScalarExpr::literal(
        control.finish(value, control.empty_reservation())?,
        control,
    )?)
}

fn merge_and(
    mut expression: OwnedScalarExpr,
    control: MergeControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let ScalarExpr::And(items) = &*expression else {
        unreachable!("threshold conjunction")
    };
    if let Some((mut layout, survivors)) = selection_layout(items, &control.scratch)? {
        for (index, placement) in layout.iter().enumerate() {
            control.scratch.check()?;
            expression = expression.map_child(index, &control.storage, |item| {
                if placement.destination == REMOVED {
                    drop(item);
                    inline_literal(Value::Null, &control.storage)
                } else if let Some(threshold) = placement.threshold {
                    item.map_child(2, &control.storage, |_| {
                        inline_literal(Value::Float(threshold), &control.storage)
                    })
                } else {
                    merge_owned_vector_thresholds(item, control)
                }
            })?;
        }
        // Each swap places a surviving expression and its owner in the same unique final slot.
        for index in 0..layout.len() {
            loop {
                control.scratch.check()?;
                let destination = layout[index].destination;
                if destination == REMOVED || destination == index {
                    break;
                }
                expression = expression.swap_children(index, destination, &control.storage)?;
                layout.as_mut_slice().swap(index, destination);
            }
        }
        expression = expression.truncate_boolean_children(survivors, &control.storage)?;
    } else {
        expression = expression.map_children(&control.storage, |_, item| {
            merge_owned_vector_thresholds(item, control)
        })?;
    }
    match expression.child_count() {
        0 => {
            drop(expression);
            inline_literal(Value::Bool(true), &control.storage)
        }
        1 => Ok(expression.select_child(0)),
        _ => Ok(expression),
    }
}

fn merge_owned_vector_thresholds(
    expression: OwnedScalarExpr,
    control: MergeControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    control.scratch.check()?;
    match &*expression {
        ScalarExpr::And(_) => merge_and(expression, control),
        ScalarExpr::Or(_) | ScalarExpr::Not(_) => expression
            .map_children(&control.storage, |_, item| {
                merge_owned_vector_thresholds(item, control)
            }),
        _ => Ok(expression),
    }
}

pub(super) fn merge_vector_thresholds(
    expression: ScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<ScalarExpr, SQLError> {
    let control = MergeControl {
        scratch: *control,
        storage: ProductionControl::uncontrolled(),
    };
    let output = merge_owned_vector_thresholds(OwnedScalarExpr::uncontrolled(expression), control)?;
    // This entry point consumes an ordinary input and introduces no structural allocation. Controlled plan inputs use the same rewrite with their original storage control.
    let (output, memory) = output.finish(&control.storage)?.into_parts();
    debug_assert!(memory.is_none());
    Ok(output)
}

#[cfg(test)]
mod tests;
