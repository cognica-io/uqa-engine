//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compose statement contexts over the current Engine state without owning plan dispatch.

use crate::{session::StatementReadSnapshot, Engine};
use uqa_execution::statement::context::{
    self, StatementEffects, StatementExecutionContext, StatementValidationContext,
};
impl Engine {
    pub(crate) fn statement_execution_context(
        &self,
    ) -> StatementExecutionContext<'_, StatementReadSnapshot> {
        let runtime = self.query_runtime_view();
        StatementExecutionContext {
            validation: StatementValidationContext {
                session: self,
                rules: self,
                effects: self,
                transactions: self,
            },
            runtime: context::StatementRuntime {
                cancellation: runtime.cancellation,
                notices: runtime.notices,
            },
            queries: self,
            mutations: self,
            schemas: context::schemas::SchemaStatements {
                creation: self,
                removal: self,
                tables_as: self,
                views: self,
                view_alteration: self,
                foreign_alteration: self,
                sequence_creation: self,
                sequence_alteration: self,
                owners: self,
                inputs: self,
            },
            routines: context::routines::RoutineStatements {
                resolution: self,
                inputs: self,
                transactions: self,
            },
            prepared: context::session::PreparedStatements {
                state: self,
                arguments: self,
            },
            settings: self,
            notifications: self,
            controls: self,
            portals: self.portal_execution_context(),
            roles: self,
            events: self,
            foreign: self,
            table_privileges: self,
            explain: uqa_planner::explain::run_explain,
        }
    }
}
impl context::StatementExecutionInputs<StatementReadSnapshot> for Engine {
    fn statement_execution_context(&self) -> StatementExecutionContext<'_, StatementReadSnapshot> {
        Engine::statement_execution_context(self)
    }
}

impl StatementEffects for Engine {
    fn query_effect_context(&self) -> uqa_sql::semantics::effects::QueryEffectContext<'_> {
        Engine::query_effect_context(self)
    }
}
mod definitions;
mod queries;
mod routines;
mod schemas;
mod session;
