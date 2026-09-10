//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use uqa_core::DocId;
use uqa_execution::mutation::prepared::PreparedDocumentRewrite;
use uqa_sql::{SQLError, SQLParam};
pub(in crate::sql) fn stage_prepared_document_rewrite(
    engine: &Engine,
    prepared: &mut PreparedDocumentRewrite,
    params: &[SQLParam],
    root_updated_columns: Option<&[String]>,
    after_row_events: &mut Vec<crate::sql::triggers::AfterRowTriggerEvent>,
) -> Result<DocId, SQLError> {
    uqa_execution::mutation::staging::stage_prepared_document_rewrite(
        engine.mutation_staging_context(),
        prepared,
        params,
        root_updated_columns,
        after_row_events,
    )
}
