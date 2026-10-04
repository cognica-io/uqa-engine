//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Latest reference scans, own-write overlays, and fixed-snapshot visibility checks.

use super::{ReferentialContext, ReferentialReadSnapshot, ReferentialSnapshots};
use crate::mutation::constraints::context::MutationRead;
use crate::mutation::constraints::index_keys::changes_error;
use crate::query::document_changes::DocumentChanges;
use uqa_core::DocId;
use uqa_sql::SQLError;
use uqa_storage::document_store::Document;

pub(super) struct ReferenceSnapshot<'a> {
    current: &'a dyn MutationRead,
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
    snapshots: &'a dyn ReferentialSnapshots,
    latest: Option<&'a dyn ReferentialReadSnapshot>,
    table: String,
    changes: DocumentChanges,
}

impl ReferenceTableSnapshot<'_> {
    pub fn doc_ids(&self) -> Result<Vec<DocId>, SQLError> {
        let Some(latest) = self.latest else {
            return self.current.table_doc_ids(&self.table);
        };
        let mut ids = latest
            .doc_ids(&self.table)?
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        for change in self.changes.changes() {
            let (doc_id, _) = change.map_err(changes_error)?;
            if self.current.get_document(&self.table, doc_id)?.is_some() {
                ids.insert(doc_id);
            } else {
                ids.remove(&doc_id);
            }
        }
        Ok(ids.into_iter().collect())
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
