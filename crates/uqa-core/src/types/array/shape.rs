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

use crate::types::work::{checkpoint, inline, Work};

type Shape = Option<(Vec<usize>, Option<MemoryReservation>)>;

#[cfg(test)]
pub(super) fn unbounded(elements: &[Value], rank: Option<usize>) -> Option<Vec<usize>> {
    validate(elements, rank, None, &mut || Ok(()))
        .ok()?
        .map(|(dimensions, _)| dimensions)
}

pub(super) fn budgeted(
    elements: &[Value],
    rank: Option<usize>,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Option<Budgeted<Vec<usize>>>, ValueRetentionError> {
    validate(elements, rank, Some(memory), &mut || {
        cancellation.check().map_err(Into::into)
    })
    .map(|shape| {
        shape.map(|(dimensions, memory)| {
            Budgeted::new(dimensions, memory.expect("controlled array dimensions"))
        })
    })
}

pub(super) fn produced(
    elements: &[Value],
    rank: Option<usize>,
    control: &crate::memory::ProductionControl<'_>,
) -> Result<Option<crate::memory::Produced<Vec<usize>>>, ValueRetentionError> {
    validate(elements, rank, control.budget(), &mut || control.check())?
        .map(|(dimensions, memory)| control.finish(dimensions, memory))
        .transpose()
}

pub(super) async fn produced_with_work<W: Work>(
    elements: &[Value],
    rank: Option<usize>,
    control: &crate::memory::ProductionControl<'_>,
    work: &mut W,
) -> Result<Option<crate::memory::Produced<Vec<usize>>>, W::Error> {
    validate_with_work(
        elements,
        rank,
        control.budget(),
        &mut || control.check(),
        work,
    )
    .await?
    .map(|(dimensions, memory)| control.finish(dimensions, memory).map_err(Into::into))
    .transpose()
}

fn nested(value: &Value, depth: usize, rank: Option<usize>) -> Option<&[Value]> {
    match value {
        Value::List(values) => Some(values),
        Value::Array(array) if rank != Some(depth) => Some(array.elements()),
        _ => None,
    }
}

fn validate(
    elements: &[Value],
    rank: Option<usize>,
    memory: Option<&MemoryBudget>,
    check: &mut (impl FnMut() -> Result<(), ValueRetentionError> + Send),
) -> Result<Shape, ValueRetentionError> {
    inline(validate_with_work(elements, rank, memory, check, &mut ()))
}

async fn validate_with_work<W: Work>(
    elements: &[Value],
    mut rank: Option<usize>,
    memory: Option<&MemoryBudget>,
    check: &mut (impl FnMut() -> Result<(), ValueRetentionError> + Send),
    work: &mut W,
) -> Result<Shape, W::Error> {
    checkpoint(work, check).await?;
    let mut dimensions = Buffer::new(memory);
    if elements.is_empty() {
        if rank == Some(1) {
            dimensions.push(0)?;
        }
        return Ok(Some(dimensions.into_parts()));
    }
    let mut first = elements;
    loop {
        checkpoint(work, check).await?;
        dimensions.push(first.len())?;
        if rank.is_none() {
            if let Some(Value::Array(array)) = first.first().filter(
                |value| matches!(value, Value::Array(array) if !array.dimensions().is_empty()),
            ) {
                rank = Some(dimensions.as_slice().len() + array.dimensions().len());
            }
        }
        let Some(children) = first
            .first()
            .and_then(|value| nested(value, dimensions.as_slice().len(), rank))
        else {
            break;
        };
        first = children;
    }

    let mut stack = Buffer::new(memory);
    stack.push((elements.iter(), 0))?;
    while let Some((values, depth)) = stack.last_mut() {
        checkpoint(work, check).await?;
        let Some(value) = values.next() else {
            stack.pop();
            continue;
        };
        let child_depth = *depth + 1;
        match (
            nested(value, child_depth, rank),
            dimensions.as_slice().get(child_depth),
        ) {
            (None, None) => {}
            (Some(children), Some(&length)) if children.len() == length => {
                if !children.is_empty() {
                    stack.push((children.iter(), child_depth))?;
                }
            }
            _ => return Ok(None),
        }
    }
    checkpoint(work, check).await?;
    Ok(Some(dimensions.into_parts()))
}

pub(super) enum Buffer<T> {
    Unbounded(Vec<T>),
    Bounded(BudgetedVec<T>),
}

impl<T> Buffer<T> {
    pub(super) fn new(memory: Option<&MemoryBudget>) -> Self {
        memory.map_or_else(
            || Self::Unbounded(Vec::new()),
            |memory| Self::Bounded(BudgetedVec::new(memory)),
        )
    }

    pub(super) fn push(&mut self, value: T) -> Result<(), ValueRetentionError> {
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

    pub(super) fn last_mut(&mut self) -> Option<&mut T> {
        match self {
            Self::Unbounded(values) => values.last_mut(),
            Self::Bounded(values) => values.last_mut(),
        }
    }

    pub(super) fn pop(&mut self) {
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
