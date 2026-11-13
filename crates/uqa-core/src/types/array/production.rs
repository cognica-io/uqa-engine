//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Array construction transfers admitted element buffers without copying them.

use super::{shape, ArrayStorage, ArrayValue};
use crate::{
    memory::{MemoryReservation, Produced, ProductionControl, ProductionVec},
    Value, ValueRetentionError,
};

mod row_major;
use crate::types::work::{checkpoint, inline, Work};

impl ArrayValue {
    pub fn try_new_with_control(
        elements: Produced<Vec<Value>>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Self>>, ValueRetentionError> {
        Self::produce(elements, None, control)
    }

    pub fn with_lower_bounds_with_control(
        elements: Produced<Vec<Value>>,
        lower_bounds: Produced<Vec<i32>>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Self>>, ValueRetentionError> {
        Self::produce(elements, Some(lower_bounds), control)
    }

    fn produce(
        elements: Produced<Vec<Value>>,
        lower_bounds: Option<Produced<Vec<i32>>>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Self>>, ValueRetentionError> {
        let rank = lower_bounds.as_ref().map(|bounds| bounds.len());
        let Some(dimensions) = shape::produced(&elements, rank, control)? else {
            return Ok(None);
        };
        let lower_bounds = match lower_bounds {
            Some(bounds) if bounds.len() != dimensions.len() => return Ok(None),
            Some(bounds) => bounds,
            None => {
                let mut bounds = ProductionVec::new(*control);
                bounds.reserve(dimensions.len())?;
                for _ in &*dimensions {
                    bounds.push_copy(1)?;
                }
                bounds.finish()?
            }
        };
        let header = control.reserve(Self::decoded_header_bytes())?;
        let (elements, elements_memory) = elements.into_parts();
        let (dimensions, dimensions_memory) = dimensions.into_parts();
        let (lower_bounds, bounds_memory) = lower_bounds.into_parts();
        // The fields keep every input allocation alive until its lease is released, including an early cancellation while normalizing nested arrays.
        let mut parts = Parts {
            value: ArrayStorage {
                elements,
                dimensions,
                lower_bounds,
                element_type_oid: None,
            },
            memory: control.combine(
                control.combine(elements_memory, dimensions_memory),
                control.combine(bounds_memory, header),
            ),
        };
        normalize(
            &mut parts.value.elements,
            parts.value.dimensions.len(),
            &mut parts.memory,
            control,
        )?;
        let value = Self {
            storage: Box::new(parts.value),
        };
        control.finish(value, parts.memory).map(Some)
    }
}

struct Parts {
    value: ArrayStorage,
    memory: Option<MemoryReservation>,
}

pub(super) fn normalize(
    elements: &mut [Value],
    rank: usize,
    memory: &mut Option<MemoryReservation>,
    control: &ProductionControl<'_>,
) -> Result<(), ValueRetentionError> {
    inline(normalize_with_work(
        elements,
        rank,
        memory,
        control,
        &mut (),
    ))
}

pub(super) async fn normalize_with_work<W: Work>(
    elements: &mut [Value],
    rank: usize,
    memory: &mut Option<MemoryReservation>,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<(), W::Error> {
    checkpoint(work, &mut || control.check()).await?;
    if rank <= 1 {
        return Ok(());
    }
    let mut stack = shape::Buffer::new(control.budget());
    stack.push((elements.iter_mut(), rank))?;
    while let Some((values, rank)) = stack.last_mut() {
        checkpoint(work, &mut || control.check()).await?;
        let Some(value) = values.next() else {
            stack.pop();
            continue;
        };
        if let Value::Array(array) = value {
            let released = array.retained_header_bytes()
                + array.storage.dimensions.capacity() * size_of::<usize>()
                + array.storage.lower_bounds.capacity() * size_of::<i32>();
            let Value::Array(array) = std::mem::take(value) else {
                unreachable!("array variant was checked");
            };
            *value = Value::List(array.into_elements());
            if let Some(memory) = memory {
                drop(memory.split(released));
            }
        }
        let child_rank = *rank - 1;
        if child_rank > 1 {
            if let Value::List(values) = value {
                stack.push((values.iter_mut(), child_rank))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
