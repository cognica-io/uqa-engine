//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dispatch ALTER SEQUENCE across definition, role-owner, and name lifecycle execution.
use super::{
    alteration::SequenceDefinitionContext, lifecycle::SequenceLifecycleContext,
    role_ownership::SequenceRoleOwnershipContext,
};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{AlterSequence, RelationPersistence},
    catalog::resolution::RelationResolution,
    SQLError,
};
pub trait SequenceCommandCatalog {
    fn resolve_visible_relation(&self, name: &str) -> Result<RelationResolution, SQLError>;
    fn sequence_persistence(&self, relation: &RelationIdentity) -> RelationPersistence;
}
pub struct SequenceAlterContext<'a> {
    pub catalog: &'a dyn SequenceCommandCatalog,
    pub definition: SequenceDefinitionContext<'a>,
    pub roles: SequenceRoleOwnershipContext<'a>,
    pub lifecycle: SequenceLifecycleContext<'a>,
}
pub fn alter_sequence(
    context: &SequenceAlterContext<'_>,
    alter: &AlterSequence,
) -> Result<bool, SQLError> {
    let Some(name) = uqa_sql::schema::sequences::lifecycle::alter_sequence_target_name(
        context.catalog.resolve_visible_relation(&alter.name)?,
        alter,
    )?
    else {
        return Ok(false);
    };
    let relation = RelationIdentity::from_legacy_name(&name)
        .map_err(|error| SQLError::Internal(format!("resolve sequence `{name}`: {error}")))?;
    if let Some(role_owner) = alter.role_owner.as_deref() {
        uqa_sql::schema::sequences::actions::validate_sequence_role_owner_shape(alter)?;
        super::role_ownership::alter_sequence_role_owner(
            &context.roles,
            &name,
            &relation,
            role_owner,
        )?;
        return Ok(true);
    }
    context
        .roles
        .access
        .ensure_sequence_owner(&name, &relation)?;
    let persistence = context.catalog.sequence_persistence(&relation);
    if alter.lifecycle != uqa_sql::ast::SequenceLifecycle::Unchanged {
        super::lifecycle::alter_sequence_lifecycle(
            &context.lifecycle,
            &name,
            &relation,
            persistence,
            alter,
        )?;
        return Ok(true);
    }
    super::alteration::alter_sequence_definition(
        &context.definition,
        &name,
        &relation,
        persistence,
        alter,
    )
}
