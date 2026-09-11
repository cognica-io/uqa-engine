//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lend portal registry state and capture RETURNING inputs only when requested.

use crate::{session::StatementReadSnapshot, Engine};
use uqa_execution::statement::{
    context::session::StatementPortals,
    portal::{
        context::{PortalExecutionContext, PortalReturningInputs},
        SessionPortalCommandDeclaration, SessionPortalDeclaration,
    },
};
use uqa_sql::{ast::FetchCursorStmt, SQLError, SQLResult};

impl Engine {
    pub(crate) fn portal_execution_context(
        &self,
    ) -> PortalExecutionContext<'_, StatementReadSnapshot> {
        PortalExecutionContext {
            state: self,
            queries: self,
            returning: self,
            command_scopes: self,
            routines: self,
            overloads: self,
            types: self,
            session: self,
            explain: uqa_planner::explain::run_explain,
        }
    }
}

impl PortalReturningInputs<StatementReadSnapshot> for Engine {
    fn returning_execution_context(
        &self,
    ) -> uqa_execution::mutation::returning::ReturningExecutionContext<'_, StatementReadSnapshot>
    {
        Engine::returning_execution_context(self)
    }
    fn returning_analysis_context(
        &self,
    ) -> uqa_sql::semantics::returning::ReturningAnalysisContext<'_> {
        Engine::returning_analysis_context(self)
    }
}

impl StatementPortals for Engine {
    fn in_transaction_block(&self) -> bool {
        Engine::in_transaction_block(self)
    }
    fn ensure_session_portal_available(&self, name: &str) -> Result<(), SQLError> {
        Engine::ensure_session_portal_available(self, name)
    }
    fn open_pending_session_portal(
        &self,
        declaration: SessionPortalDeclaration,
    ) -> Result<(), SQLError> {
        Engine::open_pending_session_portal(self, declaration)
    }
    fn open_pending_command_session_portal(
        &self,
        declaration: SessionPortalCommandDeclaration,
    ) -> Result<(), SQLError> {
        Engine::open_pending_command_session_portal(self, declaration)
    }
    fn fetch_session_portal(&self, fetch: &FetchCursorStmt) -> Result<SQLResult, SQLError> {
        Engine::fetch_session_portal(self, fetch)
    }
    fn close_session_portal(&self, name: &str) -> Result<(), SQLError> {
        Engine::close_session_portal(self, name)
    }
    fn close_all_session_portals(&self) {
        Engine::close_all_session_portals(self);
    }
}
