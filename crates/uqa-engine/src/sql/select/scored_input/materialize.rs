//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projected document filtering, materialization, and aggregate delivery.

use uqa_core::Value;
use uqa_execution::ExecResult;
use uqa_sql::ResultRow;

use super::{ScoredDocumentSource, ScoredEntry};
use crate::sql::{DocId, SQLError};

type KeptEntryVisitor<'visitor> =
    dyn FnMut(DocId, &ScoredEntry, &[&str], &[&Value]) -> ExecResult<()> + 'visitor;

impl ScoredDocumentSource {
    /// Feed an unranked in-memory scan directly from storage-owned values into a projected aggregate. `Some(true)` means the scan reached EOF, `Some(false)` means another batch remains, and `None` selects the backend-neutral fallback.
    pub(super) fn aggregate_shared_batch(
        &mut self,
        max_rows: usize,
        executor: &mut dyn uqa_execution::AggregateExecutor,
    ) -> ExecResult<Option<bool>> {
        if self.lock_origin.is_some()
            || self.recheck_pinned
            || crate::engine_generated::projection_contains_virtual_generated_column(
                &self.column_definitions,
                &self.projected_fields,
            )
            || crate::sql::projections_use_tuple_xmin(
                &self.projected_fields,
                &self.column_definitions,
            )
        {
            return Ok(None);
        }
        let after = match &self.input {
            super::ScoredInputCursor::All { after } => *after,
            super::ScoredInputCursor::Entries(_) => return Ok(None),
        };
        if max_rows == 0 {
            return Ok(Some(false));
        }
        let fields = self
            .projected_fields
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let mut last = None;
        let mut aggregate_error = None;
        let visited = self
            .table
            .document_store
            .read()
            .for_each_next_fields(after, max_rows, &fields, &mut |doc_id, values| {
                last = Some(doc_id);
                let result = (|| -> ExecResult<()> {
                    if let Some(predicate) = self.predicate.as_ref() {
                        if !predicate.keep(values)? {
                            return Ok(());
                        }
                    }
                    let metadata = self.row_metadata(doc_id, 0.0);
                    let extras = [metadata.doc_id()?, metadata.score(), metadata.table_oid()];
                    let row = uqa_execution::ProjectedRow::new(
                        &self.schema,
                        &self.projected_slots,
                        values,
                        &extras,
                    );
                    executor.consume_projected_row(&row)
                })();
                if let Err(error) = result {
                    aggregate_error = Some(error);
                    return false;
                }
                true
            })
            .map_err(|error| -> uqa_execution::ExecError {
                SQLError::Internal(format!(
                    "aggregate `{}` borrowed projected documents: {error}",
                    self.table_name
                ))
                .into()
            })?;
        let Some(visited) = visited else {
            return Ok(None);
        };
        if let Some(error) = aggregate_error {
            return Err(error);
        }
        let Some(last) = last else {
            return Ok(Some(true));
        };
        let super::ScoredInputCursor::All { after } = &mut self.input else {
            unreachable!("shared aggregate input changed variants")
        };
        *after = Some(last);
        Ok(Some(visited < max_rows))
    }

