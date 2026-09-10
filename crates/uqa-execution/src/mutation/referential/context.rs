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
}
impl<S: Clone + 'static> Copy for ReferentialContext<'_, S> {}
