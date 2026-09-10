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

#[derive(Default)]
pub(super) struct ColumnAnalyzeValues {
    pub retained: Vec<Value>,
    pub omitted: u64,
}

impl ColumnAnalyzeValues {
    fn push(&mut self, value: Value) {
        if crate::statistics::value_size::accepts(&value) {
            self.retained.push(value);
        } else {
            self.omitted += 1;
        }
    }

    pub(super) fn extend(&mut self, other: Self) {
        self.retained.extend(other.retained);
        self.omitted += other.omitted;
    }
}

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
        values.insert(column.clone(), ColumnAnalyzeValues::default());
        nulls.insert(column.clone(), 0);
    }
    let fields = columns.iter().map(String::as_str).collect::<Vec<_>>();
    let mut failure = None;
    snapshot.for_each_fields_multi(doc_ids, &fields, &mut |_, row| {
        for (column, value) in columns.iter().zip(row) {
            let result = match value {
                Value::Null => increment_analyze_null(&mut nulls, column),
                value => values
                    .get_mut(column)
                    .ok_or_else(|| {
                        StorageBackendError::Other(format!(
                            "ANALYZE lost the value buffer for column `{column}`"
                        ))
                    })
                    .map(|values| values.push(value)),
            };
            if let Err(error) = result {
                failure = Some(error);
                return false;
            }
        }
        true
    })?;
    if let Some(error) = failure {
        return Err(error);
    }
    Ok((values, nulls))
}

const HISTOGRAM_BUCKETS: usize = crate::statistics::value_size::HISTOGRAM_VALUES - 1;
const MCV_COUNT: usize = crate::statistics::value_size::MCV_VALUES;

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
    let mut sorted = values.to_vec();
    sorted.sort();
    let count = sorted.len();
    let bucket_count = HISTOGRAM_BUCKETS.min(count);
    if bucket_count <= 1 {
        return vec![sorted[0].clone(), sorted[count - 1].clone()];
    }
    let mut boundaries = vec![sorted[0].clone()];
    for bucket in 1..bucket_count {
        let value = sorted[(bucket * count) / bucket_count];
        if Some(value) != boundaries.last() {
            boundaries.push(value.clone());
        }
    }
    if boundaries.last() != Some(sorted[count - 1]) {
        boundaries.push(sorted[count - 1].clone());
    }
    boundaries
}

pub(super) fn build_mcv(values: &[Value], total: u64, distinct: u64) -> (Vec<Value>, Vec<f64>) {
    if values.is_empty() || total == 0 {
        return (Vec::new(), Vec::new());
    }
    let mut counts: std::collections::BTreeMap<&Value, u64> = std::collections::BTreeMap::new();
    for value in values {
        *counts.entry(value).or_insert(0) += 1;
    }
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