    pub(super) fn next_shared_physical_batch(
        &mut self,
        max_rows: usize,
    ) -> ExecResult<Option<Vec<uqa_execution::PhysicalRow>>> {
        if self.lock_origin.is_some()
            || self.recheck_pinned
            || crate::engine_generated::projection_contains_virtual_generated_column(
                &self.column_definitions,
                &self.projected_fields,
            )
            || crate::sql::projections_use_tuple_xmin(
                &self.projected_fields,
                &self.column_definitions,
            )
        {
            return Ok(None);
        }
        let mut after = match &self.input {
            super::ScoredInputCursor::All { after } => *after,
            super::ScoredInputCursor::Entries(_) => return Ok(None),
        };
        if max_rows == 0 {
            return Ok(Some(Vec::new()));
        }
        let fields = self
            .projected_fields
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        loop {
            let shared_rows = self
                .table
                .document_store
                .read()
                .next_shared_fields(after, max_rows, &fields)
                .map_err(|error| -> uqa_execution::ExecError {
                    SQLError::Internal(format!(
                        "scan `{}` shared projected documents: {error}",
                        self.table_name
                    ))
                    .into()
                })?;
            let Some(shared_rows) = shared_rows else {
                return Ok(None);
            };
            let reached_end = shared_rows.len() < max_rows;
            let Some(last) = shared_rows.last().map(|(doc_id, _)| *doc_id) else {
                return Ok(Some(Vec::new()));
            };
            after = Some(last);
            let super::ScoredInputCursor::All { after: cursor } = &mut self.input else {
                unreachable!("shared scan input changed variants")
            };
            *cursor = after;
            let mut rows = Vec::with_capacity(shared_rows.len());
            for (doc_id, shared) in shared_rows {
                let (values, projection) = shared.indexed_values();
                let keep = self.predicate.as_ref().map_or(Ok(true), |predicate| {
                    predicate.keep_indexed(values, projection)
                })?;
                if keep {
                    rows.push(self.physical_row_from_shared(doc_id, 0.0, shared)?);
                }
            }
            if !rows.is_empty() || reached_end {
                return Ok(Some(rows));
            }
        }
    }

    fn physical_row_from_shared(
        &self,
        doc_id: DocId,
        score: f64,
        shared: uqa_storage::document_store::SharedDocumentRow,
    ) -> ExecResult<uqa_execution::PhysicalRow> {
        let (values, projection) = shared.into_parts();
        let source = uqa_execution::PhysicalRow::from_shared_values(values, projection);
        if self.appended_doc_id_attribute.is_none()
            && self.appended_score_attribute.is_none()
            && self
            .projected_slots
            .iter()
            .enumerate()
            .all(|(expected, slot)| {
                matches!(slot, uqa_execution::ProjectedValueSlot::Field(actual) if *actual == expected)
            })
        {
            return Ok(source);
        }
        let metadata = self.row_metadata(doc_id, score);
        let extras = [metadata.doc_id()?, metadata.score(), metadata.table_oid()];
        let row = source.project_with_values(self.projected_slots.iter().map(|slot| {
            match slot {
                uqa_execution::ProjectedValueSlot::Field(index) => {
                    uqa_execution::RowProjectionValue::InputSlot(*index)
                }
                uqa_execution::ProjectedValueSlot::Extra(index) => {
                    uqa_execution::RowProjectionValue::Owned(
                        extras
                            .get(*index)
                            .and_then(Option::as_ref)
                            .cloned()
                            .unwrap_or(Value::Null),
                    )
                }
                uqa_execution::ProjectedValueSlot::Missing => {
                    uqa_execution::RowProjectionValue::Owned(Value::Null)
                }
            }
        }));
        Ok(self.append_metadata_attributes(row, doc_id, score)?)
    }

    pub(super) fn materialize_entries(
        &self,
        entries: &[ScoredEntry],
    ) -> ExecResult<Vec<ResultRow>> {
        let mut rows = Vec::with_capacity(entries.len());
        self.for_each_kept_entry(entries, &mut |doc_id, entry, fields, values| {
            let mut row = fields
                .iter()
                .zip(values)
                .map(|(field, value)| ((*field).to_string(), (*value).clone()))
                .collect::<ResultRow>();
            self.row_metadata(doc_id, entry.score)
                .insert_into(&mut row)?;
            rows.push(row);
            Ok(())
        })?;
        Ok(rows)
    }

    pub(super) fn materialize_physical_entries(
        &self,
        entries: &[ScoredEntry],
    ) -> ExecResult<Vec<uqa_execution::PhysicalRow>> {
        if let Some((qualifier, storage_name)) = self.lock_origin.as_ref() {
            return self.materialize_locking_physical_entries(entries, qualifier, storage_name);
        }
        self.materialize_plain_physical_entries(entries)
    }

    fn materialize_plain_physical_entries(
        &self,
        entries: &[ScoredEntry],
    ) -> ExecResult<Vec<uqa_execution::PhysicalRow>> {
        let mut rows = Vec::with_capacity(entries.len());
        self.for_each_kept_entry(entries, &mut |doc_id, entry, _fields, values| {
            let metadata = self.row_metadata(doc_id, entry.score);
            let extras = [metadata.doc_id()?, metadata.score(), metadata.table_oid()];
            let row = uqa_execution::ProjectedRow::new(
                &self.schema,
                &self.projected_slots,
                values,
                &extras,
            );
            let row = uqa_execution::PhysicalRow::from_values(row.into_values());
            rows.push(self.append_metadata_attributes(row, doc_id, entry.score)?);
            Ok(())
        })?;
        Ok(rows)
    }

