//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate vector and tensor values used by physical indexes.

use super::conversion::validate_vector_dimensions;
use crate::catalog::type_metadata::column_type_name;
use crate::expr::{value_to_tensor, value_to_vector};
use crate::{ColumnType, SQLError};
use uqa_core::Value;

pub fn index_vectors_for_type(value: &Value, ty: &ColumnType) -> Result<Vec<Vec<f32>>, SQLError> {
    // SQL VECTOR/TENSOR columns are nullable unless their declaration says
    // otherwise. A NULL value therefore means that the row has no vectors to
    // index; it is not a malformed vector. Returning an empty replacement set
    // also clears any vectors left by an UPDATE ... SET field = NULL while
    // retaining strict validation for every non-NULL value.
    if matches!(value, Value::Null) {
        return Ok(Vec::new());
    }
    match ty {
        ColumnType::Vector(dim) => {
            let vector = value_to_vector(value)?;
            validate_vector_dimensions(*dim, vector.len())?;
            Ok(vec![vector])
        }
        ColumnType::Tensor(dim) => {
            let tensor = value_to_tensor(value)?;
            for vector in &tensor {
                validate_vector_dimensions(*dim, vector.len())?;
            }
            Ok(tensor)
        }
        _ => Err(SQLError::TypeMismatch(format!(
            "{} is not vector-indexable",
            column_type_name(ty)
        ))),
    }
}
