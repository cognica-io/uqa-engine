//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Command-mutation-overlay table scan merging.

use super::{LocalTableRowSource, SQLError};
use crate::query::document_changes::DocumentChanges;
use crate::query::document_projection::{projections_use_tuple_xmin, read_document_projection};
use crate::storage_errors::storage_error;
use crate::RowSchemaExecution;
use std::collections::BTreeMap;
use uqa_core::DocId;
use uqa_storage::DocumentStore;

#[cfg(test)]
mod tests;

enum CommandScanCandidate {
    Persisted(DocId),
    Overlay(DocId),
}

impl LocalTableRowSource {
    pub(super) fn next_command_physical_rows_batch(
        &mut self,
        max_rows: usize,
    ) -> crate::ExecResult<Vec<crate::PhysicalRow>> {
        let Some(changes) = self.command_changes.clone() else {
            return Err(SQLError::Internal("command scan has no mutation overlay".into()).into());
        };
        let mut rows = Vec::with_capacity(max_rows);
        while rows.len() < max_rows {
            self.cancellation.check().map_err(SQLError::from)?;
            let candidates = self.next_command_candidates(max_rows - rows.len(), &changes)?;
            if candidates.is_empty() {
                break;
            }
            let mut persisted_ids = Vec::new();
            let mut private_ids = Vec::new();
            for candidate in &candidates {
                match candidate {
                    CommandScanCandidate::Persisted(id) => persisted_ids.push(*id),
                    CommandScanCandidate::Overlay(id) => private_ids.push(*id),
                }
            }
            let mut persisted =
                self.command_projected_rows(&**self.table.read_documents(), &persisted_ids)?;
            let mut private = self.command_projected_rows(&changes, &private_ids)?;
            for candidate in candidates {
                let (id, physical) = match candidate {
                    CommandScanCandidate::Persisted(id) => (id, persisted.remove(&id)),
                    CommandScanCandidate::Overlay(id) => (id, private.remove(&id)),
                };
                let physical =
                    physical.unwrap_or_else(|| crate::PhysicalRow::nulls(self.columns.len()));
                if let Some(predicate) = self.predicate.as_ref() {
                    if !predicate.keep_row(&self.physical_schema.view(&physical))? {
                        continue;
                    }
                }
                rows.push(self.with_lock_identity(physical, id)?);
            }
        }
        Ok(rows)
    }

    fn next_command_candidates(
        &mut self,
        limit: usize,
        changes: &DocumentChanges,
    ) -> Result<Vec<CommandScanCandidate>, SQLError> {
        let mut candidates = Vec::with_capacity(limit);
        while candidates.len() < limit {
            self.cancellation.check()?;
            if self.command_base_ids.is_empty() && !self.command_base_exhausted {
                let ids = self
                    .table
                    .read_documents()
                    .next_doc_ids(
                        self.command_base_after,
                        limit.max(crate::DEFAULT_BATCH_SIZE),
                    )
                    .map_err(|error| storage_error("scan command-visible document ids", &error))?;
                if let Some(last) = ids.last().copied() {
                    self.command_base_after = Some(last);
                    self.command_base_ids.extend(ids);
                } else {
                    self.command_base_exhausted = true;
                }
            }
            let next_base = self.command_base_ids.front().copied();
            let next_change = changes.changes_after(self.command_change_after).next();
            match (next_base, next_change) {
                (Some(base), Some((id, _))) if base < id => {
                    self.command_base_ids.pop_front();
                    candidates.push(CommandScanCandidate::Persisted(base));
                }
                (_, Some((id, present))) => {
                    if next_base == Some(id) {
                        self.command_base_ids.pop_front();
                    }
                    self.command_change_after = Some(id);
                    if present {
                        candidates.push(CommandScanCandidate::Overlay(id));
                    }
                }
                (Some(base), None) => {
                    self.command_base_ids.pop_front();
                    candidates.push(CommandScanCandidate::Persisted(base));
                }
                (None, None) => break,
            }
        }
        Ok(candidates)
    }

    fn command_projected_rows(
        &self,
        source: &dyn DocumentStore,
        ids: &[DocId],
    ) -> Result<BTreeMap<DocId, crate::PhysicalRow>, SQLError> {
        if ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        let fields = self.columns.iter().map(String::as_str).collect::<Vec<_>>();
        let computed = crate::query::generated::projection_contains_virtual_generated_column(
            &self.column_definitions,
            &self.columns,
        ) || projections_use_tuple_xmin(&self.columns, &self.column_definitions);
        if !computed {
            if let Some(shared) = source
                .get_shared_fields(ids, &fields)
                .map_err(|error| storage_error("read shared command-visible projection", &error))?
            {
                if shared.len() != ids.len() {
                    return Err(SQLError::Internal(format!(
                        "table `{}` returned {} shared command rows for {} document ids",
                        self.table_name,
                        shared.len(),
                        ids.len(),
                    )));
                }
                return Ok(ids
                    .iter()
                    .copied()
                    .zip(shared)
                    .filter_map(|(id, row)| {
                        row.map(|row| {
                            let (values, projection) = row.into_parts();
                            (
                                id,
                                crate::PhysicalRow::from_shared_values(values, projection),
                            )
                        })
                    })
                    .collect());
            }
        }
        let projected = read_document_projection(source, ids, &fields, &self.column_definitions)?;
        if computed {
            if let Some(missing) = ids.iter().find(|id| !projected.contains_key(id)) {
                return Err(SQLError::Internal(format!(
                    "table `{}` listed command-visible document {missing} but did not return it",
                    self.table_name,
                )));
            }
        }
        Ok(projected
            .into_iter()
            .map(|(id, values)| (id, crate::PhysicalRow::from_values(values)))
            .collect())
    }
}
