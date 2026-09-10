//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutation execution services bound to one command generation.
use super::{
    command_scope::MutationCommandState, identity::InsertIdentityContext,
    preparation::MutationPreparationContext, publication::PublicationContext,
    rules::views::ViewRuleContext,
};
use uqa_sql::semantics::mutation_qualifiers::MutationTargetColumns;
#[derive(Clone)]
pub struct MutationExecutionContext<'a, S: Clone + 'static> {
    pub insert_consumers: &'a dyn super::insert::source::binding::InsertSelectBinding<S>,
    pub preparation: MutationPreparationContext<'a, S>,
    pub identities: InsertIdentityContext<'a>,
    pub rules: ViewRuleContext<'a, S>,
    pub publication: PublicationContext<'a>,
    pub state: &'a dyn MutationCommandState,
    pub scopes: &'a dyn super::command_scope::CommandScopeSource<S>,
    pub points: &'a dyn super::point_update::context::PointMutationStorage,
    pub privileges: &'a dyn uqa_sql::semantics::mutation_privileges::MutationPrivilegeCatalog,
    pub targets: &'a dyn MutationTargetColumns,
}
impl<S: Clone + 'static> Copy for MutationExecutionContext<'_, S> {}

impl<'a, S: Clone + 'static> MutationExecutionContext<'a, S> {
    pub fn point_update(&self) -> super::point_update::PointMutationContext<'a, S> {
        super::point_update::PointMutationContext {
            assignment: self.preparation.referential.assignment,
            constraints: self.preparation.referential.constraints,
            locking: self.preparation.referential.locking,
            scopes: self.scopes,
            transaction: self.state,
            storage: self.points,
        }
    }
}

pub mod context;
