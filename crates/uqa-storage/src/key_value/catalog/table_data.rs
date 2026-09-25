//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact table-owned key families for atomic removal, rename, and destination checks.

use super::super::{occurrence_keys as occurrence, KeyValueRead};
use super::analyzers::field_binding_prefix;
use super::keys::rekey_prefix;
use super::physical_indexes::{drop_table_indexes, rename_table_indexes, table_index_prefixes};
use super::{
    column_stats_prefix, doc_length_key_prefix, document_key_prefix, field_stats_key_prefix,
    posting_cluster_positions_key_prefix, posting_cluster_score_key_prefix,
    posting_document_key_prefix, posting_key_prefix, reverse_posting_key_prefix,
    table_field_analyzer_prefix, vector_key_prefix, KeyValueBatch, StorageBackendResult,
};

fn row_prefixes(name: &str) -> StorageBackendResult<[Vec<u8>; 13]> {
    Ok([
        document_key_prefix(name)?,
        posting_key_prefix(name)?,
        occurrence::table_prefix(name)?,
        posting_cluster_score_key_prefix(name)?,
        posting_cluster_positions_key_prefix(name)?,
        posting_document_key_prefix(name)?,
        doc_length_key_prefix(name)?,
        field_stats_key_prefix(name)?,
        reverse_posting_key_prefix(name)?,
        vector_key_prefix(name)?,
        super::super::vector_index::origin::table_prefix(name)?,
        super::super::vector_index::origin::journal::table_prefix(name)?,
        column_stats_prefix(name)?,
    ])
}

fn analyzer_prefixes(name: &str) -> StorageBackendResult<[Vec<u8>; 2]> {
    Ok([
        table_field_analyzer_prefix(name)?,
        field_binding_prefix(name)?,
    ])
}

pub(super) fn has_data(read: &dyn KeyValueRead, name: &str) -> StorageBackendResult<bool> {
    for prefix in row_prefixes(name)?
        .into_iter()
        .chain(analyzer_prefixes(name)?)
        .chain(table_index_prefixes(name)?)
    {
        if read.contains_prefix_budgeted(&prefix, read.control())? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn clear(
    batch: &mut dyn KeyValueBatch,
    name: &str,
    analyzers: bool,
) -> StorageBackendResult<()> {
    batch.reset_occurrences(name)?;
    for prefix in row_prefixes(name)? {
        batch.delete_prefix(&prefix)?;
    }
    if analyzers {
        for prefix in analyzer_prefixes(name)? {
            batch.delete_prefix(&prefix)?;
        }
    }
    drop_table_indexes(batch, name)
}

pub(super) fn rename(
    read: &dyn KeyValueRead,
    batch: &mut dyn KeyValueBatch,
    from: &str,
    to: &str,
) -> StorageBackendResult<()> {
    batch.reset_occurrences(from)?;
    batch.reset_occurrences(to)?;
    let old = row_prefixes(from)?
        .into_iter()
        .chain(analyzer_prefixes(from)?);
    let new = row_prefixes(to)?.into_iter().chain(analyzer_prefixes(to)?);
    for (old, new) in old.zip(new) {
        rekey_prefix(read, batch, &old, &new)?;
    }
    rename_table_indexes(read, batch, from, to)
}
