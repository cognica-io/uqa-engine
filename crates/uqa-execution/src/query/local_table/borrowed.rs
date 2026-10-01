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
        while rows.len() < max_rows {
            self.cancellation.check().map_err(SQLError::from)?;
            let remaining = max_rows - rows.len();
            let shared = self
                .table
                .read_documents()
                .next_shared_fields(self.after, remaining, &fields)
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
                        rows.push(self.with_lock_identity(
                            PhysicalRow::from_shared_values(values, projection),
                            id,
                        )?);
                    }
                }
                if reached_end {
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
                    let result = (|| -> Result<(), SQLError> {
                        self.cancellation.check().map_err(SQLError::from)?;
                        if let Some(predicate) = &self.predicate {
                            if !predicate.keep(values)? {
                                return Ok(());
                            }
                        }
                        let row = PhysicalRow::from_values(
                            values.iter().map(|value| (*value).clone()).collect(),
                        );
                        rows.push(self.with_lock_identity(row, id)?);
                        Ok(())
                    })();
                    if let Err(error) = result {
                        failure = Some(error);
                        return false;
                    }
                    true
                })
                .map_err(|error| {
                    SQLError::Internal(format!(
                        "scan `{}` borrowed projected documents: {error}",
                        self.table_name
                    ))
                });
            if let Some(error) = failure {
                return Err(error.into());
            }
            let Some(visited) = visited? else {
                return Ok((!rows.is_empty()).then_some(rows));
            };
            if let Some(last) = last {
                self.after = Some(last);
            }
            if visited == 0 {
                break;
            }
        }
        Ok(Some(rows))
    }
}
