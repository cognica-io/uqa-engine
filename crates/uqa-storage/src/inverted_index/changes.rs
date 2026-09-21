//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Logical text changes from the original reverse entries and the already evaluated replacement.

use super::{DocId, StorageBackendResult, TokenTermKey};

/// Logical write intent, independent of shared posting clusters, acceleration caches and physical counter records. Replacing a posting remains an intent even when its contents are equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvertedIndexChange<'a> {
    Posting {
        doc_id: DocId,
        field: &'a str,
        term: &'a TokenTermKey,
    },
    Document {
        doc_id: DocId,
        field: &'a str,
    },
    FieldStatistics {
        field: &'a str,
    },
}

/// Capture evaluated changes without reentering the provider. Changes are provisional until the mutation succeeds; discard them on error. Transaction owners register the captured intents before completing the surrounding statement, with savepoint undo covering both private changes and intents.
pub type InvertedIndexChangeVisitor<'a> =
    dyn FnMut(InvertedIndexChange<'_>) -> StorageBackendResult<()> + 'a;

/// Field membership and normalization length determine the corpus scalars. Rewriting a same-length field does not change those scalars merely because the physical counter record is replaced.
pub fn visit_field_replacement(
    visit: &mut InvertedIndexChangeVisitor<'_>,
    doc_id: DocId,
    field: &str,
    previous_length: Option<u64>,
    replacement_length: Option<u64>,
) -> StorageBackendResult<()> {
    if previous_length.is_none() && replacement_length.is_none() {
        return Ok(());
    }
    visit(InvertedIndexChange::Document { doc_id, field })?;
    if previous_length != replacement_length {
        visit(InvertedIndexChange::FieldStatistics { field })?;
    }
    Ok(())
}
