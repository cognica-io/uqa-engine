//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture statistics observations and sort their exact Core identities within the caller's allowance.

use std::sync::Arc;
use uqa_core::{CancellationToken, Value};
use uqa_sql::SQLError;
use uqa_storage::{DocumentStore, StorageBackendResult};

use crate::{
    physical::physical_exec_error, spill::SpillBatchWriter, ExternalSort, PhysicalOperator,
    PhysicalRow, RowSchema, SpillBuffer, SpillScan,
};

#[cfg(test)]
mod tests;

#[derive(Default)]
pub struct ColumnObservationCounts {
    pub nulls: u64,
    pub omitted: u64,
    pub comparable: u64,
}

pub struct AnalyzeObservations {
    pub row_count: u64,
    pub counts: Vec<ColumnObservationCounts>,
    pub values: SortedColumnValues,
}

/// The caller supplies retained snapshots in hierarchy order and the existing value-eligibility policy. Collection finishes before the distribution reader can publish any statistics.
pub fn collect_values(
    snapshots: impl IntoIterator<Item = StorageBackendResult<Arc<dyn DocumentStore>>>,
    columns: &[String],
    bytes: usize,
    cancellation: &CancellationToken,
    accepts: impl Fn(&Value) -> bool,
) -> Result<AnalyzeObservations, SQLError> {
    cancellation.check()?;
    let schema = RowSchema::with_internal_relation_types(
        uqa_sql::ast::InternalRelationId::allocate(),
        vec![None; 2],
    );
    let mut rows = SpillBuffer::new(bytes / 8);
    let mut writer = SpillBatchWriter::new(schema.clone()).map_err(physical_exec_error)?;
    let fields = columns.iter().map(String::as_str).collect::<Vec<_>>();
    let mut counts = (0..columns.len())
        .map(|_| ColumnObservationCounts::default())
        .collect::<Vec<_>>();
    let mut row_count = 0;
    for snapshot in snapshots {
        let snapshot = snapshot.map_err(storage)?;
        let mut after = None;
        loop {
            cancellation.check()?;
            let ids = snapshot.next_doc_ids(after, 128).map_err(storage)?;
            if ids.is_empty() {
                break;
            }
            after = ids.last().copied();
            add(
                &mut row_count,
                u64::try_from(ids.len()).map_err(|_| overflow())?,
            )?;
            let mut failure = None;
            let result = snapshot.for_each_fields_multi(&ids, &fields, &mut |_, values| {
                let result = (|| {
                    cancellation.check()?;
                    if values.len() != columns.len() {
                        return Err(SQLError::Internal(
                            "ANALYZE projection changed its column count".into(),
                        ));
                    }
                    for (column, value) in values.into_iter().enumerate() {
                        let count = &mut counts[column];
                        if matches!(value, Value::Null) {
                            add(&mut count.nulls, 1)?;
                        } else if !accepts(&value) {
                            add(&mut count.omitted, 1)?;
                        } else {
                            if matches!(
                                value,
                                Value::Int(_) | Value::Float(_) | Value::Str(_) | Value::Bool(_)
                            ) {
                                add(&mut count.comparable, 1)?;
                            }
                            writer
                                .push(
                                    &mut rows,
                                    PhysicalRow::from_values(vec![
                                        Value::Int(i64::try_from(column).map_err(|_| overflow())?),
                                        value,
                                    ]),
                                )
                                .map_err(physical_exec_error)?;
                        }
                    }
                    Ok::<_, SQLError>(())
                })();
                if let Err(error) = result {
                    failure = Some(error);
                    return false;
                }
                true
            });
            if let Some(error) = failure {
                return Err(error);
            }
            result.map_err(storage)?;
        }
    }
    writer.finish(&mut rows).map_err(physical_exec_error)?;
    // Each merge input owns a decoded batch. Share the sort allowance across all input readers and its output instead of granting the entire allowance to every run.
    let run_bytes = bytes / (crate::external_sort::EXTERNAL_SORT_MERGE_FAN_IN + 2);
    let mut sorted = ExternalSort::physical_columns(
        Box::new(SpillScan::new(schema, rows)),
        vec![0, 1],
        run_bytes,
        cancellation,
    )
    .map_err(physical_exec_error)?;
    sorted.open().map_err(physical_exec_error)?;
    Ok(AnalyzeObservations {
        row_count,
        counts,
        values: SortedColumnValues {
            sorted,
            pending: Vec::new().into_iter(),
            exhausted: false,
            cancellation: cancellation.clone(),
        },
    })
}

pub struct SortedColumnValues {
    sorted: ExternalSort<'static>,
    pending: std::vec::IntoIter<PhysicalRow>,
    exhausted: bool,
    cancellation: CancellationToken,
}

impl Iterator for SortedColumnValues {
    type Item = Result<(usize, Value), SQLError>;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.exhausted {
            if let Err(error) = self.cancellation.check() {
                self.exhausted = true;
                return Some(Err(error.into()));
            }
        }
        while !self.exhausted {
            if let Some(row) = self.pending.next() {
                return Some((|| {
                    let values: [Value; 2] = row
                        .into_physical_values()
                        .try_into()
                        .map_err(|_| SQLError::Internal("invalid ANALYZE value width".into()))?;
                    let [Value::Int(column), value] = values else {
                        return Err(SQLError::Internal("invalid ANALYZE column identity".into()));
                    };
                    Ok((usize::try_from(column).map_err(|_| overflow())?, value))
                })());
            }
            match self.sorted.next() {
                Ok(Some(batch)) => self.pending = batch.rows.into_iter(),
                Ok(None) => self.exhausted = true,
                Err(error) => {
                    self.exhausted = true;
                    return Some(Err(physical_exec_error(error)));
                }
            }
        }
        None
    }
}

impl Drop for SortedColumnValues {
    fn drop(&mut self) {
        let _ = self.sorted.close();
    }
}

fn storage(error: uqa_storage::StorageBackendError) -> SQLError {
    crate::storage_errors::storage_error("collect ANALYZE values", &error)
}

fn overflow() -> SQLError {
    SQLError::Internal("ANALYZE observation count overflow".into())
}

fn add(target: &mut u64, count: u64) -> Result<(), SQLError> {
    *target = target.checked_add(count).ok_or_else(overflow)?;
    Ok(())
}
