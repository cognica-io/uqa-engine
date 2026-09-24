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
        let Some(dimensions) = shape::produced(&elements, control)? else {
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
            },
            memory: control.combine(
                control.combine(elements_memory, dimensions_memory),
                control.combine(bounds_memory, header),
            ),
        };
        normalize(&mut parts.value.elements, &mut parts.memory, control)?;
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
    memory: &mut Option<MemoryReservation>,
    control: &ProductionControl<'_>,
) -> Result<(), ValueRetentionError> {
    for value in elements {
        control.check()?;
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
        if let Value::List(values) = value {
            normalize(values, memory, control)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
