//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Copy projected rows while an ordered provider cursor holds its storage borrow.

use super::{ScoredDocumentSource, ScoredInputCursor};
use crate::{ExecResult, PhysicalRow, ProjectedRow};
use uqa_sql::SQLError;

impl ScoredDocumentSource {
    pub(super) fn next_borrowed_physical_batch(
        &mut self,
        max_rows: usize,
    ) -> ExecResult<Option<Vec<PhysicalRow>>> {
        if self.lock_origin.is_some()
            || self.recheck_pinned
            || crate::query::generated::projection_contains_virtual_generated_column(
                &self.column_definitions,
                &self.projected_fields,
            )
            || crate::query::document_projection::projections_use_tuple_xmin(
                &self.projected_fields,
                &self.column_definitions,
            )
        {
            return Ok(None);
        }
        let ScoredInputCursor::All { mut after } = self.input else {
            return Ok(None);
        };
        if max_rows == 0 {
            return Ok(Some(Vec::new()));
        }
        self.serializable.observe_relation()?;
        let fields = self
            .projected_fields
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let mut rows = Vec::with_capacity(max_rows);
        while rows.len() < max_rows {
            let remaining = max_rows - rows.len();
            let mut last = None;
            let mut failure = None;
            let visited = self
                .table
                .read_documents()
                .for_each_next_fields_borrowed(after, remaining, &fields, &mut |id, values| {
                    last = Some(id);
                    // Only positional evaluation and owned row construction are allowed here: neither can reenter the provider.
                    let result = (|| -> ExecResult<()> {
                        if let Some(predicate) = &self.predicate {
                            if !predicate.keep(values)? {
                                return Ok(());
                            }
                        }
                        let metadata = self.row_metadata(id, 0.0);
                        let extras = [metadata.doc_id()?, metadata.score(), metadata.table_oid()];
                        let row =
                            ProjectedRow::new(&self.schema, &self.projected_slots, values, &extras);
                        let row = PhysicalRow::from_values(row.into_values());
                        rows.push(self.append_metadata_attributes(row, id, 0.0)?);
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
                return Err(error);
            }
            let Some(visited) = visited? else {
                return Ok((!rows.is_empty()).then_some(rows));
            };
            if let Some(last) = last {
                after = Some(last);
                self.input = ScoredInputCursor::All { after };
            }
            if visited == 0 {
                break;
            }
        }
        Ok(Some(rows))
    }
}
