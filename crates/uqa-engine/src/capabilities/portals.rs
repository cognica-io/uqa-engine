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

impl Engine {
    pub(crate) fn portal_binding_context(
        &self,
    ) -> uqa_sql::binding::portals::PortalBindingContext<'_> {
        uqa_sql::binding::portals::PortalBindingContext {
            catalog: self,
            routines: self,
            transitions: self,
        }
    }
}

impl uqa_sql::binding::portals::PortalRelationCatalog for Engine {
    fn try_resolve_table_name(&self, name: &str) -> Result<Option<String>, String> {
        Engine::try_resolve_table_name(self, name).map_err(|error| error.to_string())
    }
    fn hierarchy_scan_tables(
        &self,
        table: &str,
        include_descendants: bool,
    ) -> Result<Vec<String>, SQLError> {
        Engine::hierarchy_scan_tables(self, table, include_descendants)
    }
    fn view_plan(&self, name: &str) -> Result<Option<uqa_sql::plan::QueryPlan>, SQLError> {
        Engine::view_plan(self, name)
    }
    fn try_resolve_visible_relation_kind(
        &self,
        name: &str,
    ) -> Result<Option<(String, &'static str)>, SQLError> {
        Engine::try_resolve_visible_relation_kind(self, name)
    }
    fn resolve_age_label_relation_name(&self, name: &str) -> Result<Option<String>, SQLError> {
        uqa_execution::catalog::projection::resolve_age_label_relation_name(
            &self.catalog_execution(),
            name,
        )
    }
    fn try_resolve_sequence_reference(&self, name: &str) -> Result<Option<String>, String> {
        Engine::try_resolve_sequence_oid_reference_for_binding(self, name)
            .map_err(|error| error.to_string())
    }
}

impl uqa_sql::binding::portals::PortalTransitionRelations for Engine {
    fn active_transition_relation_names(&self) -> std::collections::BTreeSet<String> {
        uqa_execution::mutation::triggers::current_transition_relation_names()
    }
}
