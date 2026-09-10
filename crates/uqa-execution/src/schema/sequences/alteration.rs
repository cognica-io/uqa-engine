//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Apply sequence definition changes and publish allocation generations and owner metadata.
use crate::catalog::sequence::SequenceState;
use crate::schema::publication::dependencies::SchemaDependencyPublicationContext;
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{AlterSequence, RelationPersistence},
    schema::sequences::ownership::SequenceOwnerCatalog,
    SQLError,
};
use uqa_storage::{SequenceOwner, SequenceOwnerDependency, StorageBackendResult};

pub trait SequenceDefinitionCatalog {
    fn object_id(&self, relation: &RelationIdentity) -> Option<[u8; 16]>;
    fn state(&self, relation: &RelationIdentity) -> Option<SequenceState>;
    fn owner_target(&self, owner: SequenceOwner) -> Option<(String, String, bool)>;
}
pub trait SequenceDefinitionPublication {
    fn replace_sequence(
        &self,
        name: &str,
        relation: &RelationIdentity,
        object_id: [u8; 16],
        persistence: RelationPersistence,
        state: SequenceState,
        invalidate_current_cache: bool,
    ) -> Result<(), SQLError>;
}
pub struct SequenceDefinitionContext<'a> {
    pub catalog: &'a dyn SequenceDefinitionCatalog,
    pub owners: &'a dyn SequenceOwnerCatalog,
    pub publication: &'a dyn SequenceDefinitionPublication,
    pub markers: SchemaDependencyPublicationContext<'a>,
    pub new_generation: fn() -> StorageBackendResult<[u8; 16]>,
}
pub fn alter_sequence_definition(
    context: &SequenceDefinitionContext<'_>,
    name: &str,
    relation: &RelationIdentity,
    persistence: RelationPersistence,
    alter: &AlterSequence,
) -> Result<bool, SQLError> {
    let target_persistence = uqa_sql::schema::sequences::actions::altered_sequence_persistence(
        alter,
        persistence,
        &relation.name,
    )?;
    if target_persistence == persistence
        && uqa_sql::schema::sequences::actions::sequence_alter_is_persistence_only(alter)
    {
        return Ok(true);
    }
    let object_id = context
        .catalog
        .object_id(relation)
        .ok_or_else(|| SQLError::Internal(format!("sequence `{name}` has no object identity")))?;
    let state = context
        .catalog
        .state(relation)
        .ok_or_else(|| SQLError::Internal(format!("sequence `{name}` disappeared")))?;
    let mut state = crate::catalog::sequence::altered_sequence_state(state, alter)?;
    if alter.ownership != uqa_sql::ast::SequenceOwnership::Unchanged {
        let owner = crate::schema::sequences::creation::bind_sequence_owner(
            context.owners,
            name,
            &alter.ownership,
        )?;
        if state
            .owner
            .is_some_and(|current| current.dependency == SequenceOwnerDependency::Internal)
        {
            let owner_table = context
                .catalog
                .owner_target(state.owner.expect("identity owner was checked"))
                .map_or_else(|| "<missing>".into(), |(table, _, _)| table);
            return Err(SQLError::Routine {
                    sqlstate: "0A000".into(),
                    message: format!(
                        "cannot change ownership of identity sequence; sequence \"{}\" is linked to table \"{owner_table}\"",
                        relation.name
                    ),
                });
        }
        state.owner = owner;
    }
    let definition_generation = (context.new_generation)().map_err(|error| {
        SQLError::Internal(format!(
            "allocate sequence `{name}` definition generation: {error}"
        ))
    })?;
    state.definition_generation = definition_generation;
    context.publication.replace_sequence(
        name,
        relation,
        object_id,
        target_persistence,
        state,
        alter.persistence.is_none(),
    )?;
    if alter.ownership != uqa_sql::ast::SequenceOwnership::Unchanged {
        super::ownership::clear_auto_increment_owner_markers(&context.markers, name).map_err(
            |error| {
                SQLError::Internal(format!(
                    "detach legacy sequence owner metadata for `{name}`: {error}"
                ))
            },
        )?;
    }
    Ok(true)
}
