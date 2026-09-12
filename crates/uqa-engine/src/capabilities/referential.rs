//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind referential execution to transaction queues and mutation snapshots.
use crate::Engine;
use uqa_core::DocId;
use uqa_execution::mutation::referential::{
    ReferentialContext, ReferentialDeferrals, ReferentialReadSnapshot, ReferentialSnapshots,
};
use uqa_sql::{ast::ForeignKey, SQLError};
impl Engine {
    pub(crate) fn referential_execution_context(
        &self,
    ) -> ReferentialContext<'_, crate::session::StatementReadSnapshot> {
        ReferentialContext {
            constraints: self.constraint_execution_context(),
            locking: self.row_lock_context(),
            assignment: self.mutation_assignment_context(),
            identifiers: self,
            triggers: self.trigger_execution_context(),
            deferrals: self,
            snapshots: self,
        }
    }
}
impl ReferentialDeferrals for Engine {
    fn defer_foreign_key_check(
        &self,
        constraint_table: &str,
        firing_table: &str,
        row_table: &str,
        doc_id: DocId,
        foreign_key: &ForeignKey,
    ) -> Result<(), SQLError> {
        Engine::defer_foreign_key_check(
            self,
            constraint_table,
            firing_table,
            row_table,
            doc_id,
            foreign_key,
        )
    }
    fn defer_foreign_key_parent_event(
        &self,
        constraint_table: &str,
        firing_table: &str,
        foreign_key: &ForeignKey,
    ) -> Result<(), SQLError> {
        Engine::defer_foreign_key_parent_event(self, constraint_table, firing_table, foreign_key)
    }
}

/// Own or borrow the existing snapshot resource; referential scan policy belongs to execution.
struct ReferenceSnapshotEngine<T: std::ops::Deref<Target = Engine>>(T);

impl<T: std::ops::Deref<Target = Engine>> ReferentialReadSnapshot for ReferenceSnapshotEngine<T> {
    fn doc_ids(&self, table: &str) -> Result<Vec<DocId>, SQLError> {
        self.0.live_table_doc_ids(table)
    }
    fn document(
        &self,
        table: &str,
        doc_id: DocId,
    ) -> Result<Option<uqa_storage::document_store::Document>, SQLError> {
        self.0.get_document(table, doc_id)
    }
    fn metadata(
        &self,
        table: &str,
        doc_id: DocId,
    ) -> Result<Option<uqa_storage::DocumentMetadata>, SQLError> {
        self.0
            .require_table(table)?
            .document_store
            .read()
            .get_metadata(doc_id)
            .map_err(|error| SQLError::Internal(format!("read reference tuple metadata: {error}")))
    }
}
impl ReferentialSnapshots for Engine {
    fn latest_reference_snapshot(&self) -> Result<Box<dyn ReferentialReadSnapshot + '_>, SQLError> {
        let Some(backend) = self.storage.backend.as_ref() else {
            return Ok(Box::new(ReferenceSnapshotEngine(self)));
        };
        if backend.transaction_has_written().map_err(|error| {
            SQLError::Internal(format!("inspect reference snapshot transaction: {error}"))
        })? {
            Ok(Box::new(ReferenceSnapshotEngine(self)))
        } else {
            Ok(Box::new(ReferenceSnapshotEngine(
                self.open_independent_pinned_read_snapshot()?,
            )))
        }
    }
    fn transaction_document_metadata(
        &self,
        table: &str,
        doc_id: DocId,
    ) -> Result<Option<uqa_storage::DocumentMetadata>, SQLError> {
        self.get_query_document_metadata(table, doc_id)
    }
}
