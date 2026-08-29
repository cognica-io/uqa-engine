//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Command-mutation-overlay table scan merging.

use super::{EngineTableRowSource, SQLError, Value};
use std::sync::Arc;

enum CommandScanCandidate {
    Persisted(uqa_core::DocId),
    Overlay(uqa_core::DocId, uqa_storage::document_store::Document),
}

impl EngineTableRowSource {
    pub(super) fn next_command_physical_rows_batch(
        &mut self,
        max_rows: usize,
    ) -> uqa_execution::ExecResult<Vec<uqa_execution::PhysicalRow>> {
        let Some(overlay_changes) = self.command_changes.as_ref().map(Arc::clone) else {
            return Err(SQLError::Internal("command scan has no mutation overlay".into()).into());
        };
        let has_virtual = crate::engine_generated::projection_contains_virtual_generated_column(
            &self.column_definitions,
            &self.columns,
        );
        let mut rows = Vec::with_capacity(max_rows);
        while rows.len() < max_rows {
            let remaining = max_rows - rows.len();
            let mut candidates = Vec::with_capacity(remaining);
            let mut exhausted = false;
            while candidates.len() < remaining {
                if self.command_base_ids.is_empty() && !self.command_base_exhausted {
                    let page_size = remaining.max(uqa_execution::DEFAULT_BATCH_SIZE);
                    let doc_ids = self
                        .table
                        .document_store
                        .read()
                        .next_doc_ids(self.command_base_after, page_size)
                        .map_err(|error| {
                            SQLError::Internal(format!(
                                "scan command-visible document ids from `{}`: {error}",
                                self.table_name
                            ))
                        })?;
                    if let Some(last) = doc_ids.last().copied() {
                        self.command_base_after = Some(last);
                        self.command_base_ids.extend(doc_ids);
                    } else {
                        self.command_base_exhausted = true;
                    }
                }
                let next_base = self.command_base_ids.front().copied();
                let change_start = self
                    .command_change_after
                    .map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded);
                let next_change = overlay_changes
                    .range((change_start, std::ops::Bound::Unbounded))
                    .next();
                match (next_base, next_change) {
                    (Some(base), Some((overlay_id, _))) if base < *overlay_id => {
                        self.command_base_ids.pop_front();
                        candidates.push(CommandScanCandidate::Persisted(base));
                    }
                    (Some(base), Some((overlay_id, document))) if base == *overlay_id => {
                        self.command_base_ids.pop_front();
                        self.command_change_after = Some(*overlay_id);
                        if let Some(document) = document {
                            candidates
                                .push(CommandScanCandidate::Overlay(*overlay_id, document.clone()));
                        }
                    }
                    (Some(_), Some((overlay_id, document))) => {
                        self.command_change_after = Some(*overlay_id);
                        if let Some(document) = document {
                            candidates
                                .push(CommandScanCandidate::Overlay(*overlay_id, document.clone()));
                        }
                    }
                    (Some(base), None) => {
                        self.command_base_ids.pop_front();
                        candidates.push(CommandScanCandidate::Persisted(base));
                    }
                    (None, Some((overlay_id, document))) => {
                        self.command_change_after = Some(*overlay_id);
                        if let Some(document) = document {
                            candidates
                                .push(CommandScanCandidate::Overlay(*overlay_id, document.clone()));
                        }
                    }
                    (None, None) => {
                        exhausted = self.command_base_exhausted;
                        break;
                    }
                }
            }
            if candidates.is_empty() {
                if exhausted {
                    break;
                }
                continue;
            }
            let persisted_ids = candidates
                .iter()
                .filter_map(|candidate| match candidate {
                    CommandScanCandidate::Persisted(doc_id) => Some(*doc_id),
                    CommandScanCandidate::Overlay(_, _) => None,
                })
                .collect::<Vec<_>>();
            let mut persisted_documents = std::collections::BTreeMap::new();
            let mut persisted_values = std::collections::BTreeMap::new();
            let mut persisted_shared = None;
            if !persisted_ids.is_empty() {
                let store = self.table.document_store.read();
                if has_virtual {
                    persisted_documents = store.get_many(&persisted_ids).map_err(|error| {
                        SQLError::Internal(format!(
                            "read command-visible generated rows from `{}`: {error}",
                            self.table_name
                        ))
                    })?;
                } else {
                    let fields = self
                        .columns
                        .iter()
                        .map(|column| {
                            crate::sql::storage_projection_column_for_table(
                                column,
                                &self.column_definitions,
                            )
                        })
                        .collect::<Vec<_>>();
                    if let Some(shared_rows) = store
                        .get_shared_fields(&persisted_ids, &fields)
                        .map_err(|error| {
                            SQLError::Internal(format!(
                                "read shared command-visible projected rows from `{}`: {error}",
                                self.table_name
                            ))
                        })?
                    {
                        if shared_rows.len() != persisted_ids.len() {
                            return Err(SQLError::Internal(format!(
                                "table `{}` returned {} shared command rows for {} document ids",
                                self.table_name,
                                shared_rows.len(),
                                persisted_ids.len()
                            ))
                            .into());
                        }
                        persisted_shared = Some(
                            persisted_ids
                                .iter()
                                .copied()
                                .zip(shared_rows)
                                .collect::<std::collections::BTreeMap<_, _>>(),
                        );
                    } else {
                        persisted_values = store
                            .get_fields_multi(&persisted_ids, &fields)
                            .map_err(|error| {
                                SQLError::Internal(format!(
                                    "read command-visible projected rows from `{}`: {error}",
                                    self.table_name
                                ))
                            })?;
                    }
                }
            }
            for candidate in candidates {
                let (doc_id, physical) = match candidate {
                    CommandScanCandidate::Persisted(doc_id) if has_virtual => {
                        let mut document = persisted_documents.remove(&doc_id).ok_or_else(|| {
                            SQLError::Internal(format!(
                                "table `{}` listed command-visible document {doc_id} but did not return it",
                                self.table_name
                            ))
                        })?;
                        crate::engine_generated::materialize_projected_virtual_generated_columns(
                            &self.column_definitions,
                            &mut document,
                            &self.columns,
                        )?;
                        let values = self
                            .columns
                            .iter()
                            .map(|column| {
                                document
                                    .get(crate::sql::storage_projection_column_for_table(
                                        column,
                                        &self.column_definitions,
                                    ))
                                    .cloned()
                                    .unwrap_or(Value::Null)
                            })
                            .collect();
                        (doc_id, uqa_execution::PhysicalRow::from_values(values))
                    }
                    CommandScanCandidate::Persisted(doc_id) => {
                        let physical = if let Some(shared_rows) = persisted_shared.as_mut() {
                            match shared_rows.remove(&doc_id).flatten() {
                                Some(shared) => {
                                    let (values, projection) = shared.into_parts();
                                    uqa_execution::PhysicalRow::from_shared_values(
                                        values, projection,
                                    )
                                }
                                None => uqa_execution::PhysicalRow::nulls(self.columns.len()),
                            }
                        } else {
                            uqa_execution::PhysicalRow::from_values(
                                persisted_values
                                    .remove(&doc_id)
                                    .unwrap_or_else(|| vec![Value::Null; self.columns.len()]),
                            )
                        };
                        (doc_id, physical)
                    }
                    CommandScanCandidate::Overlay(doc_id, mut document) => {
                        crate::engine_generated::materialize_projected_virtual_generated_columns(
                            &self.column_definitions,
                            &mut document,
                            &self.columns,
                        )?;
                        let values = self
                            .columns
                            .iter()
                            .map(|column| {
                                document
                                    .get(crate::sql::storage_projection_column_for_table(
                                        column,
                                        &self.column_definitions,
                                    ))
                                    .cloned()
                                    .unwrap_or(Value::Null)
                            })
                            .collect();
                        (doc_id, uqa_execution::PhysicalRow::from_values(values))
                    }
                };
                if let Some(predicate) = self.predicate.as_ref() {
                    if !predicate.keep_row(&self.physical_schema.view(&physical))? {
                        continue;
                    }
                }
                rows.push(self.with_lock_identity(physical, doc_id)?);
            }
            if exhausted {
                break;
            }
        }
        Ok(rows)
    }
}
