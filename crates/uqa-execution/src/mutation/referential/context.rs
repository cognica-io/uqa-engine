//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capabilities for referential action preparation and tuple rechecks.
use crate::{
    mutation::{
        assignment::MutationAssignmentContext, constraints::ConstraintContext,
        identity::MutationIdentifiers, triggers::context::TriggerContext,
    },
    query::locking::context::RowLockContext,
};
use uqa_core::DocId;
use uqa_sql::{ast::ForeignKey, SQLError};
/// Retained latest row images used only for referential checks under a fixed transaction snapshot.
pub trait ReferentialReadSnapshot {
    fn doc_ids(&self, table: &str) -> Result<Vec<DocId>, SQLError>;
    fn document(
        &self,
        table: &str,
        doc_id: DocId,
    ) -> Result<Option<uqa_storage::document_store::Document>, SQLError>;
    fn metadata(
        &self,
        table: &str,
        doc_id: DocId,
    ) -> Result<Option<uqa_storage::DocumentMetadata>, SQLError>;
}
pub trait ReferentialSnapshots {
    fn latest_reference_snapshot(&self) -> Result<Box<dyn ReferentialReadSnapshot + '_>, SQLError>;
    fn transaction_document_metadata(
        &self,
        table: &str,
        doc_id: DocId,
    ) -> Result<Option<uqa_storage::DocumentMetadata>, SQLError>;
}
pub trait ReferentialDeferrals {
    fn defer_foreign_key_check(
        &self,
        constraint_table: &str,
        firing_table: &str,
        row_table: &str,
        doc_id: DocId,
        foreign_key: &ForeignKey,
    ) -> Result<(), SQLError>;
    fn defer_foreign_key_parent_event(
        &self,
        constraint_table: &str,
        firing_table: &str,
        foreign_key: &ForeignKey,
    ) -> Result<(), SQLError>;
}
#[derive(Clone)]
pub struct ReferentialContext<'a, S: Clone + 'static> {
    pub constraints: ConstraintContext<'a>,
    pub locking: RowLockContext<'a, S>,
    pub assignment: MutationAssignmentContext<'a, S>,
    pub identifiers: &'a dyn MutationIdentifiers,
    pub triggers: TriggerContext<'a>,
    pub deferrals: &'a dyn ReferentialDeferrals,
    pub snapshots: &'a dyn ReferentialSnapshots,
}
impl<S: Clone + 'static> Copy for ReferentialContext<'_, S> {}
