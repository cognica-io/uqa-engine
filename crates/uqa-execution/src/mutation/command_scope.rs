//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Command state boundaries independent of the owning session.
use crate::query::CteScope;
use uqa_sql::SQLError;
/// Mutation lifecycle operations on the active transaction frame.
pub trait MutationCommandState {
    fn prepare_writer(&self) -> Result<(), SQLError>;
    fn begin_overlay(&self);
    fn end_overlay(&self);
}
/// Capture catalog, namespace, trigger transition, and privilege state for a command.
pub trait CommandScopeSource<S: Clone> {
    fn command_scope(
        &self,
        privilege_subject: Option<&str>,
        relations_bound: bool,
    ) -> Result<CteScope<S>, SQLError>;
}
pub struct MutationOverlayScope<'a> {
    state: &'a dyn MutationCommandState,
}
impl<'a> MutationOverlayScope<'a> {
    pub fn new(state: &'a dyn MutationCommandState) -> Self {
        state.begin_overlay();
        Self { state }
    }
}
impl Drop for MutationOverlayScope<'_> {
    fn drop(&mut self) {
        self.state.end_overlay();
    }
}

/// Retain the command's original read generation when CTE writes or BEFORE statement triggers can change visible rows.
pub fn capture_command_read_snapshot<S: Clone + 'static>(
    snapshots: &dyn crate::query::statement::context::StatementSnapshots<S>,
    inherited: Option<&CteScope<S>>,
    before_statement_trigger: bool,
    ctes: &[uqa_sql::plan::CtePlan],
) -> Result<Option<std::sync::Arc<S>>, SQLError> {
    match inherited.and_then(CteScope::command_cte_snapshot) {
        Some(snapshot) => Ok(Some(snapshot)),
        None if before_statement_trigger || ctes.iter().any(|cte| cte.body.modifies_data()) => {
            Ok(Some(std::sync::Arc::new(snapshots.capture()?)))
        }
        None => Ok(None),
    }
}
