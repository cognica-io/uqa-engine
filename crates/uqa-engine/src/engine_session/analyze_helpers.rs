//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! ANALYZE value and NULL collection.

use super::{
    AnalyzeNullCounts, AnalyzeValues, DocId, DocumentStore, StorageBackendError,
    StorageBackendResult, Value,
};

pub(super) fn increment_analyze_null(
    counts: &mut AnalyzeNullCounts,
    column: &str,
) -> StorageBackendResult<()> {
    let count = counts.get_mut(column).ok_or_else(|| {
        StorageBackendError::Other(format!(
            "ANALYZE lost the null counter for column `{column}`"
        ))
    })?;
    *count = count
        .checked_add(1)
        .ok_or_else(|| StorageBackendError::Other("ANALYZE null count overflow".into()))?;
    Ok(())
}

pub(super) fn collect_analyze_values(
    snapshot: &dyn DocumentStore,
    doc_ids: &[DocId],
    columns: &[String],
) -> StorageBackendResult<(AnalyzeValues, AnalyzeNullCounts)> {
    let mut values = AnalyzeValues::new();
    let mut nulls = AnalyzeNullCounts::new();
    for column in columns {
        values.insert(column.clone(), Vec::new());
        nulls.insert(column.clone(), 0);
    }
    for doc_id in doc_ids {
        let Some(document) = snapshot.get(*doc_id)? else {
            for column in columns {
                increment_analyze_null(&mut nulls, column)?;
            }
            continue;
        };
        for column in columns {
            match document.get(column) {
                None | Some(Value::Null) => increment_analyze_null(&mut nulls, column)?,
                Some(value) => values
                    .get_mut(column)
                    .ok_or_else(|| {
                        StorageBackendError::Other(format!(
                            "ANALYZE lost the value buffer for column `{column}`"
                        ))
                    })?
                    .push(value.clone()),
            }
        }
    }
    Ok((values, nulls))
}
