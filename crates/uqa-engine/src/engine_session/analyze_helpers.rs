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

const HISTOGRAM_BUCKETS: usize = 100;
const MCV_COUNT: usize = 10;

pub(super) fn distinct_count(values: &[Value]) -> StorageBackendResult<u64> {
    let mut set: std::collections::BTreeSet<&Value> = std::collections::BTreeSet::new();
    for value in values {
        set.insert(value);
    }
    u64::try_from(set.len())
        .map_err(|_| StorageBackendError::Other("ANALYZE distinct count exceeds u64".into()))
}

pub(super) fn build_histogram(values: &[&Value]) -> Vec<Value> {
    if values.is_empty() {
        return Vec::new();
    }
    let mut sorted: Vec<Value> = values.iter().map(|value| (*value).clone()).collect();
    sorted.sort();
    let count = sorted.len();
    let bucket_count = HISTOGRAM_BUCKETS.min(count);
    if bucket_count <= 1 {
        return vec![sorted[0].clone(), sorted[count - 1].clone()];
    }
    let mut boundaries = vec![sorted[0].clone()];
    for bucket in 1..bucket_count {
        let value = &sorted[(bucket * count) / bucket_count];
        if Some(value) != boundaries.last() {
            boundaries.push(value.clone());
        }
    }
    if boundaries.last() != Some(&sorted[count - 1]) {
        boundaries.push(sorted[count - 1].clone());
    }
    boundaries
}

pub(super) fn build_mcv(values: &[Value], total: u64) -> (Vec<Value>, Vec<f64>) {
    if values.is_empty() || total == 0 {
        return (Vec::new(), Vec::new());
    }
    let mut counts: std::collections::BTreeMap<&Value, u64> = std::collections::BTreeMap::new();
    for value in values {
        *counts.entry(value).or_insert(0) += 1;
    }
    let distinct = counts.len();
    if distinct == 0 {
        return (Vec::new(), Vec::new());
    }
    let average_frequency = 1.0 / distinct as f64;
    let mut sorted: Vec<(&Value, u64)> = counts.into_iter().collect();
    sorted.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    let total = total as f64;
    let mut common_values = Vec::new();
    let mut common_frequencies = Vec::new();
    for (value, count) in sorted.into_iter().take(MCV_COUNT) {
        let frequency = count as f64 / total;
        if frequency > average_frequency {
            common_values.push(value.clone());
            common_frequencies.push(frequency);
        }
    }
    (common_values, common_frequencies)
}
