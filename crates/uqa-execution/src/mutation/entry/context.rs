//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Name resolution before command entry and fresh execution inputs inside the transaction.
use crate::mutation::{
    statement::context::MutationStatementContext, views::commands::SourceOutputPruning,
};
use uqa_sql::{
    routines::RoutineResolution,
    semantics::{conflict::InferenceContext, returning::ReturningAnalysisContext},
    SQLError, SQLResult,
};

pub trait MutationTargetResolution {
    fn resolve_target(&self, name: &str, bound: bool) -> Result<String, SQLError>;
}
pub struct MutationCommandContext<'a, S: Clone + 'static> {
    pub statement: MutationStatementContext<'a, S>,
    pub inference: InferenceContext<'a>,
    pub returning: ReturningAnalysisContext<'a>,
    pub prune_source_outputs: SourceOutputPruning,
}
pub type MutationCommand<'a, S> =
    Box<dyn FnOnce(&MutationCommandContext<'_, S>) -> Result<SQLResult, SQLError> + 'a>;
pub trait MutationCommandBoundary<S: Clone + 'static> {
    fn with_command(&self, command: MutationCommand<'_, S>) -> Result<SQLResult, SQLError>;
}
pub struct MutationEntryContext<'a, S: Clone + 'static> {
    pub targets: &'a dyn MutationTargetResolution,
    pub transactions: &'a dyn MutationCommandBoundary<S>,
    pub routines: &'a dyn RoutineResolution,
}
