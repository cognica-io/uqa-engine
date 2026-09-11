//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compose native statement execution from its query, schema, routine and session inputs.

use crate::catalog::services::CatalogSession;
use uqa_sql::{
    plan::UnifiedPlan,
    result::ExplainAnalysis,
    semantics::{effects::QueryEffectContext, rules::RuleCatalog},
    SQLError, SQLResult,
};
pub mod definitions;
pub mod queries;
pub mod routines;
pub mod schemas;
pub mod session;

pub type ExplainRenderer =
    fn(&UnifiedPlan, bool, Option<&str>, Option<&ExplainAnalysis>) -> Result<SQLResult, SQLError>;

pub trait StatementEffects {
    fn query_effect_context(&self) -> QueryEffectContext<'_>;
}

/// Cancellation and diagnostics for the top-level statement boundary.
#[derive(Clone, Copy)]
pub struct StatementRuntime<'a> {
    pub cancellation: &'a uqa_core::CancellationToken,
    pub notices: &'a parking_lot::Mutex<Vec<(String, String)>>,
}

#[derive(Clone, Copy)]
pub struct StatementValidationContext<'a> {
    pub session: &'a dyn CatalogSession,
    pub rules: &'a dyn RuleCatalog,
    pub effects: &'a dyn StatementEffects,
    pub transactions: &'a dyn super::transactions::StatementTransactions,
}

pub trait StatementMutationInputs<S: Clone + 'static> {
    fn mutation_context(&self) -> crate::mutation::entry::MutationEntryContext<'_, S>;
}

#[derive(Clone)]
pub struct StatementExecutionContext<'a, S: Clone + 'static> {
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
    pub portals: &'a dyn session::StatementPortals,
    pub roles: &'a dyn definitions::RoleDefinitions,
    pub events: &'a dyn definitions::EventDefinitions,
    pub foreign: &'a dyn definitions::ForeignDefinitions,
    pub table_privileges: &'a dyn definitions::TablePrivileges,
    pub explain: ExplainRenderer,
}
