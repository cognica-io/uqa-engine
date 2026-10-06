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
            diagnostics: self,
            validation: StatementValidationContext {
                session: self,
                rules: self,
                effects: self,
                transactions: self,
                aliases: self,
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
                renames: self,
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
                definitions: self.prepared_registration_context(),
                plans: self,
            },
            settings: self,
            notifications: self,
            controls: self,
            portals: self.portal_execution_context(),
            roles: self.role_execution_context(),
            events: self.event_lifecycle_context(),
            foreign: self,
            table_privileges: self,
            explain: uqa_planner::explain::run_explain_with_physical,
            physical_explain: self,
        }
    }
}

impl context::PhysicalExplainPlanning for Engine {
    fn physical_explain_plan(
        &self,
        body: &uqa_sql::plan::UnifiedPlan,
        params: &[uqa_sql::SQLParam],
    ) -> Result<uqa_sql::result::ExplainPhysicalPlan, uqa_sql::SQLError> {
        let catalog = self.catalog_read_view();
        let resolution = self.session_execution_view().relation_name_resolution();
        uqa_planner::explain::physical_plan(
            uqa_planner::explain::PhysicalExplainContext {
                retrieval: self,
                statistics: self,
                validate_source: &|source_resolution, name| {
                    uqa_execution::catalog::foreign::reference::validate_query_source(
                        &catalog,
                        source_resolution,
                        name,
                    )
                },
                filters: uqa_planner::filter_pushdown::context::FilterPushdownContext {
                    volatility: self,
                    correlation: uqa_sql::binding::correlation::CorrelationContext {
                        catalog: &catalog,
                        resolution: &resolution,
                    },
                    optimizer: &|plan| super::statement_planning::optimize_engine_plan(self, plan),
                },
                evaluate: &|expression, params| {
                    uqa_execution::eval_scalar(
                        expression,
                        &uqa_execution::ScalarEvalContext::new(None, params),
                    )
                },
            },
            body,
            params,
        )
    }
}
impl context::StatementExecutionInputs<StatementReadSnapshot> for Engine {
    fn parser_settings(&self) -> uqa_sql::parser::ParserSettings {
        Engine::parser_settings(self)
    }

    fn transaction_timestamp_micros(&self) -> Option<i64> {
        Some(Engine::transaction_timestamp_micros(self))
    }

    fn temporal_date_order(&self) -> Option<uqa_core::TemporalDateOrder> {
        Some(uqa_sql::semantics::parameters::datestyle::date_order(
            &self.session.setting("DateStyle"),
        ))
    }

    fn diagnostic_search_path(&self) -> Option<Vec<String>> {
        Some(self.session.state.read().search_path.clone())
    }

    fn notification_subscriptions_required(&self) -> bool {
        Engine::notification_subscriptions_required(self)
    }

    fn statement_timeout(&self) -> Option<std::time::Duration> {
        let milliseconds = self
            .session
            .setting("statement_timeout")
            .parse::<u64>()
            .ok()?;
        (milliseconds != 0).then(|| std::time::Duration::from_millis(milliseconds))
    }

    fn statement_execution_context(&self) -> StatementExecutionContext<'_, StatementReadSnapshot> {
        Engine::statement_execution_context(self)
    }
}

impl StatementEffects for Engine {
    fn query_effect_context(&self) -> uqa_sql::semantics::effects::QueryEffectContext<'_> {
        Engine::query_effect_context(self)
    }
}
mod queries;
mod routines;
mod schemas;
mod session;

impl Engine {
    pub(crate) fn compiled_statement_context(
        &self,
    ) -> uqa_execution::statement::compiled::CompiledStatementContext<
        '_,
        crate::session::StatementReadSnapshot,
    > {
        uqa_execution::statement::compiled::CompiledStatementContext {
            aggregates: self,
            planning: self,
            statements: self,
        }
    }
}

impl context::StatementDiagnostics for Engine {
    fn diagnostics_scope(
        &self,
    ) -> Result<uqa_execution::query::diagnostics::DiagnosticsScope, uqa_sql::SQLError> {
        self.runtime
            .diagnostics
            .enter(self.query_retention_control()?)
            .map_err(|error| {
                uqa_execution::storage_errors::storage_error("retain EXPLAIN diagnostics", &error)
            })
    }
}
