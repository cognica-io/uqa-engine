//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! VECTOR and TENSOR value normalization for indexes.

use super::{
    column_type_name, validate_vector_dimensions, value_to_tensor, value_to_vector, ColumnType,
    SQLError, Value,
};

pub(in crate::sql) fn index_vectors_for_type(
    value: &Value,
    ty: &ColumnType,
) -> Result<Vec<Vec<f32>>, SQLError> {
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
