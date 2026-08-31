//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! VECTOR and TENSOR value normalization for indexes.

use super::{
    column_type_name, dml_storage_error, validate_vector_dimensions, value_to_tensor,
    value_to_vector, ColumnType, Document, Engine, SQLError, Value,
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

pub(in crate::sql) fn document_vectors(
    engine: &Engine,
    table: &str,
    document: &Document,
) -> Result<std::collections::BTreeMap<uqa_core::FieldName, Vec<Vec<f32>>>, SQLError> {
    let mut vectors = std::collections::BTreeMap::new();
    for (field, value) in document {
        let Some(ty) = engine
            .column_type(table, field)
            .map_err(|err| dml_storage_error("vector extraction", err))?
        else {
            continue;
        };
        if matches!(ty, ColumnType::Vector(_) | ColumnType::Tensor(_)) {
            vectors.insert(field.clone(), index_vectors_for_type(value, &ty)?);
        }
    }
    Ok(vectors)
}
