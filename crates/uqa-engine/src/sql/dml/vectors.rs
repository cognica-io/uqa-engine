//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
pub(in crate::sql) use uqa_sql::assignment::vectors::index_vectors_for_type;
use uqa_sql::SQLError;
use uqa_storage::document_store::Document;
pub(in crate::sql) fn document_vectors(
    engine: &Engine,
    table: &str,
    document: &Document,
) -> Result<std::collections::BTreeMap<uqa_core::FieldName, Vec<Vec<f32>>>, SQLError> {
    uqa_execution::mutation::vectors::document_vectors(engine, table, document)
}