    #[inline(never)]
    fn materialize_locking_physical_entries(
        &self,
        entries: &[ScoredEntry],
        qualifier: &std::sync::Arc<str>,
        storage_name: &std::sync::Arc<str>,
    ) -> ExecResult<Vec<uqa_execution::PhysicalRow>> {
        let mut rows = Vec::with_capacity(entries.len());
        self.for_each_kept_entry(entries, &mut |doc_id, entry, _fields, values| {
            let metadata = self.row_metadata(doc_id, entry.score);
            let extras = [metadata.doc_id()?, metadata.score(), metadata.table_oid()];
            let row = uqa_execution::ProjectedRow::new(
                &self.schema,
                &self.projected_slots,
                values,
                &extras,
            );
            let row = self
                .append_metadata_attributes(
                    uqa_execution::PhysicalRow::from_values(row.into_values()),
                    doc_id,
                    entry.score,
                )?
                .with_lock_origin(uqa_execution::RowLockOrigin::from_shared(
                    std::sync::Arc::clone(qualifier),
                    std::sync::Arc::clone(storage_name),
                    doc_id,
                ));
            rows.push(row);
            Ok(())
        })?;
        Ok(rows)
    }

    fn for_each_kept_entry(
        &self,
        entries: &[ScoredEntry],
        visitor: &mut KeptEntryVisitor<'_>,
    ) -> ExecResult<()> {
        if self.recheck_pinned {
            return self.for_each_pinned_entry(entries, visitor);
        }
        self.for_each_snapshot_entry(entries, visitor)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "preserves SELECT schema and row identity"
    )]
    fn for_each_snapshot_entry(
        &self,
        entries: &[ScoredEntry],
        visitor: &mut KeptEntryVisitor<'_>,
    ) -> ExecResult<()> {
        let fields = self
            .projected_fields
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        if fields.is_empty() && self.input_guarantees_presence {
            for entry in entries {
                if let Some(predicate) = self.predicate.as_ref() {
                    if !predicate.keep(&[])? {
                        continue;
                    }
                }
                visitor(entry.doc_id, entry, &fields, &[])?;
            }
            return Ok(());
        }
        let doc_ids = entries.iter().map(|entry| entry.doc_id).collect::<Vec<_>>();
        let store = self.table.document_store.read();
        if crate::engine_generated::projection_contains_virtual_generated_column(
            &self.column_definitions,
            &self.projected_fields,
        ) || crate::sql::projections_use_tuple_xmin(
            &self.projected_fields,
            &self.column_definitions,
        ) {
            let mut documents =
                store
                    .get_stored_many(&doc_ids)
                    .map_err(|error| -> uqa_execution::ExecError {
                        SQLError::Internal(format!(
                            "read `{}` generated documents: {error}",
                            self.table_name
                        ))
                        .into()
                    })?;
            for entry in entries {
                let mut document = documents.remove(&entry.doc_id).ok_or_else(|| {
                    SQLError::Internal(format!(
                        "access path returned document {}, but table `{}` omitted it",
                        entry.doc_id, self.table_name
                    ))
                })?;
                crate::engine_generated::materialize_projected_virtual_generated_columns(
                    &self.column_definitions,
                    document.fields_mut(),
                    &self.projected_fields,
                )?;
                let values = fields
                    .iter()
                    .map(|field| {
                        crate::sql::project_stored_document_column(
                            &document,
                            field,
                            &self.column_definitions,
                        )
                    })
                    .collect::<Vec<_>>();
                let value_refs = values.iter().collect::<Vec<_>>();
                if let Some(predicate) = self.predicate.as_ref() {
                    if !predicate.keep(&value_refs)? {
                        continue;
                    }
                }
                visitor(entry.doc_id, entry, &fields, &value_refs)?;
            }
            return Ok(());
        }
        let mut index = 0usize;
        let mut materialization_error = None;
        store
            .for_each_fields_multi_ref_with_presence(
                &doc_ids,
                &fields,
                &mut |doc_id, exists, values| {
                    if !exists {
                        materialization_error = Some(
                            SQLError::Internal(format!(
                                "access path returned document {doc_id}, but table `{}` omitted it",
                                self.table_name
                            ))
                            .into(),
                        );
                        return false;
                    }
                    let Some(entry) = entries.get(index) else {
                        materialization_error = Some(
                            SQLError::Internal("document projection produced too many rows".into())
                                .into(),
                        );
                        return false;
                    };
                    index += 1;
                    if entry.doc_id != doc_id || values.len() != fields.len() {
                        materialization_error = Some(
                            SQLError::Internal(format!(
                                "document projection for `{}` lost row alignment",
                                self.table_name
                            ))
                            .into(),
                        );
                        return false;
                    }
                    if let Some(predicate) = self.predicate.as_ref() {
                        match predicate.keep(values) {
                            Ok(true) => {}
                            Ok(false) => return true,
                            Err(error) => {
                                materialization_error = Some(error.into());
                                return false;
                            }
                        }
                    }
                    if let Err(error) = visitor(doc_id, entry, &fields, values) {
                        materialization_error = Some(error);
                        return false;
                    }
                    true
                },
            )
            .map_err(|error| -> uqa_execution::ExecError {
                SQLError::Internal(format!(
                    "read `{}` projected documents: {error}",
                    self.table_name
                ))
                .into()
            })?;
        if let Some(error) = materialization_error {
            return Err(error);
        }
        if index != entries.len() {
            return Err(SQLError::Internal(format!(
                "document projection for `{}` visited {index} rows, expected {}",
                self.table_name,
                entries.len()
            ))
            .into());
        }
        Ok(())
    }

    /// Materialize tuples for a pinned tuple-local recheck scan: a changed tuple projects its latest committed image, an unchanged join partner reads the statement snapshot, and a tuple missing from the snapshot is skipped so the recheck drops the candidate naturally.
    #[cold]
    #[inline(never)]
    fn for_each_pinned_entry(
        &self,
        entries: &[ScoredEntry],
        visitor: &mut KeptEntryVisitor<'_>,
    ) -> ExecResult<()> {
        let fields = self
            .projected_fields
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let store = self.table.document_store.read();
        for entry in entries {
            let mut document = if let Some(document) = self.recheck_documents.get(&entry.doc_id) {
                (**document).clone()
            } else {
                let Some(document) = store.get_stored(entry.doc_id).map_err(
                    |error| -> uqa_execution::ExecError {
                        SQLError::Internal(format!(
                            "read `{}` rows from the pinned command snapshot: {error}",
                            self.table_name
                        ))
                        .into()
                    },
                )?
                else {
                    continue;
                };
                document
            };
            crate::engine_generated::materialize_projected_virtual_generated_columns(
                &self.column_definitions,
                document.fields_mut(),
                &self.projected_fields,
            )?;
            let values = fields
                .iter()
                .map(|field| {
                    crate::sql::project_stored_document_column(
                        &document,
                        field,
                        &self.column_definitions,
                    )
                })
                .collect::<Vec<_>>();
            let value_refs = values.iter().collect::<Vec<_>>();
            if let Some(predicate) = self.predicate.as_ref() {
                if !predicate.keep(&value_refs)? {
                    continue;
                }
            }
            visitor(entry.doc_id, entry, &fields, &value_refs)?;
        }
        Ok(())
    }

    pub(super) fn aggregate_entries(
        &self,
        entries: &[ScoredEntry],
        executor: &mut dyn uqa_execution::AggregateExecutor,
    ) -> ExecResult<()> {
        self.for_each_kept_entry(entries, &mut |doc_id, entry, _fields, values| {
            let metadata = self.row_metadata(doc_id, entry.score);
            let extras = [metadata.doc_id()?, metadata.score(), metadata.table_oid()];
            let row = uqa_execution::ProjectedRow::new(
                &self.schema,
                &self.projected_slots,
                values,
                &extras,
            );
            executor.consume_projected_row(&row)
        })
    }
}
