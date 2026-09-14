//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit source-rebuild requirement and retirement of legacy index namespaces.

use super::super::codec::{
    doc_length_key_prefix, field_stats_key_prefix, posting_cluster_positions_key_prefix,
    posting_cluster_score_key_prefix, posting_document_key_prefix, posting_key_prefix,
    reverse_posting_key_prefix,
};
use super::{keys, other_error, KeyValueBatch, KeyValueInvertedIndex, StorageBackendResult};

fn legacy_prefixes(table: &str) -> StorageBackendResult<Vec<Vec<u8>>> {
    Ok(vec![
        posting_key_prefix(table)?,
        posting_cluster_score_key_prefix(table)?,
        posting_cluster_positions_key_prefix(table)?,
        posting_document_key_prefix(table)?,
        doc_length_key_prefix(table)?,
        field_stats_key_prefix(table)?,
        reverse_posting_key_prefix(table)?,
    ])
}

impl KeyValueInvertedIndex {
    pub(super) fn needs_source_rebuild(&self) -> StorageBackendResult<bool> {
        let format = self
            .store
            .get(&keys::kind_prefix(&self.table, keys::FORMAT)?)?;
        if format
            .as_deref()
            .is_some_and(|format| format != keys::FORMAT_NAME)
        {
            return Err(other_error("unsupported occurrence index format"));
        }
        for prefix in legacy_prefixes(&self.table)? {
            if !self.store.scan_prefix_after(&prefix, None, 1)?.is_empty() {
                return Ok(true);
            }
        }
        if format.is_none()
            && !self
                .store
                .scan_prefix_after(&keys::table_prefix(&self.table)?, None, 1)?
                .is_empty()
        {
            return Err(other_error("occurrence index format marker is missing"));
        }
        Ok(false)
    }

    pub(super) fn require_graph_format(&self) -> StorageBackendResult<()> {
        if self.needs_source_rebuild()? {
            return Err(other_error(
                "legacy positional data requires an atomic source rebuild",
            ));
        }
        Ok(())
    }

    pub(super) fn clear_index_batch(
        &self,
        batch: &mut dyn KeyValueBatch,
    ) -> StorageBackendResult<()> {
        batch.delete_prefix(&keys::table_prefix(&self.table)?)?;
        for prefix in legacy_prefixes(&self.table)? {
            batch.delete_prefix(&prefix)?;
        }
        batch.put(
            &keys::kind_prefix(&self.table, keys::FORMAT)?,
            keys::FORMAT_NAME,
        )
    }
}
