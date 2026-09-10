//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    DocId, Document, Engine, PartitionUpdateRoute, PreparedDocumentRewrite,
    ReferentialActionContext, SQLError, SQLParam,
};

pub(in crate::sql) use uqa_execution::mutation::referential::prepare_partition_update_route;

pub(in crate::sql) fn prepare_routed_document_rewrite(
    engine: &Engine,
    table: &str,
    doc_id: DocId,
    old_document: Document,
    route: PartitionUpdateRoute,
    params: &[SQLParam],
    referential_actions: &mut ReferentialActionContext,
) -> Result<Option<PreparedDocumentRewrite>, SQLError> {
    uqa_execution::mutation::referential::prepare_routed_document_rewrite(
        &engine.referential_execution_context(),
        table,
        doc_id,
        old_document,
        route,
        params,
        referential_actions,
    )
}
