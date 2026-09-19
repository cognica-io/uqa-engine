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
use crate::catalog::security::table_inquiry::TablePrivilegeContext;
use crate::row_locks::binding::{bind_relation, RelationBinding};
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
    pub authority: TablePrivilegeContext<'a>,
    pub definition: SequenceDefinitionContext<'a>,
    pub roles: SequenceRoleOwnershipContext<'a>,
    pub lifecycle: SequenceLifecycleContext<'a>,
}
pub fn alter_sequence(
    context: &SequenceAlterContext<'_>,
    alter: &AlterSequence,
) -> Result<bool, SQLError> {
    let Some(binding) = bind_alter_sequence(context, alter)? else {
        return Ok(false);
    };
    if let Some(role_owner) = alter.role_owner.as_ref() {
        super::role_ownership::alter_sequence_role_owner(
            &context.roles,
            &binding.name,
            &binding.value.relation,
            role_owner,
        )?;
        return Ok(true);
    }
    let persistence = context
        .catalog
        .sequence_persistence(&binding.value.relation);
    if alter.lifecycle != uqa_sql::ast::SequenceLifecycle::Unchanged {
        super::lifecycle::alter_sequence_lifecycle(
            &context.lifecycle,
            &binding.name,
            &binding.value.relation,
            persistence,
            alter,
        )?;
        return Ok(true);
    }
    super::alteration::alter_sequence_definition(
        &context.definition,
        &binding.name,
        &binding.value.relation,
        persistence,
        alter,
    )
}

struct SequenceAlterTarget {
    relation: RelationIdentity,
    kind: &'static str,
}

fn bind_alter_sequence(
    context: &SequenceAlterContext<'_>,
    alter: &AlterSequence,
) -> Result<Option<RelationBinding<SequenceAlterTarget>>, SQLError> {
    bind_relation(
        context.roles.writer,
        uqa_sql::schema::sequences::actions::sequence_alter_lock_mode(alter).into(),
        false,
        || {
            let Some((name, kind)) =
                uqa_sql::schema::sequences::lifecycle::sequence_alter_relation(
                    context.catalog.resolve_visible_relation(&alter.name)?,
                    alter,
                )?
            else {
                return Ok(None);
            };
            if alter.role_owner.is_some() {
                uqa_sql::schema::sequences::actions::validate_sequence_role_owner_shape(alter)?;
            }
            let relation = RelationIdentity::from_legacy_name(&name).map_err(|error| {
                SQLError::Internal(format!("resolve sequence `{name}`: {error}"))
            })?;
            Ok(Some(RelationBinding {
                name,
                object_id: context.roles.metadata.object_id(&relation),
                value: SequenceAlterTarget { relation, kind },
            }))
        },
        |binding| {
            let target = &binding.value;
            context
                .authority
                .ensure_relation_owner(&target.relation, target.kind)?;
            uqa_sql::catalog::security::ownership::reject_system_relation_alter(&target.relation)?;
            if matches!(
                alter.lifecycle,
                uqa_sql::ast::SequenceLifecycle::RenameTo { .. }
            ) {
                context
                    .lifecycle
                    .creation
                    .ensure_namespace_create(&target.relation.schema)?;
            }
            uqa_sql::schema::sequences::lifecycle::validate_sequence_alter_kind(
                alter,
                target.kind,
                &target.relation.name,
            )
        },
    )
}
