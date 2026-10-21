//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered persisted scans with pure projection under the storage borrow.

use super::LocalTableRowSource;
use crate::{ExecResult, PhysicalRow};
use uqa_sql::SQLError;

impl LocalTableRowSource {
    pub(super) fn next_borrowed_physical_rows_batch(
        &mut self,
        max_rows: usize,
    ) -> ExecResult<Option<Vec<PhysicalRow>>> {
        let fields = self.columns.iter().map(String::as_str).collect::<Vec<_>>();
        let mut rows = Vec::with_capacity(max_rows);
        let mut size = self.batch_layout.for_schema(&self.physical_schema)?;
        while rows.len() < max_rows {
            self.cancellation.check().map_err(SQLError::from)?;
            let remaining = max_rows - rows.len();
            let shared = self
                .table
                .read_documents()
                .next_shared_fields(self.after, remaining.min(self.shared_page_rows), &fields)
                .map_err(|error| {
                    SQLError::Internal(format!(
                        "scan shared projected fields from `{}`: {error}",
                        self.table_name
                    ))
                })?;
            if let Some(shared) = shared {
                let reached_end = shared.is_empty();
                for (id, shared) in shared {
                    self.after = Some(id);
                    let keep = shared.with_projected(|values| {
                        self.predicate
                            .as_ref()
                            .map_or(Ok(true), |predicate| predicate.keep(values))
                    })?;
                    if keep {
                        let (values, projection) = shared.into_parts();
                        let row = self.with_lock_identity(
                            PhysicalRow::from_shared_values(values, projection),
                            id,
                        )?;
                        let before = size.bytes();
                        size.append(&row)?;
                        self.shared_page_rows = self
                            .scan_batch_bytes
                            .div_ceil((size.bytes() - before).max(1))
                            .clamp(1, crate::DEFAULT_BATCH_SIZE);
                        rows.push(row);
                        if size.bytes() >= self.scan_batch_bytes {
                            break;
                        }
                    }
                }
                if reached_end || (!rows.is_empty() && size.bytes() >= self.scan_batch_bytes) {
                    break;
                }
                continue;
            }
            let mut last = None;
            let mut failure = None;
            let visited = self
                .table
                .read_documents()
                .for_each_next_fields_borrowed(self.after, remaining, &fields, &mut |id, values| {
                    last = Some(id);
                    let result = (|| -> ExecResult<()> {
                        self.cancellation.check().map_err(SQLError::from)?;
                        if let Some(predicate) = &self.predicate {
                            if !predicate.keep(values)? {
                                return Ok(());
                            }
                        }
                        let row = PhysicalRow::from_values(
                            values.iter().map(|value| (*value).clone()).collect(),
                        );
                        let row = self.with_lock_identity(row, id)?;
                        size.append(&row)?;
                        rows.push(row);
                        Ok(())
                    })();
                    if let Err(error) = result {
                        failure = Some(error);
                        return false;
                    }
                    rows.is_empty() || size.bytes() < self.scan_batch_bytes
                })
                .map_err(|error| {
                    SQLError::Internal(format!(
                        "scan `{}` borrowed projected documents: {error}",
                        self.table_name
                    ))
                });
            if let Some(error) = failure {
                return Err(error);
            }
            let Some(visited) = visited? else {
                return Ok((!rows.is_empty()).then_some(rows));
            };
            if let Some(last) = last {
                self.after = Some(last);
            }
            if visited == 0 || (!rows.is_empty() && size.bytes() >= self.scan_batch_bytes) {
                break;
            }
        }
        Ok(Some(rows))
    }
}
