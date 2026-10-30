//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Name resolution before command entry and fresh execution inputs inside the transaction.
use crate::mutation::output::MutationOutput;
use crate::mutation::{
    statement::context::MutationStatementContext, views::commands::SourceOutputPruning,
};
use uqa_sql::{
    routines::RoutineResolution,
    semantics::{conflict::InferenceContext, returning::ReturningAnalysisContext},
    SQLError,
};

pub trait MutationTargetResolution {
    fn resolve_target(&self, name: &str, bound: bool) -> Result<String, SQLError>;
    /// Native resolvers admit the selected name; external resolvers keep their original callback.
    fn resolve_target_with_control(
        &self,
        name: &str,
        bound: bool,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<String>, SQLError> {
        control.check()?;
        let resolved = self.resolve_target(name, bound)?;
        Ok(control.copy_text(&resolved)?)
    }
}
pub struct MutationCommandContext<'a, S: Clone + 'static> {
    pub statement: MutationStatementContext<'a, S>,
    pub inference: InferenceContext<'a>,
    pub returning: ReturningAnalysisContext<'a>,
    pub prune_source_outputs: SourceOutputPruning,
}
pub type MutationCommand<'a, S> =
    Box<dyn FnOnce(&MutationCommandContext<'_, S>) -> Result<MutationOutput, SQLError> + 'a>;
pub trait MutationCommandBoundary<S: Clone + 'static> {
    fn with_command(&self, command: MutationCommand<'_, S>) -> Result<MutationOutput, SQLError>;
}
pub struct MutationEntryContext<'a, S: Clone + 'static> {
    pub targets: &'a dyn MutationTargetResolution,
    pub transactions: &'a dyn MutationCommandBoundary<S>,
    pub routines: &'a dyn RoutineResolution,
}
