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
