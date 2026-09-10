//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
//! Physical row production for persisted, generated, pinned, and command-overlay table rows.

use super::{Arc, LocalTableRowSource, ResultRow, SQLError, SharedLockOrigin, Value};
use crate::catalog::{CatalogReadView, RelationNameResolution};
use crate::RowSchemaExecution;
use uqa_sql::semantics::doc_id_value;

pub fn table_lock_origin(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    table: &str,
    qualifier: &str,
    enabled: bool,
) -> Result<Option<SharedLockOrigin>, SQLError> {
    if !enabled {
        return Ok(None);
    }
    let storage_name = catalog
        .table_name_resolved(resolution, table)?
        .unwrap_or_else(|| table.to_string());
    Ok(Some((
        Arc::<str>::from(qualifier),
        Arc::<str>::from(storage_name),
    )))
}

impl LocalTableRowSource {
    #[expect(
        clippy::too_many_lines,
        reason = "preserves source schema and row identity"
    )]
    pub(super) fn next_physical_rows_batch(
        &mut self,
        max_rows: usize,
    ) -> crate::ExecResult<Vec<crate::PhysicalRow>> {
        self.cancellation.check().map_err(SQLError::from)?;
        if max_rows == 0 {
            return Ok(Vec::new());
        }
        if self.recheck_pins.is_some() {
            return self.next_pinned_physical_rows_batch(max_rows);
        }
        if self.command_changes.is_some() {
            return self.next_command_physical_rows_batch(max_rows);
        }
        if crate::query::generated::projection_contains_virtual_generated_column(
            &self.column_definitions,
            &self.columns,
        ) || crate::query::document_projection::projections_use_tuple_xmin(
            &self.columns,
            &self.column_definitions,
        ) {
            return self.next_stored_physical_rows_batch(max_rows);
        }
        let store = self.table.read_documents();
        let fields = self.columns.iter().map(String::as_str).collect::<Vec<_>>();
        let mut rows = Vec::with_capacity(max_rows);
        loop {
            self.cancellation.check().map_err(SQLError::from)?;
            // A source must not return an empty batch before end-of-stream: TableScan treats it as EOF. Keep advancing storage pages when a pushed predicate rejects an entire page, and fill the requested output batch when selectivity permits it.
            let remaining = max_rows - rows.len();
            if remaining == 0 {
                break;
            }
            let direct_shared = store
                .next_shared_fields(self.after, remaining, &fields)
                .map_err(|error| {
                    SQLError::Internal(format!(
                        "scan shared projected fields from `{}`: {error}",
                        self.table_name
                    ))
                })?;
            if let Some(shared_rows) = direct_shared {
                let Some(last) = shared_rows.last().map(|(doc_id, _)| *doc_id) else {
                    break;
                };
                self.after = Some(last);
                for (doc_id, shared) in shared_rows {
                    let keep = shared.with_projected(|projected| {
                        self.predicate
                            .as_ref()
                            .map_or(Ok(true), |predicate| predicate.keep(projected))
                    })?;
                    if keep {
                        let (values, projection) = shared.into_parts();
                        rows.push(self.with_lock_identity(
                            crate::PhysicalRow::from_shared_values(values, projection),
                            doc_id,
                        )?);
                    }
                }
                continue;
            }
            let doc_ids = store.next_doc_ids(self.after, remaining).map_err(|error| {
                SQLError::Internal(format!(
                    "scan document ids for `{}`: {error}",
                    self.table_name
                ))
            })?;
            let Some(last) = doc_ids.last().copied() else {
                break;
            };
            self.after = Some(last);

            let shared_rows = store
                .get_shared_fields(&doc_ids, &fields)
                .map_err(|error| {
                    SQLError::Internal(format!(
                        "read shared projected fields from `{}`: {error}",
                        self.table_name
                    ))
                })?;
            if let Some(shared_rows) = shared_rows {
                if shared_rows.len() != doc_ids.len() {
                    return Err(SQLError::Internal(format!(
                        "table `{}` returned {} shared rows for {} document ids",
                        self.table_name,
                        shared_rows.len(),
                        doc_ids.len()
                    ))
                    .into());
                }
                let null = Value::Null;
                for (doc_id, shared) in doc_ids.iter().copied().zip(shared_rows) {
                    let keep = if let Some(shared) = shared.as_ref() {
                        shared.with_projected(|projected| {
                            self.predicate
                                .as_ref()
                                .map_or(Ok(true), |predicate| predicate.keep(projected))
                        })?
                    } else {
                        let projected = vec![&null; fields.len()];
                        self.predicate
                            .as_ref()
                            .map_or(Ok(true), |predicate| predicate.keep(&projected))?
                    };
                    if !keep {
                        continue;
                    }
                    rows.push(self.with_lock_identity(
                        match shared {
                            Some(shared) => {
                                let (values, projection) = shared.into_parts();
                                crate::PhysicalRow::from_shared_values(values, projection)
                            }
                            None => crate::PhysicalRow::nulls(fields.len()),
                        },
                        doc_id,
                    )?);
                }
            } else {
                let mut visited = 0usize;
                let mut predicate_error = None;
                store
                    .for_each_fields_multi_ref(&doc_ids, &fields, &mut |doc_id, values| {
                        visited += 1;
                        if let Some(predicate) = self.predicate.as_ref() {
                            match predicate.keep(values) {
                                Ok(true) => {}
                                Ok(false) => return true,
                                Err(error) => {
                                    predicate_error = Some(error);
                                    return false;
                                }
                            }
                        }
                        match self.with_lock_identity(
                            crate::PhysicalRow::from_values(
                                values.iter().map(|value| (*value).clone()).collect(),
                            ),
                            doc_id,
                        ) {
                            Ok(row) => rows.push(row),
                            Err(error) => {
                                predicate_error = Some(error);
                                return false;
                            }
                        }
                        true
                    })
                    .map_err(|error| {
                        SQLError::Internal(format!(
                            "read projected fields from `{}`: {error}",
                            self.table_name
                        ))
                    })?;
                if let Some(error) = predicate_error {
                    return Err(error.into());
                }
                if visited != doc_ids.len() {
                    return Err(SQLError::Internal(format!(
                        "table `{}` visited {visited} of {} projected cursor rows",
                        self.table_name,
                        doc_ids.len()
                    ))
                    .into());
                }
            }
        }
        Ok(rows)
    }

    /// Emit exactly the candidate tuples pinned for a tuple-local row-lock recheck: the latest committed image for a changed tuple, or the statement-snapshot image for an unchanged join partner. Pushed predicates re-apply to the substituted values, matching `PostgreSQL`'s `EvalPlanQual` scan behavior for marked relations.
    fn next_pinned_physical_rows_batch(
        &mut self,
        max_rows: usize,
    ) -> crate::ExecResult<Vec<crate::PhysicalRow>> {
        let Some(pins) = self.recheck_pins.as_ref() else {
            return Err(
                SQLError::Internal("row-lock recheck scan has no pinned tuples".into()).into(),
            );
        };
        let pins = Arc::clone(pins);
        let store = self.table.read_documents();
        let mut rows = Vec::with_capacity(max_rows.min(pins.len()));
        while rows.len() < max_rows && self.recheck_cursor < pins.len() {
            self.cancellation.check().map_err(SQLError::from)?;
            let pin = &pins[self.recheck_cursor];
            self.recheck_cursor += 1;
            let mut document = if let Some(document) = pin.document.as_ref() {
                (**document).clone()
            } else {
                let Some(document) = store.get_stored(pin.doc_id).map_err(|error| {
                    SQLError::Internal(format!(
                        "read pinned recheck row from `{}`: {error}",
                        self.table_name
                    ))
                })?
                else {
                    continue;
                };
                document
            };
            crate::query::generated::materialize_projected_virtual_generated_columns(
                &self.column_definitions,
                document.fields_mut(),
                &self.columns,
            )?;
            let values = self
                .columns
                .iter()
                .map(|column| {
                    crate::query::document_projection::project_stored_document_column(
                        &document,
                        column,
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
            rows.push(
                self.with_lock_identity(crate::PhysicalRow::from_values(values), pin.doc_id)?,
            );
        }
        Ok(rows)
    }

    fn next_stored_physical_rows_batch(
        &mut self,
        max_rows: usize,
    ) -> crate::ExecResult<Vec<crate::PhysicalRow>> {
        let store = self.table.read_documents();
        let mut rows = Vec::with_capacity(max_rows);
        while rows.len() < max_rows {
            self.cancellation.check().map_err(SQLError::from)?;
            let remaining = max_rows - rows.len();
            let doc_ids = store.next_doc_ids(self.after, remaining).map_err(|error| {
                SQLError::Internal(format!(
                    "scan generated rows from `{}`: {error}",
                    self.table_name
                ))
            })?;
            let Some(last) = doc_ids.last().copied() else {
                break;
            };
            self.after = Some(last);
            let mut documents = store.get_stored_many(&doc_ids).map_err(|error| {
                SQLError::Internal(format!(
                    "read rows with metadata from `{}`: {error}",
                    self.table_name
                ))
            })?;
            for doc_id in doc_ids {
                let mut document = documents.remove(&doc_id).ok_or_else(|| {
                    SQLError::Internal(format!(
                        "table `{}` listed document {doc_id} but did not return it",
                        self.table_name
                    ))
                })?;
                crate::query::generated::materialize_projected_virtual_generated_columns(
                    &self.column_definitions,
                    document.fields_mut(),
                    &self.columns,
                )?;
                let values = self
                    .columns
                    .iter()
                    .map(|column| {
                        crate::query::document_projection::project_stored_document_column(
                            &document,
                            column,
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
                rows.push(
                    self.with_lock_identity(crate::PhysicalRow::from_values(values), doc_id)?,
                );
            }
        }
        Ok(rows)
    }

    pub(super) fn with_lock_identity(
        &self,
        mut row: crate::PhysicalRow,
        doc_id: uqa_core::DocId,
    ) -> Result<crate::PhysicalRow, SQLError> {
        if let Some(table_oid) = self.table_oid.as_ref() {
            row = row.append_values(vec![table_oid.clone()]);
        }
        let mut metadata = Vec::with_capacity(2);
        if self.metadata.includes_doc_id() {
            metadata.push(doc_id_value(doc_id)?);
        }
        if self.metadata.includes_score() {
            metadata.push(Value::Float(0.0));
        }
        if !metadata.is_empty() {
            row = row.append_values(metadata);
        }
        let Some((qualifier, storage_name)) = self.lock_origin.as_ref() else {
            return Ok(row);
        };
        Ok(row.with_lock_origin(crate::RowLockOrigin::from_shared(
            std::sync::Arc::clone(qualifier),
            std::sync::Arc::clone(storage_name),
            doc_id,
        )))
    }
}

impl crate::RowSource for LocalTableRowSource {
    fn schema(&self) -> &[String] {
        &self.schema
    }

    fn physical_schema(&self) -> Option<&crate::RowSchema> {
        Some(&self.physical_schema)
    }

    fn estimated_cardinality(&self) -> Option<u64> {
        Some(self.estimated_cardinality)
    }

    fn next_row(&mut self) -> crate::ExecResult<Option<ResultRow>> {
        Ok(self.next_batch(1)?.pop())
    }

    fn next_batch(&mut self, max_rows: usize) -> crate::ExecResult<Vec<ResultRow>> {
        let rows = self.next_physical_rows_batch(max_rows)?;
        Ok(rows
            .iter()
            .map(|row| self.physical_schema.view(row).to_result_row())
            .collect())
    }

    fn next_physical_batch(
        &mut self,
        max_rows: usize,
    ) -> crate::ExecResult<Vec<crate::PhysicalRow>> {
        self.next_physical_rows_batch(max_rows)
    }
}
