//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compose native statement execution from its query, schema, routine and session inputs.

use crate::catalog::services::CatalogSession;
use uqa_sql::{
    plan::UnifiedPlan,
    result::{ExplainAnalysis, ExplainPhysicalPlan},
    semantics::{effects::QueryEffectContext, rules::RuleCatalog},
    SQLError, SQLParam, SQLResult,
};
pub mod queries;
pub mod routines;
pub mod schemas;
pub mod session;

pub type PhysicalExplainRenderer = fn(
    &UnifiedPlan,
    &uqa_sql::explain::ExplainSettings,
    Option<&ExplainAnalysis>,
    &ExplainPhysicalPlan,
) -> Result<SQLResult, SQLError>;

/// Capture physical facts before an optional execution without invoking the explained query.
pub trait PhysicalExplainPlanning: Sync {
    /// Optimize the already analyzed body only after the command has interpreted its options.
    fn plan_explained_statement(&self, body: &UnifiedPlan) -> Result<UnifiedPlan, SQLError>;
    fn physical_explain_plan(
        &self,
        body: &UnifiedPlan,
        params: &[SQLParam],
    ) -> Result<ExplainPhysicalPlan, SQLError>;
}

pub trait StatementDiagnostics: Sync {
    fn diagnostics_scope(&self) -> Result<crate::query::diagnostics::DiagnosticsScope, SQLError>;
}

pub trait StatementEffects {
    fn query_effect_context(&self) -> QueryEffectContext<'_>;
}

/// Cancellation and diagnostics for the top-level statement boundary.
#[derive(Clone, Copy)]
pub struct StatementRuntime<'a> {
    pub cancellation: &'a uqa_core::CancellationToken,
    pub notices: &'a crate::query::NoticeQueue,
}

#[derive(Clone, Copy)]
pub struct StatementValidationContext<'a> {
    pub session: &'a dyn CatalogSession,
    pub rules: &'a dyn RuleCatalog,
    pub effects: &'a dyn StatementEffects,
    pub transactions: &'a dyn super::transactions::StatementTransactions,
    /// Reads the objects the statement's `reg*` constants name, as their input functions read them when the statement is analyzed.
    pub aliases: &'a dyn uqa_sql::schema::dependencies::oid_alias::OidAliasInput,
}

/// Capture live subsystem inputs only when a statement is ready to execute.
pub trait StatementExecutionInputs<S: Clone + 'static> {
    /// The session-owned compiled regex cache, shared by its nested statements and workers.
    fn regex_cache(&self) -> Option<std::sync::Arc<uqa_sql::expr::RegexCache>> {
        None
    }
    fn statement_execution_context(&self) -> StatementExecutionContext<'_, S>;
    /// The live transaction start, or the current message's start outside a transaction. `None` preserves the calling statement's clock without capturing execution inputs.
    fn transaction_timestamp_micros(&self) -> Option<i64> {
        None
    }
    /// The live session order for ambiguous date inputs. None preserves a caller's input context.
    fn temporal_date_order(&self) -> Option<uqa_core::TemporalDateOrder> {
        None
    }
    /// The live interval input/output style; None preserves a caller's context.
    fn temporal_interval_style(&self) -> Option<uqa_core::TemporalIntervalStyle> {
        None
    }
    /// The session's `statement_timeout`, which a statement starts with; `None` lets a statement run without a limit. Reading it captures none of the statement's execution inputs.
    fn statement_timeout(&self) -> Option<std::time::Duration>;
    /// Capture scanner settings at the SQL message boundary, before its first SET can execute.
    fn parser_settings(&self) -> uqa_sql::parser::ParserSettings {
        uqa_sql::parser::ParserSettings::default()
    }
    /// Read the live host policy without capturing the statement's catalog/execution inputs.
    fn notification_subscriptions_required(&self) -> bool {
        false
    }

    /// The session search path, which diagnostics use to qualify types it does not include. `None` leaves type names unqualified.
    fn diagnostic_search_path(&self) -> Option<Vec<String>> {
        None
    }
}

/// Enter the session's live temporal input and the statement's `cancellation` on the invoking thread for the statement's analysis, planning and execution, without capturing its execution inputs.
pub(super) fn statement_thread_scope<S: Clone + 'static>(
    statements: &dyn StatementExecutionInputs<S>,
    cancellation: &uqa_core::CancellationToken,
) -> super::thread_scope::StatementThreadScope {
    super::thread_context::StatementThreadContext {
        retained_rows: crate::query::sources::retained_source::RetainedRowContext::current(),
        regex_cache: statements.regex_cache(),
        temporal_input: uqa_sql::expr::TemporalInputContext {
            transaction_clock_micros: statements
                .transaction_timestamp_micros()
                .or_else(uqa_sql::expr::transaction_clock_micros),
            interval_style: statements
                .temporal_interval_style()
                .unwrap_or_else(uqa_sql::expr::temporal_interval_style),
            date_order: statements
                .temporal_date_order()
                .unwrap_or_else(uqa_sql::expr::temporal_date_order),
        },
        cancellation: Some(cancellation.clone()),
    }
    .enter()
}

pub trait StatementMutationInputs<S: Clone + 'static> {
    fn mutation_context(&self) -> crate::mutation::entry::MutationEntryContext<'_, S>;
    /// Borrow read services for expression initialization without entering a writable command.
    fn mutation_initialization_context(
        &self,
    ) -> crate::mutation::statement::context::MutationStatementContext<'_, S>;
}

#[derive(Clone)]
pub struct StatementExecutionContext<'a, S: Clone + 'static> {
    pub diagnostics: &'a dyn StatementDiagnostics,
    pub validation: StatementValidationContext<'a>,
    pub runtime: StatementRuntime<'a>,
    pub queries: &'a dyn queries::StatementQueryContexts<S>,
    pub mutations: &'a dyn StatementMutationInputs<S>,
    pub schemas: schemas::SchemaStatements<'a, S>,
    pub routines: routines::RoutineStatements<'a>,
    pub prepared: session::PreparedStatements<'a>,
    pub settings: &'a dyn session::StatementSettings,
    pub notifications: &'a dyn session::StatementNotifications,
    pub controls: &'a dyn session::StatementControl,
    pub portals: super::portal::context::PortalExecutionContext<'a, S>,
    pub roles: crate::catalog::security::role_lifecycle::context::RoleExecutionContext<'a>,
    pub events: crate::schema::events::context::EventLifecycleContext<'a>,
    pub foreign: &'a dyn crate::schema::foreign_creation::entry::ForeignCreationTransactions,
    pub table_privileges: &'a dyn crate::catalog::security::table_grants::TableGrantInputs,
    pub explain: PhysicalExplainRenderer,
    pub physical_explain: &'a dyn PhysicalExplainPlanning,
}
