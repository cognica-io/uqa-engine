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
            explain: uqa_planner::explain::run_explain_with_control,
            physical_explain: self,
        }
    }
}

impl context::PhysicalExplainPlanning for Engine {
    fn explained_plan_estimates(
        &self,
        body: &uqa_sql::plan::UnifiedPlan,
        params: &[uqa_sql::SQLParam],
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<
        uqa_core::memory::Produced<Vec<uqa_sql::result::explain::ExplainNodeEstimate>>,
        uqa_sql::SQLError,
    > {
        uqa_planner::explain::estimate_plan(
            self.statement_statistics_context(),
            &self.statement_planning_context().analysis,
            body,
            params,
            control,
        )
    }

    fn plan_explained_statement(
        &self,
        body: &uqa_sql::plan::UnifiedPlan,
    ) -> Result<uqa_sql::plan::UnifiedPlan, uqa_sql::SQLError> {
        super::statement_planning::optimize_engine_plan(self, body.clone())
    }

    fn physical_explain_plan(
        &self,
        body: &uqa_sql::plan::UnifiedPlan,
        params: &[uqa_sql::SQLParam],
        costs: bool,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<uqa_sql::result::ExplainPhysicalPlan>, uqa_sql::SQLError>
    {
        let catalog = self.catalog_read_view();
        let resolution = self.session_execution_view().relation_name_resolution();
        uqa_planner::explain::physical_plan_with_control(
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
                        routines: self,
                        catalog: &catalog,
                        resolution: &resolution,
                        scoped_relations: None,
                    },
                    optimizer: &|plan| super::statement_planning::optimize_engine_plan(self, plan),
                },
                evaluate: &uqa_execution::eval_constant_scalar_with_control,
            },
            body,
            params,
            costs,
            control,
        )
    }
}
impl context::StatementExecutionInputs<StatementReadSnapshot> for Engine {
    fn io_context(&self) -> uqa_core::io_usage::IOContext {
        self.runtime.diagnostics.io_context()
    }

    fn regex_cache(&self) -> Option<std::sync::Arc<uqa_sql::expr::RegexCache>> {
        Some(self.session_regex_cache())
    }
    fn parser_settings(&self) -> uqa_sql::parser::ParserSettings {
        Engine::parser_settings(self)
    }

    fn transaction_timestamp_micros(&self) -> Option<i64> {
        self.session_transaction_clock_micros()
    }

    fn temporal_date_order(&self) -> Option<uqa_core::TemporalDateOrder> {
        Some(uqa_sql::semantics::parameters::datestyle::date_order(
            &self.session.setting("DateStyle"),
        ))
    }

    fn temporal_interval_style(&self) -> Option<uqa_core::TemporalIntervalStyle> {
        Some(self.session.interval_style())
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
            cancellation: &self.runtime.cancellation,
        }
    }
}

impl context::StatementDiagnostics for Engine {
    fn diagnostics_control(
        &self,
    ) -> Result<uqa_storage::read_control::StorageReadControl, uqa_sql::SQLError> {
        self.query_retention_control()
    }

    fn diagnostics_scope(
        &self,
        timing: Option<bool>,
        io: uqa_core::io_usage::IOOptions,
    ) -> Result<uqa_execution::query::diagnostics::DiagnosticsScope, uqa_sql::SQLError> {
        self.runtime
            .diagnostics
            .enter_with_options(self.query_retention_control()?, timing, io)
            .map_err(|error| {
                uqa_execution::storage_errors::storage_error("retain EXPLAIN diagnostics", &error)
            })
    }
}
