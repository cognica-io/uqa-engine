//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Extract typed vector index inputs from a physical document.
use super::{constraints::context::ConstraintCatalog, errors::dml_storage_error};
use uqa_sql::{assignment::vectors::index_vectors_for_type, ColumnType, SQLError};
use uqa_storage::document_store::Document;
pub fn document_vectors(
    catalog: &dyn ConstraintCatalog,
    table: &str,
    document: &Document,
) -> Result<std::collections::BTreeMap<uqa_core::FieldName, Vec<Vec<f32>>>, SQLError> {
    let mut vectors = std::collections::BTreeMap::new();
    for (field, value) in document {
        let Some(ty) = catalog
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
