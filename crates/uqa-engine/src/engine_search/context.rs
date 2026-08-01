//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Snapshot assembly for operator execution.

use super::{storage_sql_error, Engine, ExecutionContext, SQLError};

impl Engine {
    pub(crate) fn snapshot_context(
        &self,
        table: &str,
    ) -> Result<Option<ExecutionContext>, SQLError> {
        let Some(t) = self
            .try_table(table)
            .map_err(|error| storage_sql_error("resolve snapshot table", error))?
        else {
            return Ok(None);
        };
        let inv = t
            .inverted_index
            .read()
            .snapshot()
            .map_err(|error| storage_sql_error("snapshot inverted index", error))?;
        let docs = t
            .document_store
            .read()
            .snapshot()
            .map_err(|error| storage_sql_error("snapshot document store", error))?;

        let mut ctx = ExecutionContext::new()
            .with_inverted_index(inv)
            .with_document_store(docs);

        for (field, idx) in t.vector_indexes.read().iter() {
            ctx = ctx.with_vector_index(
                field.clone(),
                idx.snapshot()
                    .map_err(|error| storage_sql_error("snapshot vector index", error))?,
            );
        }

        Ok(Some(ctx))
    }
}
