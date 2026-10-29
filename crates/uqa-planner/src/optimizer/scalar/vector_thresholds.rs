//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Move the first threshold call for each field, admitting only the merge's temporary index.

use super::{rewrite_box, ScalarExpr, Value};
use uqa_core::memory::{ProductionControl, ProductionMap, ProductionVec};
use uqa_sql::SQLError;

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

fn merge_and(
    mut items: Vec<ScalarExpr>,
    control: &ProductionControl<'_>,
) -> Result<ScalarExpr, SQLError> {
    if let Some((mut layout, survivors)) = selection_layout(&items, control)? {
        for (item, placement) in items.iter_mut().zip(layout.iter()) {
            control.check()?;
            if placement.destination == REMOVED {
                *item = ScalarExpr::Literal(Value::Null);
            } else if let Some(threshold) = placement.threshold {
                let ScalarExpr::Func { args, .. } = item else {
                    unreachable!("selected threshold call")
                };
                args[2] = ScalarExpr::Literal(Value::Float(threshold));
            } else {
                merge_slot(item, control)?;
            }
        }
        // Each swap places a surviving expression in its unique final slot. Deleted slots contain only NULL, and no output allocation or payload copy is needed.
        for index in 0..items.len() {
            loop {
                control.check()?;
                let destination = layout[index].destination;
                if destination == REMOVED || destination == index {
                    break;
                }
                items.swap(index, destination);
                layout.as_mut_slice().swap(index, destination);
            }
        }
        items.truncate(survivors);
    } else {
        for item in &mut items {
            merge_slot(item, control)?;
        }
    }
    Ok(match items.len() {
        0 => ScalarExpr::Literal(Value::Bool(true)),
        1 => items.pop().unwrap(),
        _ => ScalarExpr::And(items),
    })
}

fn merge_slot(
    expression: &mut ScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let original = std::mem::replace(expression, ScalarExpr::Literal(Value::Null));
    *expression = merge_vector_thresholds(original, control)?;
    Ok(())
}

pub(super) fn merge_vector_thresholds(
    expression: ScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<ScalarExpr, SQLError> {
    control.check()?;
    Ok(match expression {
        ScalarExpr::And(items) => merge_and(items, control)?,
        ScalarExpr::Or(mut items) => {
            for item in &mut items {
                merge_slot(item, control)?;
            }
            ScalarExpr::Or(items)
        }
        ScalarExpr::Not(inner) => ScalarExpr::Not(rewrite_box(inner, |inner| {
            merge_vector_thresholds(inner, control)
        })?),
        other => other,
    })
}

#[cfg(test)]
mod tests;
