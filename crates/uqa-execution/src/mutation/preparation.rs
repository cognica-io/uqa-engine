//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical row preparation services shared by INSERT, UPDATE, and referential actions.
use super::{
    referential::ReferentialContext, returning::ReturningExecutionContext,
    staging::MutationStagingContext,
};
#[derive(Clone)]
pub struct MutationPreparationContext<'a, S: Clone + 'static> {
    pub referential: ReferentialContext<'a, S>,
    pub staging: MutationStagingContext<'a>,
    pub returning: ReturningExecutionContext<'a, S>,
}
impl<S: Clone + 'static> Copy for MutationPreparationContext<'_, S> {}
