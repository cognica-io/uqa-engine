//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rectangular shape validation shares one borrowed traversal for ordinary and controlled decoding.

use crate::{
    memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation},
    CancellationToken, Value, ValueRetentionError,
};

type Shape = Option<(Vec<usize>, Option<MemoryReservation>)>;

pub(super) fn unbounded(elements: &[Value], preserve_empty_dimension: bool) -> Option<Vec<usize>> {
    validate(elements, preserve_empty_dimension, None, &mut || Ok(()))
        .ok()?
        .map(|(dimensions, _)| dimensions)
}

pub(super) fn budgeted(
    elements: &[Value],
    preserve_empty_dimension: bool,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Option<Budgeted<Vec<usize>>>, ValueRetentionError> {
    validate(
        elements,
        preserve_empty_dimension,
        Some(memory),
        &mut || cancellation.check().map_err(Into::into),
    )
    .map(|shape| {
        shape.map(|(dimensions, memory)| {
            Budgeted::new(dimensions, memory.expect("controlled array dimensions"))
        })
    })
}

pub(super) fn produced(
    elements: &[Value],
    preserve_empty_dimension: bool,
    control: &crate::memory::ProductionControl<'_>,
) -> Result<Option<crate::memory::Produced<Vec<usize>>>, ValueRetentionError> {
    validate(
        elements,
        preserve_empty_dimension,
        control.budget(),
        &mut || control.check(),
    )?
    .map(|(dimensions, memory)| control.finish(dimensions, memory))
    .transpose()
}

fn nested(value: &Value) -> Option<&[Value]> {
    match value {
        Value::List(values) => Some(values),
        Value::Array(array) => Some(array.elements()),
        _ => None,
    }
}

fn validate(
    elements: &[Value],
    preserve_empty_dimension: bool,
    memory: Option<&MemoryBudget>,
    check: &mut impl FnMut() -> Result<(), ValueRetentionError>,
) -> Result<Shape, ValueRetentionError> {
    check()?;
    let mut dimensions = Buffer::new(memory);
    if elements.is_empty() {
        if preserve_empty_dimension {
            dimensions.push(0)?;
        }
        return Ok(Some(dimensions.into_parts()));
    }
    let mut first = elements;
    loop {
        check()?;
        dimensions.push(first.len())?;
        let Some(children) = first.first().and_then(nested) else {
            break;
        };
        first = children;
    }

    let mut stack = Buffer::new(memory);
    stack.push((elements.iter(), 0))?;
    while let Some((values, depth)) = stack.last_mut() {
        check()?;
        let Some(value) = values.next() else {
            stack.pop();
            continue;
        };
        let child_depth = *depth + 1;
        match (nested(value), dimensions.as_slice().get(child_depth)) {
            (None, None) => {}
            (Some(children), Some(&length)) if children.len() == length => {
                if !children.is_empty() {
                    stack.push((children.iter(), child_depth))?;
                }
            }
            _ => return Ok(None),
        }
    }
    check()?;
    Ok(Some(dimensions.into_parts()))
}

enum Buffer<T> {
    Unbounded(Vec<T>),
    Bounded(BudgetedVec<T>),
}

impl<T> Buffer<T> {
    fn new(memory: Option<&MemoryBudget>) -> Self {
        memory.map_or_else(
            || Self::Unbounded(Vec::new()),
            |memory| Self::Bounded(BudgetedVec::new(memory)),
        )
    }

    fn push(&mut self, value: T) -> Result<(), ValueRetentionError> {
        match self {
            Self::Unbounded(values) => values.push(value),
            Self::Bounded(values) => values.push(value)?,
        }
        Ok(())
    }

    fn as_slice(&self) -> &[T] {
        match self {
            Self::Unbounded(values) => values,
            Self::Bounded(values) => values,
        }
    }

    fn last_mut(&mut self) -> Option<&mut T> {
        match self {
            Self::Unbounded(values) => values.last_mut(),
            Self::Bounded(values) => values.last_mut(),
        }
    }

    fn pop(&mut self) {
        match self {
            Self::Unbounded(values) => {
                values.pop();
            }
            Self::Bounded(values) => {
                values.pop();
            }
        }
    }

    fn into_parts(self) -> (Vec<T>, Option<MemoryReservation>) {
        match self {
            Self::Unbounded(values) => (values, None),
            Self::Bounded(values) => {
                let (values, memory) = values.into_parts();
                (values, Some(memory))
            }
        }
    }
}

#[cfg(test)]
mod tests;
