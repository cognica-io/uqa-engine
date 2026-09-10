//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind engine relation metadata to generated-column validation and evaluation.

use crate::Engine;
pub(crate) use uqa_sql::schema::generated::prepare_generated_columns;
use uqa_sql::SQLError;
use uqa_storage::document_store::Document;

pub(crate) fn refresh_stored_generated_columns(
    engine: &Engine,
    table: &str,
    document: &mut Document,
) -> Result<(), SQLError> {
    uqa_execution::mutation::assignment::refresh_stored_generated_columns(
        engine.mutation_assignment_context(),
        table,
        document,
    )
}
