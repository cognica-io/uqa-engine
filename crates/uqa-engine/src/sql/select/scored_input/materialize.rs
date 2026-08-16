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
        let mut rows = Vec::with_capacity(entries.len());
        self.for_each_kept_entry(entries, &mut |doc_id, entry, _fields, values| {
            let metadata = self.row_metadata(doc_id, entry.score);
            let extras = [
                metadata.doc_id()?,
                metadata.score(),
                metadata.score_provenance(),
            ];
            let row = uqa_execution::ProjectedRow::new(
                &self.schema,
                &self.projected_slots,
                values,
                &extras,
            );
            rows.push(uqa_execution::PhysicalRow::from_values(row.into_values()));
            Ok(())
        })?;
        Ok(rows)
    }

    fn for_each_kept_entry(
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
        ) {
            let mut documents =
                store
                    .get_many(&doc_ids)
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
                    &mut document,
                    &self.projected_fields,
                )?;
                let values = fields
                    .iter()
                    .map(|field| document.get(*field).cloned().unwrap_or(Value::Null))
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

    pub(super) fn aggregate_entries(
        &self,
        entries: &[ScoredEntry],
        executor: &mut dyn uqa_execution::AggregateExecutor,
    ) -> ExecResult<()> {
        self.for_each_kept_entry(entries, &mut |doc_id, entry, _fields, values| {
            let metadata = self.row_metadata(doc_id, entry.score);
            let extras = [
                metadata.doc_id()?,
                metadata.score(),
                metadata.score_provenance(),
            ];
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
