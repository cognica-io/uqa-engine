//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Vector conversion reserves destination buffers before evaluating their elements.

use super::{column_type_name, validate_vector_dimensions, ColumnType, SQLError, Value};
use crate::expr::{tensor_items, vector_element, vector_items};
use uqa_core::{
    memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryError},
    QueryCancelled,
};

/// Preserve assignment conversion while charging tensor headers and float buffers to the caller's allowance. The result transfers its reservations together with its buffers to the retaining index owner.
pub fn index_vectors_for_type_budgeted(
    value: &Value,
    ty: &ColumnType,
    memory: &MemoryBudget,
    poll: &mut impl FnMut() -> Result<(), QueryCancelled>,
) -> Result<Budgeted<Vec<Vec<f32>>>, SQLError> {
    poll()?;
    if matches!(value, Value::Null) {
        return Ok(Budgeted::new(Vec::new(), memory.empty_reservation()));
    }
    let (values, dimensions) = match ty {
        ColumnType::Vector(dim) => (std::slice::from_ref(value), *dim),
        ColumnType::Tensor(dim) => (tensor_items(value)?, *dim),
        _ => {
            return Err(SQLError::TypeMismatch(format!(
                "{} is not vector-indexable",
                column_type_name(ty)
            )))
        }
    };
    // The nested buffers must drop before their payload reservation on every error.
    let mut pending = (BudgetedVec::new(memory), memory.empty_reservation());
    pending.0.reserve(values.len()).map_err(memory_error)?;
    for value in values {
        poll()?;
        let items = vector_items(value)?;
        let mut vector = BudgetedVec::new(memory);
        vector.reserve(items.len()).map_err(memory_error)?;
        for item in items {
            poll()?;
            vector.push(vector_element(item)?).map_err(memory_error)?;
        }
        let (vector, reservation) = vector.into_parts();
        pending.0.push(vector).map_err(memory_error)?;
        pending.1.absorb(reservation);
    }
    for vector in pending.0.iter() {
        poll()?;
        validate_vector_dimensions(dimensions, vector.len())?;
    }
    poll()?;
    let (vectors, mut reservation) = pending.0.into_parts();
    reservation.absorb(pending.1);
    Ok(Budgeted::new(vectors, reservation))
}

fn memory_error(error: MemoryError) -> SQLError {
    SQLError::Routine {
        sqlstate: "53200".into(),
        message: format!("convert vector index input: {error}"),
    }
}

#[cfg(test)]
mod tests;
