//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Latest reference scans, own-write overlays, and fixed-snapshot visibility checks.

use super::{ReferentialContext, ReferentialReadSnapshot, ReferentialSnapshots};
use crate::mutation::constraints::context::{MutationIndexRead, MutationRead};
use crate::mutation::constraints::index_keys::changes_error;
use crate::mutation::row_membership::{PhysicalRowBuffer, PhysicalRows};
use crate::query::document_changes::DocumentChanges;
use uqa_core::{CancellationToken, DocId, Value};
use uqa_sql::SQLError;
use uqa_storage::document_store::Document;

#[cfg(test)]
mod tests;

pub(super) struct ReferenceSnapshot<'a> {
    current: &'a dyn MutationRead,
    indexes: &'a dyn MutationIndexRead,
    snapshots: &'a dyn ReferentialSnapshots,
    latest: Option<Box<dyn ReferentialReadSnapshot + 'a>>,
}

impl<'a> ReferenceSnapshot<'a> {
    pub fn new<S: Clone + 'static>(context: &ReferentialContext<'a, S>) -> Result<Self, SQLError> {
        // A child may commit immediately before the parent lock is granted without making that acquisition wait. Every reference scan therefore starts after this visibility boundary.
        context
            .constraints
            .transactions
            .refresh_explicit_statement_snapshot()?;
        Ok(Self {
            current: context.constraints.reads,
            indexes: context.constraints.indexes,
            snapshots: context.snapshots,
            latest: context
                .locking
                .session
                .uses_fixed_snapshot()
                .then(|| context.snapshots.latest_reference_snapshot())
                .transpose()?,
        })
    }

    pub fn table(&self, table: &str) -> Result<ReferenceTableSnapshot<'_>, SQLError> {
        Ok(ReferenceTableSnapshot {
            current: self.current,
            indexes: self.indexes,
            snapshots: self.snapshots,
            latest: self.latest.as_deref(),
            table: table.to_string(),
            changes: self
                .current
                .command_overlay_changes(table)?
                .unwrap_or_default(),
        })
    }
}

pub(super) struct ReferenceTableSnapshot<'a> {
    current: &'a dyn MutationRead,
    indexes: &'a dyn MutationIndexRead,
    snapshots: &'a dyn ReferentialSnapshots,
    latest: Option<&'a dyn ReferentialReadSnapshot>,
    table: String,
    changes: DocumentChanges,
}

impl ReferenceTableSnapshot<'_> {
    /// Candidate identities from an eligible child index, with private replacements masking the same identities in the committed generation. Full FK comparison still rechecks every selected row.
    pub fn indexed_doc_ids(
        &self,
        columns: &[String],
        values: &[Value],
        bytes: usize,
        cancellation: &CancellationToken,
    ) -> Result<Option<PhysicalRows>, SQLError> {
        let buffer = || PhysicalRowBuffer::new(bytes, cancellation.clone(), "reference candidates");
        for (column, value) in columns.iter().zip(values) {
            let key = uqa_storage::ValueIndexKey::Column(column.clone());
            let mut current = buffer();
            if !self
                .indexes
                .visit_value_index_equal(&self.table, &key, value, &mut |id| {
                    current.insert("", id)
                })?
            {
                continue;
            }
            let current = current.finish()?;
            let latest = if let Some(latest) = self.latest {
                let mut ids = buffer();
                if !latest.visit_value_index_equal(&self.table, &key, value, &mut |id| {
                    ids.insert("", id)
                })? {
                    continue;
                }
                Some(ids.finish()?)
            } else {
                None
            };
            let mut ids = buffer();
            if let Some(latest) = &latest {
                latest.visit(&mut |_, id| {
                    if !self.changes.contains_change(id).map_err(changes_error)? {
                        ids.insert("", id)?;
                    }
                    Ok(true)
                })?;
            }
            current.visit(&mut |_, id| {
                let changed = self.changes.contains_change(id).map_err(changes_error)?;
                if changed {
                    if self.changes.row_matches(
                        id,
                        columns,
                        values,
                        crate::query::exact_lookup::FieldPresence::MissingIsNull,
                    )? {
                        ids.insert("", id)?;
                    }
                } else if latest.is_none() {
                    ids.insert("", id)?;
                }
                Ok(true)
            })?;
            self.indexes
                .visit_staged_matches(&self.table, columns, values, &mut |id| ids.insert("", id))?;
            return Ok(Some(ids.finish()?));
        }
        Ok(None)
    }

    pub fn visit_doc_ids(
        &self,
        bytes: usize,
        cancellation: &CancellationToken,
        visit: &mut dyn FnMut(DocId) -> Result<bool, SQLError>,
    ) -> Result<(), SQLError> {
        let control = uqa_storage::read_control::StorageReadControl::new(
            &uqa_core::memory::MemoryBudget::new(64 * 1024),
            cancellation,
        );
        let mut ids = PhysicalRowBuffer::new(bytes, cancellation.clone(), "reference candidates");
        let mut after = None;
        loop {
            let page = match self.latest {
                Some(latest) => latest.doc_id_page(&self.table, after, 128, &control)?,
                None => self
                    .current
                    .table_doc_id_page(&self.table, after, 128, &control)?,
            };
            let Some(last) = page.last().copied() else {
                break;
            };
            after = Some(last);
            for &id in &*page {
                if self.latest.is_none()
                    || !self.changes.contains_change(id).map_err(changes_error)?
                {
                    ids.insert("", id)?;
                }
            }
        }
        if self.latest.is_some() {
            for change in self.changes.changes() {
                let (id, _) = change.map_err(changes_error)?;
                if self.current.get_document(&self.table, id)?.is_some() {
                    ids.insert("", id)?;
                }
            }
        }
        ids.finish()?.visit(&mut |_, id| visit(id))
    }

    pub fn document(&self, doc_id: DocId) -> Result<Option<Document>, SQLError> {
        if let Some(latest) = self.latest {
            if !self
                .changes
                .contains_change(doc_id)
                .map_err(changes_error)?
            {
                return latest.document(&self.table, doc_id);
            }
        }
        self.current.get_document(&self.table, doc_id)
    }

    /// A fixed snapshot must not silently accept a matching version committed after it. Own command and transaction changes are visible to their referential actions.
    pub fn check_visible(&self, doc_id: DocId) -> Result<(), SQLError> {
        let Some(latest) = self.latest else {
            return Ok(());
        };
        if self
            .changes
            .contains_change(doc_id)
            .map_err(changes_error)?
        {
            return Ok(());
        }
        if latest.metadata(&self.table, doc_id)?
            != self
                .snapshots
                .transaction_document_metadata(&self.table, doc_id)?
        {
            return Err(SQLError::Routine {
                sqlstate: "40001".into(),
                message: "could not serialize access due to concurrent update".into(),
            });
        }
        Ok(())
    }
}
