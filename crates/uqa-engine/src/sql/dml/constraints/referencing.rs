//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{DocId, Document, Engine, SQLError};

pub(in crate::sql) fn integer_primary_key_doc_id(
    engine: &Engine,
    table: &str,
    doc: &Document,
) -> Result<Option<DocId>, SQLError> {
    uqa_execution::mutation::identity::integer_primary_key_doc_id(engine, table, doc)
}
