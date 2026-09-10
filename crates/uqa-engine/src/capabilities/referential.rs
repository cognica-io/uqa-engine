//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind referential execution to transaction queues and mutation snapshots.
use crate::Engine;
use uqa_core::DocId;
use uqa_execution::mutation::referential::{ReferentialContext, ReferentialDeferrals};
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
