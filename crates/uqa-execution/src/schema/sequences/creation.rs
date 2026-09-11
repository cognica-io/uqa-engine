//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence creation order, collision handling, and catalog publication.
use crate::catalog::sequence::SequenceState;
use uqa_core::RelationIdentity;
use uqa_sql::ast::{RelationPersistence, SequenceOwnership};
use uqa_sql::schema::sequences::{
    definition::validate_sequence_definition, ownership::SequenceOwnerCatalog,
};
use uqa_sql::SQLError;
use uqa_storage::{SequenceOwner, SequenceOwnerDependency, StorageBackendResult};

pub trait SequenceCreationNamespace {
    fn refresh_sequences(&self) -> StorageBackendResult<()>;
    fn relation_exists(&self, name: &str) -> StorageBackendResult<bool>;
}

pub trait SequenceCreationPublication {
    fn insert_sequence(
        &self,
        name: &str,
        relation: &RelationIdentity,
        state: SequenceState,
        persistence: RelationPersistence,
    ) -> Result<bool, SQLError>;
}

#[derive(Clone, Copy)]
pub struct SequenceCreationContext<'a> {
    pub creation: crate::schema::namespaces::relations::RelationCreationContext<'a>,
    pub namespace: &'a dyn SequenceCreationNamespace,
    pub owners: &'a dyn SequenceOwnerCatalog,
    pub publication: &'a dyn SequenceCreationPublication,
}

pub fn bind_sequence_owner(
    catalog: &dyn SequenceOwnerCatalog,
    name: &str,
    ownership: &SequenceOwnership,
) -> Result<Option<SequenceOwner>, SQLError> {
    uqa_sql::schema::sequences::ownership::bind_sequence_owner(catalog, name, ownership).map(
        |owner| {
            owner.map(|owner| SequenceOwner {
                table_object_id: owner.table_object_id,
                column_object_id: owner.column_object_id,
                dependency: SequenceOwnerDependency::Automatic,
            })
        },
    )
}

pub fn create_sequence(
    context: &SequenceCreationContext<'_>,
    name: &str,
    mut state: SequenceState,
    if_not_exists: bool,
    persistence: RelationPersistence,
    ownership: &SequenceOwnership,
) -> Result<bool, SQLError> {
    validate_sequence_definition(&state.definition(), None)?;
    let name = if persistence == RelationPersistence::Temporary {
        context.creation.temporary_name(name)?
    } else {
        context.creation.persistent_name(name)?
    };
    let relation = RelationIdentity::from_legacy_name(&name)
        .map_err(|error| SQLError::Internal(format!("resolve sequence `{name}`: {error}")))?;
    context.namespace.refresh_sequences().map_err(|error| {
        SQLError::Internal(format!("load sequence catalog for `{name}`: {error}"))
    })?;
    if context
        .namespace
        .relation_exists(&name)
        .map_err(|error| SQLError::Internal(format!("resolve relation `{name}`: {error}")))?
    {
        return sequence_create_collision(&name, if_not_exists);
    }
    state.owner = bind_sequence_owner(context.owners, &name, ownership)?;
    if !context
        .publication
        .insert_sequence(&name, &relation, state, persistence)?
    {
        return sequence_create_collision(&name, if_not_exists);
    }
    Ok(true)
}

fn sequence_create_collision(name: &str, if_not_exists: bool) -> Result<bool, SQLError> {
    if if_not_exists {
        Ok(false)
    } else {
        Err(SQLError::Routine {
            sqlstate: "42P07".into(),
            message: format!("relation \"{name}\" already exists"),
        })
    }
}
